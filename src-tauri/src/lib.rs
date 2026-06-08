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
}

// ─── Tauri Commands ────────────────────────────────────────

#[tauri::command]
async fn get_status(state: State<'_, Mutex<AppState>>) -> Result<ServerStatus, String> {
    let app = state.lock().await;
    let ip = server::get_local_ip();
    let url = format!("http://{}:{}", ip, app.port);
    let qr_svg = server::generate_qr_svg(&url);

    Ok(ServerStatus {
        running: app.running,
        ip,
        port: app.port,
        url,
        qr_svg,
        events: Vec::new(),
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

    // Check xdotool
    if !std::process::Command::new("which")
        .arg("xdotool")
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
    {
        return Err("xdotool not found. Install: apt install xdotool".into());
    }

    let input_sim = input::InputSimulator::new().await
        .map_err(|e| e.to_string())?;

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
    })
}

#[tauri::command]
async fn stop_server_cmd(state: State<'_, Mutex<AppState>>) -> Result<(), String> {
    let mut app = state.lock().await;
    if !app.running {
        return Err("Server not running".into());
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
