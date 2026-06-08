use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::ConnectInfo;
use axum::extract::State as AxumState;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;
use futures_util::StreamExt;
use tokio::net::TcpListener;
use tokio::sync::{broadcast, Mutex, oneshot};
use serde_json;
use anyhow::Result;
use tracing::{info, warn};

use crate::input::InputSimulator;
use crate::protocol::{ClientMsg, ServerMsg};

/// Shared server state
pub struct ServerState {
    pub input: Arc<Mutex<InputSimulator>>,
    pub active_ws: Arc<Mutex<Option<SocketAddr>>>,
    pub pending_ws: Arc<Mutex<Option<SocketAddr>>>,
    pub approval_tx: Arc<Mutex<Option<oneshot::Sender<String>>>>,
    pub event_tx: broadcast::Sender<String>,
    pub frontend_dir: PathBuf,
}

impl ServerState {
    pub fn new(input: InputSimulator, frontend_dir: PathBuf) -> Self {
        let (event_tx, _) = broadcast::channel(100);
        Self {
            input: Arc::new(Mutex::new(input)),
            active_ws: Arc::new(Mutex::new(None)),
            pending_ws: Arc::new(Mutex::new(None)),
            approval_tx: Arc::new(Mutex::new(None)),
            event_tx,
            frontend_dir,
        }
    }

    fn send_event(&self, msg: String) {
        let _ = self.event_tx.send(msg);
    }
}

/// Get LAN IP address
pub fn get_local_ip() -> String {
    if let Ok(ip) = local_ip_address::local_ip() {
        return ip.to_string();
    }
    "127.0.0.1".to_string()
}

/// Generate QR code as text (for terminal) or SVG (for GUI)
pub fn generate_qr_svg(url: &str) -> String {
    use qrcode::QrCode;
    use qrcode::render::svg;

    let code = QrCode::new(url.as_bytes()).unwrap();
    code.render::<svg::Color>()
        .min_dimensions(200, 200)
        .build()
}

/// Start the HTTP + WebSocket server
pub async fn start_server(
    port: u16,
    state: Arc<ServerState>,
    mut stop_rx: broadcast::Receiver<()>,
) -> Result<()> {
    let frontend = state.frontend_dir.clone();

    // Build HTTP response for the frontend page
    let index_html = std::fs::read_to_string(frontend.join("index.html"))
        .unwrap_or_else(|_| "<h1>Frontend not found</h1>".to_string());

    let app = Router::new()
        .route("/ws", get(ws_handler))
        .route("/", get(move || {
            let html = index_html.clone();
            async move {
                axum::response::Html(html)
            }
        }))
        .with_state(state.clone());

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = TcpListener::bind(addr).await?;
    info!("Server listening on {}", addr);

    // Serve with graceful shutdown
    axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
        .with_graceful_shutdown(async move {
            let _ = stop_rx.recv().await;
            info!("Server shutting down");
        })
        .await?;

    Ok(())
}

/// WebSocket upgrade handler
async fn ws_handler(
    ws: WebSocketUpgrade,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    AxumState(state): AxumState<Arc<ServerState>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_ws(socket, state, addr))
}

