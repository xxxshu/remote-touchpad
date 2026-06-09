use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::ConnectInfo;
use axum::extract::State as AxumState;
use axum::http::HeaderMap;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpListener;
use tokio::sync::{broadcast, mpsc, Mutex, oneshot};
use serde_json;
use anyhow::Result;
use tracing::{info, warn};

use crate::input::InputSimulator;
use crate::protocol::{ClientMsg, ServerMsg};

/// Frontend files embedded in the binary at compile time
#[derive(Clone)]
struct EmbeddedFile {
    content: &'static [u8],
    mime: &'static str,
}

fn embedded_frontend() -> HashMap<&'static str, EmbeddedFile> {
    let mut m: HashMap<&'static str, EmbeddedFile> = HashMap::new();
    m.insert("", EmbeddedFile { content: include_bytes!("../../frontend/index.html"), mime: "text/html; charset=utf-8" });
    m.insert("index.html", EmbeddedFile { content: include_bytes!("../../frontend/index.html"), mime: "text/html; charset=utf-8" });
    m.insert("style.css", EmbeddedFile { content: include_bytes!("../../frontend/style.css"), mime: "text/css; charset=utf-8" });
    m.insert("app.js", EmbeddedFile { content: include_bytes!("../../frontend/app.js"), mime: "application/javascript; charset=utf-8" });
    m.insert("iconfont/iconfont.js", EmbeddedFile { content: include_bytes!("../../frontend/iconfont/iconfont.js"), mime: "application/javascript; charset=utf-8" });
    m
}

/// Shared server state
pub struct ServerState {
    pub input: Arc<Mutex<InputSimulator>>,
    /// Active controller: (addr, sender_to_ws)
    pub active_ws: Arc<Mutex<Option<(SocketAddr, mpsc::UnboundedSender<Message>)>>>,
    /// Connected device name (from User-Agent)
    pub connected_device: Arc<Mutex<Option<String>>>,
    /// Pending controller waiting for approval
    pub pending_ws: Arc<Mutex<Option<SocketAddr>>>,
    /// Channel to send approval response from active → pending handler
    pub approval_tx: Arc<Mutex<Option<oneshot::Sender<String>>>>,
    /// PIN code for authentication (wrapped in Mutex for interior mutability)
    pub pin: Mutex<String>,
    pub event_tx: broadcast::Sender<String>,
    pub frontend_dir: PathBuf,
}

/// Generate a random 6-digit PIN
fn generate_pin() -> String {
    use rand::Rng;
    let pin = rand::rng().random_range(100000u32..=999999);
    format!("{}", pin)
}

impl ServerState {
    pub fn new(input: InputSimulator, frontend_dir: PathBuf) -> Self {
        let (event_tx, _) = broadcast::channel(100);
        Self {
            input: Arc::new(Mutex::new(input)),
            active_ws: Arc::new(Mutex::new(None)),
            connected_device: Arc::new(Mutex::new(None)),
            pending_ws: Arc::new(Mutex::new(None)),
            approval_tx: Arc::new(Mutex::new(None)),
            pin: Mutex::new(generate_pin()),
            event_tx,
            frontend_dir,
        }
    }

    fn send_event(&self, msg: String) {
        let _ = self.event_tx.send(msg);
    }

    /// Send a message to the active controller's WebSocket
    async fn send_to_active(&self, msg: &str) -> bool {
        let active = self.active_ws.lock().await;
        if let Some((_, ref tx)) = *active {
            tx.send(Message::Text(msg.into())).is_ok()
        } else {
            false
        }
    }
}

