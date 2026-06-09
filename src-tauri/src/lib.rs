use std::path::PathBuf;
use std::sync::Arc;
use tauri::{Manager, State};
use tokio::sync::{broadcast, Mutex};
use serde::Serialize;

mod input;
mod protocol;
mod server;

use server::ServerState;

/// Tauri-managed app state
pub struct AppState {
    server_state: Option<Arc<ServerState>>,
    stop_tx: Option<broadcast::Sender<()>>,
    port: u16,
    running: bool,
}

#[derive(Serialize)]
pub struct ServerStatus {
    pub running: bool,
    pub ip: String,
    pub port: u16,
    pub url: String,
    pub qr_svg: String,
    pub events: Vec<String>,
    pub device_name: Option<String>,
    pub pin: Option<String>,
}

// ─── Tauri Commands ────────────────────────────────────────

#[tauri::command]
async fn get_status(state: State<'_, Mutex<AppState>>) -> Result<ServerStatus, String> {
    let app = state.lock().await;
    let ip = server::get_local_ip();
    let url = format!("http://{}:{}", ip, app.port);
    let qr_svg = server::generate_qr_svg(&url);

    let device_name = if let Some(ref ss) = app.server_state {
        ss.connected_device.lock().await.clone()
    } else {
        None
    };

    let pin = if let Some(ref ss) = app.server_state {
        Some(ss.pin.lock().await.clone())
    } else {
        None
    };

    Ok(ServerStatus {
        running: app.running,
        ip,
        port: app.port,
        url,
        qr_svg,
        events: Vec::new(),
        device_name,
        pin,
    })
}

#[tauri::command]
async fn start_server_cmd(
    port: u16,
    state: State<'_, Mutex<AppState>>,
    app_handle: tauri::AppHandle,
) -> Result<ServerStatus, String> {
    let mut app = state.lock().await;
    if app.running {
        return Err("Server already running".into());
    }

    let input_sim = match input::InputSimulator::new().await {
        Ok(sim) => sim,
        Err(e) => {
            tracing::warn!("InputSimulator init failed (will retry on first input): {}", e);
            input::InputSimulator::new_lazy()
        }
    };

    let frontend_dir: PathBuf = app_handle
        .path()
        .resource_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("frontend");

    // Fallback: check relative to binary
    let frontend_dir = if frontend_dir.join("index.html").exists() {
        frontend_dir
    } else {
        // Try relative to the executable
        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|p| p.to_path_buf()))
            .unwrap_or_default();
        // Try: exe_dir/frontend, exe_dir/../frontend, cwd/frontend, cwd/../frontend
        let candidates = [
            exe_dir.join("frontend"),
            exe_dir.join("..").join("frontend"),
            std::env::current_dir().unwrap_or_default().join("frontend"),
            std::env::current_dir().unwrap_or_default().join("..").join("frontend"),
        ];
        let found = candidates.iter().find(|c| c.join("index.html").exists());
        if let Some(dir) = found {
            dir.clone()
        } else {
            PathBuf::from("frontend")
        }
    };

    let server_state = Arc::new(ServerState::new(input_sim, frontend_dir));
    let (stop_tx, stop_rx) = broadcast::channel(1);

    let state_clone = server_state.clone();
    let port_clone = port;

    // Start server in background
    tokio::spawn(async move {
        if let Err(e) = server::start_server(port_clone, state_clone, stop_rx).await {
            tracing::error!("Server error: {}", e);
        }
    });

    let ip = server::get_local_ip();
    let url = format!("http://{}:{}", ip, port);
    let qr_svg = server::generate_qr_svg(&url);
    let pin = server_state.pin.lock().await.clone();

    app.server_state = Some(server_state);
    app.stop_tx = Some(stop_tx);
    app.port = port;
    app.running = true;

    Ok(ServerStatus {
        running: true,
        ip,
        port,
        url,
        qr_svg,
        events: Vec::new(),
        device_name: None,
        pin: Some(pin),
    })
}

#[tauri::command]
async fn stop_server_cmd(state: State<'_, Mutex<AppState>>) -> Result<(), String> {
    let mut app = state.lock().await;
    if !app.running {
        return Err("Server not running".into());
    }

    // Close active WebSocket connection first
    if let Some(ref ss) = app.server_state {
        if let Some((_, tx)) = ss.active_ws.lock().await.take() {
            let _ = tx.send(axum::extract::ws::Message::Close(Some(axum::extract::ws::CloseFrame {
                code: 1000,
                reason: "server stopped".into(),
            })));
        }
        *ss.connected_device.lock().await = None;
    }

    if let Some(tx) = app.stop_tx.take() {
        let _ = tx.send(());
    }

    // Close input simulator
    if let Some(server_state) = app.server_state.take() {
        server_state.input.lock().await.close().await;
    }

    app.running = false;
    Ok(())
}

// ─── Tauri App Setup ──────────────────────────────────────

pub fn run() {
    tracing_subscriber::fmt::init();

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .manage(Mutex::new(AppState {
            server_state: None,
            stop_tx: None,
            port: 8765,
            running: false,
        }))
        .invoke_handler(tauri::generate_handler![
            get_status,
            start_server_cmd,
            stop_server_cmd,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

/// CLI mode: start server without Tauri GUI
pub fn run_cli() {
    tracing_subscriber::fmt::init();
    let port: u16 = std::env::args()
        .collect::<Vec<_>>()
        .windows(2)
        .find(|w| w[0] == "--port")
        .and_then(|w| w[1].parse().ok())
        .unwrap_or(8765);

    let rt = tokio::runtime::Runtime::new().expect("Failed to create tokio runtime");
    rt.block_on(async {
        let input_sim = match input::InputSimulator::new().await {
            Ok(sim) => sim,
            Err(e) => {
                eprintln!("! InputSimulator init failed: {}", e);
                input::InputSimulator::new_lazy()
            }
        };

        let frontend_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|p| p.to_path_buf()))
            .unwrap_or_default();

        let server_state = Arc::new(server::ServerState::new(input_sim, frontend_dir));
        let pin = server_state.pin.lock().await.clone();
        let local_ip = server::get_local_ip();
        let url = format!("http://{}:{}", local_ip, port);

        eprintln!("========================================");
        eprintln!("  Remote Touchpad (CLI)");
        eprintln!("  URL: {}", url);
        eprintln!("  PIN: {}", pin);
        eprintln!("========================================");

        let (stop_tx, stop_rx) = tokio::sync::broadcast::channel(1);
        let state_clone = server_state.clone();

        tokio::spawn(async move {
            if let Err(e) = server::start_server(port, state_clone, stop_rx).await {
                eprintln!("! Server error: {}", e);
            }
        });

        eprintln!("Server started on port {}", port);
        eprintln!("Press Ctrl+C to stop...");

        // Wait for Ctrl+C
        let _ = tokio::signal::ctrl_c().await;

        eprintln!("Stopping server...");
        let _ = stop_tx.send(());
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
        eprintln!("Server stopped.");
    });
}