/// Handle a single WebSocket connection
async fn handle_ws(mut socket: WebSocket, state: Arc<ServerState>, addr: SocketAddr) {
    let addr_str = format!("{}", addr);
    info!("Client connected: {}", addr_str);

    // Check if there's already an active controller
    let has_active = state.active_ws.lock().await.is_some();

    if !has_active {
        // No active controller → take control immediately
        *state.active_ws.lock().await = Some(addr);
        let msg = serde_json::to_string(&ServerMsg::CtrlOk).unwrap();
        let _ = socket.send(Message::Text(msg.into())).await;
        state.send_event(format!("✅ {} 已连接", addr_str));
        info!("{} is now controller", addr_str);
    } else {
        // Active controller exists → need approval
        let has_pending = state.pending_ws.lock().await.is_some();
        if has_pending {
            // Already someone waiting → reject
            let msg = serde_json::to_string(&ServerMsg::Wait {
                reason: Some("busy".into())
            }).unwrap();
            let _ = socket.send(Message::Text(msg.into())).await;
            let _ = socket.send(Message::Close(Some(axum::extract::ws::CloseFrame {
                code: 4002,
                reason: "busy".into(),
            }))).await;
            return;
        }

        // Set as pending
        *state.pending_ws.lock().await = Some(addr);

        // Create approval channel
        let (tx, rx) = oneshot::channel::<String>();
        *state.approval_tx.lock().await = Some(tx);

        // Notify active controller
        if let Some(_active_addr) = *state.active_ws.lock().await {
            // We need to send via broadcast to the active controller
            // For simplicity, we'll use the event system
            state.send_event(format!("approval_req:{}", addr_str));
        }

        // Notify new client they're waiting
        let msg = serde_json::to_string(&ServerMsg::Wait { reason: None }).unwrap();
        let _ = socket.send(Message::Text(msg.into())).await;
        state.send_event(format!("⏳ {} 等待审批", addr_str));

        // Wait for approval with timeout
        let result = tokio::time::timeout(
            tokio::time::Duration::from_secs(30),
            rx,
        ).await;

        match result {
            Ok(Ok(response)) if response == "accept" => {
                // Disconnect old controller (via broadcast)
                state.send_event("kick_active:4001".to_string());

                // Promote pending to active
                *state.active_ws.lock().await = Some(addr);
                *state.pending_ws.lock().await = None;
                *state.approval_tx.lock().await = None;

                let msg = serde_json::to_string(&ServerMsg::CtrlOk).unwrap();
                let _ = socket.send(Message::Text(msg.into())).await;
                state.send_event(format!("✅ {} 已接管控制", addr_str));
                info!("{} approved, now controller", addr_str);
            }
            _ => {
                // Timeout or reject
                let reason = match result {
                    Ok(Ok(r)) => r, // "reject"
                    _ => "timeout".to_string(),
                };
                let msg = serde_json::to_string(&ServerMsg::Wait {
                    reason: Some(reason.clone())
                }).unwrap();
                let _ = socket.send(Message::Text(msg.into())).await;
                let _ = socket.send(Message::Close(Some(axum::extract::ws::CloseFrame {
                    code: 4002,
                    reason: reason.clone().into(),
                }))).await;

                *state.pending_ws.lock().await = None;
                *state.approval_tx.lock().await = None;
                state.send_event(format!("🚫 {} {}", addr_str,
                    if reason == "timeout" { "等待超时" } else { "被拒绝" }));
                return;
            }
        }
    }

    // Message loop for the active controller
    while let Some(Ok(msg)) = socket.next().await {
        match msg {
            Message::Text(text) => {
                let text_str: &str = text.as_ref();
                match serde_json::from_str::<ClientMsg>(text_str) {
                    Ok(client_msg) => {
                        handle_client_msg(client_msg, &state, &addr_str).await;
                    }
                    Err(e) => {
                        warn!("Invalid message from {}: {}", addr_str, e);
                    }
                }
            }
            Message::Close(_) => {
                break;
            }
            _ => {}
        }
    }

    // Cleanup on disconnect
    let mut active = state.active_ws.lock().await;
    if *active == Some(addr) {
        *active = None;
    }
    drop(active);

    let mut pending = state.pending_ws.lock().await;
    if *pending == Some(addr) {
        *pending = None;
        // Resolve pending approval as timeout
        let mut tx_lock = state.approval_tx.lock().await;
        if let Some(tx) = tx_lock.take() {
            let _ = tx.send("timeout".to_string());
        }
    }

    state.send_event(format!("❌ {} 已断开", addr_str));
    info!("Client disconnected: {}", addr_str);
}

/// Handle a parsed client message
async fn handle_client_msg(msg: ClientMsg, state: &Arc<ServerState>, addr: &str) {
    let mut input = state.input.lock().await;

    let result = match msg {
        ClientMsg::Move { x, y } => input.mouse_move(x, y).await,
        ClientMsg::Click { b } => input.mouse_click(b).await,
        ClientMsg::DoubleClick => input.mouse_double_click().await,
        ClientMsg::MouseDown { b } => input.mouse_down(b).await,
        ClientMsg::MouseUp { b } => input.mouse_up(b).await,
        ClientMsg::Scroll { y } => input.mouse_scroll(y).await,
        ClientMsg::TypeText { t } => input.type_text(&t).await,
        ClientMsg::Key { k } => input.send_key(&k).await,
        ClientMsg::Backspace { n } => {
            input.send_key(&format!("--repeat {} BackSpace", n)).await
        }
        ClientMsg::ApprovalResp { r } => {
            // Forward approval response
            drop(input);
            let mut tx_lock = state.approval_tx.lock().await;
            if let Some(tx) = tx_lock.take() {
                let _ = tx.send(r.clone());
                info!("Approval response: {}", r);
            }
            return;
        }
    };

    if let Err(e) = result {
        warn!("Input error from {}: {}", addr, e);
    }
}