/// Get LAN IP address (prefer WiFi/Ethernet over virtual adapters).
///
/// 1. Enumerate interfaces via `local_ip_address` (2-second timeout to guard
///    against `getifaddrs()` stalling on ARM64 / Android / containers).
///    Prefers 192.168.x.x / 10.x.x.x — IPs the phone can actually reach.
/// 2. Read `/proc/net/route` to find the default-route interface, then bind a
///    UDP socket to that interface (`SO_BINDTODEVICE`) to get its IP.
/// 3. UDP connect trick (fast, but may return VPN/virtual adapter IPs).
/// 4. "127.0.0.1" as last resort.
pub fn get_local_ip() -> String {
    // 1) Interface scan with timeout — finds real LAN addresses.
    let (tx, rx) = std::sync::mpsc::channel();
    let _ = std::thread::Builder::new()
        .name("local-ip-scan".into())
        .spawn(move || {
            let result = (|| -> Option<String> {
                let addrs = local_ip_address::list_afinet_netifas().ok()?;
                let mut best = None;
                for (_name, ip) in &addrs {
                    if let std::net::IpAddr::V4(v4) = ip {
                        let s = v4.to_string();
                        if s.starts_with("127.") || s.starts_with("169.254.") {
                            continue;
                        }
                        // Prefer addresses the phone is most likely to reach
                        if s.starts_with("192.168.") || s.starts_with("10.") {
                            return Some(s);
                        }
                        if best.is_none() {
                            best = Some(s);
                        }
                    }
                }
                best
            })();
            let _ = tx.send(result);
        });

    if let Ok(Some(ip)) = rx.recv_timeout(std::time::Duration::from_secs(2)) {
        tracing::info!("get_local_ip via interface scan: {}", ip);
        return ip;
    }
    tracing::warn!("get_local_ip: interface scan timed out (2s), trying /proc/net/route");

    // 2) /proc/net/route fallback: find the default-route interface, then use
    //    SO_BINDTODEVICE + UDP connect to discover that interface's IP.
    //    This avoids the Netlink socket that getifaddrs() uses (which hangs on
    //    some ARM64 / Android kernels).
    if let Some(ip) = proc_route_local_ip() {
        return ip;
    }

    // 3) UDP connect trick — fast, never blocks, but may pick a VPN / virtual
    //    adapter address that the phone cannot reach.
    if let Some(ip) = udp_local_ip() {
        if !ip.starts_with("127.") {
            tracing::warn!("get_local_ip via UDP fallback: {} (may be VPN/virtual)", ip);
            return ip;
        }
    }

    // 4) Last resort
    "127.0.0.1".to_string()
}

/// Read `/proc/net/route` to find the default-route network interface, then
/// use `ip addr show <iface>` to discover its IPv4 address.
///
/// Returns `None` on non-Linux systems or if parsing fails.
fn proc_route_local_ip() -> Option<String> {
    // Step 1: find the default-route interface name from /proc/net/route
    let contents = std::fs::read_to_string("/proc/net/route").ok()?;
    let iface = contents
        .lines()
        .skip(1) // header
        .find(|line| {
            let cols: Vec<&str> = line.split_whitespace().collect();
            // Destination == 0.0.0.0 (hex 00000000) = default route
            cols.get(1).map_or(false, |d| *d == "00000000")
        })
        .and_then(|line| line.split_whitespace().next())?;
    tracing::debug!("default route interface: {}", iface);

    // Step 2: query that interface's IP via `ip addr show <iface>`
    let output = std::process::Command::new("ip")
        .args(["addr", "show", iface])
        .output()
        .ok()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("inet ") {
            if let Some(cidr) = rest.split_whitespace().next() {
                if let Some(ip) = cidr.split('/').next() {
                    let ip = ip.to_string();
                    if !ip.starts_with("127.") {
                        tracing::info!("get_local_ip via proc_route+ip: {}", ip);
                        return Some(ip);
                    }
                }
            }
        }
    }
    None
}

/// Fast local IP detection via UDP socket connect (no traffic sent).
fn udp_local_ip() -> Option<String> {
    use std::net::UdpSocket;
    let sock = UdpSocket::bind("0.0.0.0:0").ok()?;
    sock.connect("8.8.8.8:80").ok()?;
    sock.local_addr().ok().map(|a| a.ip().to_string())
}

/// Generate QR code as SVG
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
    let embedded = embedded_frontend();

    let app = Router::new()
        .route("/ws", get(ws_handler))
        .fallback(move |req: axum::http::Request<axum::body::Body>| {
            let frontend = frontend.clone();
            let embedded = embedded.clone();
            async move {
                let path = req.uri().path().trim_start_matches('/');
                let path = if path.is_empty() { "index.html" } else { path };

                // Try embedded files first (works in packaged app)
                if let Some(file) = embedded.get(path) {
                    return axum::response::Response::builder()
                        .header("content-type", file.mime)
                        .body(axum::body::Body::from(file.content))
                        .unwrap();
                }

                // Fallback to filesystem (dev mode)
                let file_path = frontend.join(path);
                let file_path = match file_path.canonicalize() {
                    Ok(p) => p,
                    Err(_) => {
                        return axum::response::Response::builder()
                            .status(404)
                            .body(axum::body::Body::from("Not found"))
                            .unwrap();
                    }
                };
                if !file_path.starts_with(frontend.canonicalize().as_deref().unwrap_or(&frontend)) {
                    return axum::response::Response::builder()
                        .status(403)
                        .body(axum::body::Body::from("Forbidden"))
                        .unwrap();
                }

                match tokio::fs::read(&file_path).await {
                    Ok(data) => {
                        let mime = match file_path.extension().and_then(|e| e.to_str()) {
                            Some("html") => "text/html; charset=utf-8",
                            Some("css") => "text/css; charset=utf-8",
                            Some("js") => "application/javascript; charset=utf-8",
                            Some("png") => "image/png",
                            Some("svg") => "image/svg+xml",
                            Some("json") => "application/json",
                            _ => "application/octet-stream",
                        };
                        axum::response::Response::builder()
                            .header("content-type", mime)
                            .body(axum::body::Body::from(data))
                            .unwrap()
                    }
                    Err(_) => axum::response::Response::builder()
                        .status(404)
                        .body(axum::body::Body::from("Not found"))
                        .unwrap(),
                }
            }
        })
        .with_state(state.clone());

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = TcpListener::bind(addr).await?;
    info!("Server listening on {}", addr);

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
    headers: HeaderMap,
    AxumState(state): AxumState<Arc<ServerState>>,
) -> impl IntoResponse {
    let device_name = parse_device_name(&headers);
    ws.on_upgrade(move |socket| handle_ws(socket, state, addr, device_name))
}

/// Extract a friendly device name from User-Agent
fn parse_device_name(headers: &HeaderMap) -> String {
    let ua = headers.get("user-agent")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("Unknown Device");

    if ua.contains("iPhone") { "iPhone".to_string() }
    else if ua.contains("iPad") { "iPad".to_string() }
    else if ua.contains("Android") { "Android".to_string() }
    else if ua.contains("Windows") { "Windows".to_string() }
    else if ua.contains("Macintosh") || ua.contains("Mac OS") { "Mac".to_string() }
    else if ua.contains("Linux") { "Linux".to_string() }
    else { "设备".to_string() }
}

/// Handle a single WebSocket connection
async fn handle_ws(socket: WebSocket, state: Arc<ServerState>, addr: SocketAddr, device_name: String) {
    let addr_str = format!("{}", addr);
    info!("Client connected: {} ({})", addr_str, device_name);

    // Split socket into sink (for sending) and stream (for receiving)
    let (mut ws_sink, mut ws_stream) = socket.split();

    // Create an unbounded channel for sending messages to this WebSocket
    let (tx, mut rx) = mpsc::unbounded_channel::<Message>();

    // Task: forward channel messages → WebSocket
    let forward_task = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if ws_sink.send(msg).await.is_err() {
                break;
            }
        }
    });

    // Step 1: Require PIN authentication
    let auth_msg = serde_json::to_string(&ServerMsg::AuthRequired).unwrap();
    let _ = tx.send(Message::Text(auth_msg.into()));

    let authenticated = loop {
        match tokio::time::timeout(
            tokio::time::Duration::from_secs(60),
            ws_stream.next(),
        ).await {
            Ok(Some(Ok(Message::Text(text)))) => {
                let text_str: &str = text.as_ref();
                if let Ok(ClientMsg::Auth { pin }) = serde_json::from_str(text_str) {
                    let current_pin = state.pin.lock().await.clone();
                    if pin == current_pin {
                        info!("{} authenticated", addr_str);
                        // Refresh PIN for next device
                        *state.pin.lock().await = generate_pin();
                        break true;
                    } else {
                        warn!("{} auth failed (wrong PIN)", addr_str);
                        let fail = serde_json::to_string(&ServerMsg::AuthFail).unwrap();
                        let _ = tx.send(Message::Text(fail.into()));
                    }
                }
            }
            _ => break false,
        }
    };

    if !authenticated {
        let _ = tx.send(Message::Close(Some(axum::extract::ws::CloseFrame {
            code: 4003,
            reason: "auth failed".into(),
        })));
        forward_task.abort();
        return;
    }

    // Check if there's already an active controller
    let has_active = state.active_ws.lock().await.is_some();

    if !has_active {
        // No active controller → take control immediately
        *state.active_ws.lock().await = Some((addr, tx.clone()));
        *state.connected_device.lock().await = Some(device_name.clone());
        let msg = serde_json::to_string(&ServerMsg::CtrlOk).unwrap();
        let _ = tx.send(Message::Text(msg.into()));
        state.send_event(format!("✅ {} ({}) 已连接", device_name, addr_str));
        info!("{} is now controller", addr_str);
    } else {
        // Active controller exists → need approval
        let has_pending = state.pending_ws.lock().await.is_some();
        if has_pending {
            let msg = serde_json::to_string(&ServerMsg::Wait {
                reason: Some("busy".into())
            }).unwrap();
            let _ = tx.send(Message::Text(msg.into()));
            let _ = tx.send(Message::Close(Some(axum::extract::ws::CloseFrame {
                code: 4002,
                reason: "busy".into(),
            })));
            forward_task.abort();
            return;
        }

        *state.pending_ws.lock().await = Some(addr);

        let (approval_tx, approval_rx) = oneshot::channel::<String>();
        *state.approval_tx.lock().await = Some(approval_tx);

        // Send approval request DIRECTLY to the active controller's WebSocket
        let req_msg = serde_json::json!({"a": "approval_req", "ip": addr_str}).to_string();
        state.send_to_active(&req_msg).await;

        // Notify new client they're waiting
        let msg = serde_json::to_string(&ServerMsg::Wait { reason: None }).unwrap();
        let _ = tx.send(Message::Text(msg.into()));
        state.send_event(format!("⏳ {} 等待审批", addr_str));

        // Wait for approval with timeout
        let result = tokio::time::timeout(
            tokio::time::Duration::from_secs(30),
            approval_rx,
        ).await;

        match result {
            Ok(Ok(response)) if response == "accept" => {
                // Kick old controller
                let kick = serde_json::json!({"a": "wait", "reason": "kicked"}).to_string();
                state.send_to_active(&kick).await;
                // Close old controller's WebSocket
                if let Some((_, old_tx)) = state.active_ws.lock().await.take() {
                    let _ = old_tx.send(Message::Close(Some(axum::extract::ws::CloseFrame {
                        code: 4001,
                        reason: "new controller".into(),
                    })));
                }

                // Promote pending to active
                *state.active_ws.lock().await = Some((addr, tx.clone()));
                *state.connected_device.lock().await = Some(device_name.clone());
                *state.pending_ws.lock().await = None;
                *state.approval_tx.lock().await = None;

                let msg = serde_json::to_string(&ServerMsg::CtrlOk).unwrap();
                let _ = tx.send(Message::Text(msg.into()));
                state.send_event(format!("✅ {} ({}) 已接管控制", device_name, addr_str));
                info!("{} approved, now controller", addr_str);
            }
            _ => {
                let reason = match result {
                    Ok(Ok(r)) if r == "reject" => "rejected".to_string(),
                    Ok(Ok(r)) => r,
                    _ => "timeout".to_string(),
                };
                // Send rejection message while socket is still open
                let msg = serde_json::to_string(&ServerMsg::Wait {
                    reason: Some(reason.clone())
                }).unwrap();
                let _ = tx.send(Message::Text(msg.into()));
                // Give frontend time to process the message before closing
                tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
                let _ = tx.send(Message::Close(Some(axum::extract::ws::CloseFrame {
                    code: 4002,
                    reason: reason.clone().into(),
                })));

                *state.pending_ws.lock().await = None;
                *state.approval_tx.lock().await = None;
                state.send_event(format!("🚫 {} {}", addr_str,
                    if reason == "timeout" { "等待超时" } else { "被拒绝" }));
                forward_task.abort();
                return;
            }
        }
    }

    // Message loop for the active controller
    while let Some(Ok(msg)) = ws_stream.next().await {
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
            Message::Close(_) => break,
            _ => {}
        }
    }

    // Cleanup on disconnect
    let mut active = state.active_ws.lock().await;
    if active.as_ref().map(|(a, _)| *a) == Some(addr) {
        *active = None;
        *state.connected_device.lock().await = None;
    }
    drop(active);

    let mut pending = state.pending_ws.lock().await;
    if *pending == Some(addr) {
        *pending = None;
        let mut tx_lock = state.approval_tx.lock().await;
        if let Some(approval_sender) = tx_lock.take() {
            let _ = approval_sender.send("timeout".to_string());
        }
    }

    state.send_event(format!("❌ {} 已断开", addr_str));
    info!("Client disconnected: {}", addr_str);
    forward_task.abort();
}

/// Handle a parsed client message
async fn handle_client_msg(msg: ClientMsg, state: &Arc<ServerState>, addr: &str) {
    let input = state.input.lock().await;

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
            for _ in 0..n {
                if let Err(e) = input.send_key("Backspace").await {
                    warn!("Backspace error: {}", e);
                    break;
                }
            }
            return;
        }
        ClientMsg::ApprovalResp { r } => {
            drop(input);
            let mut tx_lock = state.approval_tx.lock().await;
            if let Some(tx) = tx_lock.take() {
                let _ = tx.send(r.clone());
                info!("Approval response: {}", r);
            }
            return;
        }
        ClientMsg::Auth { .. } => {
            // Auth handled in connection setup, ignore here
            return;
        }
    };

    if let Err(e) = result {
        warn!("Input error from {}: {}", addr, e);
    }
}
