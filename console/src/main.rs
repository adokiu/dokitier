use std::{path::PathBuf, time::{SystemTime, UNIX_EPOCH}};

use axum::{
    extract::{ws::{Message, WebSocket, WebSocketUpgrade}, Path, State},
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use dokitier_controller::{hash_token, read_frame, write_frame, AgentFrame};
use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::{sqlite::SqlitePoolOptions, SqlitePool};
use tokio::net::TcpListener;
use tower_http::services::ServeDir;

#[derive(Clone)]
struct AppState {
    db: SqlitePool,
    webui: PathBuf,
    agents: std::sync::Arc<dashmap::DashMap<String, tokio::sync::mpsc::Sender<Value>>>,
    pending: std::sync::Arc<dashmap::DashMap<String, tokio::sync::oneshot::Sender<Value>>>,
}

fn now() -> i64 { SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() as i64 }

async fn init_db(db: &SqlitePool) -> anyhow::Result<()> {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS nodes (
            node_id TEXT PRIMARY KEY,
            token_hash TEXT NOT NULL,
            hostname TEXT NOT NULL DEFAULT '',
            version TEXT NOT NULL DEFAULT '',
            networks TEXT NOT NULL DEFAULT '[]',
            snapshot TEXT NOT NULL DEFAULT 'null',
            last_seen INTEGER NOT NULL DEFAULT 0,
            status TEXT NOT NULL DEFAULT 'offline'
        )",
    ).execute(db).await?;
    Ok(())
}

async fn verify_node(db: &SqlitePool, node_id: &str, token: &str) -> bool {
    let row: Option<(String,)> = sqlx::query_as("SELECT token_hash FROM nodes WHERE node_id = ?")
        .bind(node_id).fetch_optional(db).await.ok().flatten();
    row.is_some_and(|(hash,)| hash == hash_token(token))
}

async fn touch_node(db: &SqlitePool, node_id: &str, hostname: Option<&str>, version: Option<&str>,
                    networks: Option<&Value>, snapshot: Option<&Value>, status: &str) {
    let _ = sqlx::query(
        "UPDATE nodes SET last_seen = ?, status = ?,
            hostname = COALESCE(?, hostname), version = COALESCE(?, version),
            networks = COALESCE(?, networks), snapshot = COALESCE(?, snapshot)
         WHERE node_id = ?",
    ).bind(now()).bind(status).bind(hostname).bind(version)
     .bind(networks.map(|v| v.to_string())).bind(snapshot.map(|v| v.to_string()))
     .bind(node_id).execute(db).await;
}

async fn handle_agent_frame(state: &AppState, frame: AgentFrame) -> Result<(), String> {
    match frame {
        AgentFrame::Hello { node_id, token, hostname, version, networks } => {
            if !verify_node(&state.db, &node_id, &token).await {
                return Err("unknown node or invalid token".into());
            }
            touch_node(&state.db, &node_id, Some(&hostname), Some(&version),
                       Some(&Value::Array(networks)), None, "online").await;
            Ok(())
        }
        AgentFrame::Heartbeat { node_id, .. } => {
            touch_node(&state.db, &node_id, None, None, None, None, "online").await; Ok(())
        }
        AgentFrame::Snapshot { node_id, data } => {
            let hostname = data.pointer("/instances/0/hostname").and_then(|v| v.as_str());
            let version = data.pointer("/instances/0/version").and_then(|v| v.as_str());
            touch_node(&state.db, &node_id, hostname, version, None, Some(&data), "online").await; Ok(())
        }
        AgentFrame::Result { node_id, .. } => {
            touch_node(&state.db, &node_id, None, None, None, None, "online").await; Ok(())
        }
    }
}

async fn ws_agent(State(state): State<AppState>, ws: WebSocketUpgrade) -> impl IntoResponse {
    ws.on_upgrade(move |socket| ws_session(state, socket))
}

async fn send_json(socket: &mut WebSocket, value: Value) {
    let _ = socket.send(Message::Text(value.to_string().into())).await;
}

async fn ws_session(state: AppState, mut socket: WebSocket) {
    let mut node_id: Option<String> = None;
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Value>(32);
    loop {
        tokio::select! {
            msg = socket.recv() => {
                let Some(Ok(msg)) = msg else { break };
                let text = match msg {
                    Message::Text(text) => text.to_string(),
                    Message::Binary(bin) => String::from_utf8_lossy(&bin).to_string(),
                    Message::Close(_) => break,
                    _ => continue,
                };
                if let Ok(value) = serde_json::from_str::<Value>(&text) {
                    dispatch_result(&state, &value).await;
                }
                let Ok(frame) = serde_json::from_str::<AgentFrame>(&text) else { continue };
                if let AgentFrame::Hello { node_id: id, token, .. } = &frame {
                    if !verify_node(&state.db, id, token).await {
                        let _ = socket.send(Message::Text(json!({"type":"error","error":"unknown node or invalid token"}).to_string().into())).await;
                        break;
                    }
                    node_id = Some(id.clone());
                    state.agents.insert(id.clone(), tx.clone());
                }
                let id_matches = node_id.is_some();
                if !id_matches { break; }
                let _ = handle_agent_frame(&state, frame).await;
            }
            outbound = rx.recv() => {
                let Some(value) = outbound else { break };
                send_json(&mut socket, value).await;
            }
        }
        // Dispatch command results to waiting HTTP handlers.
        if let Some(id) = &node_id {
            let _ = id;
        }
    }
    if let Some(id) = node_id {
        state.agents.remove(&id);
        touch_node(&state.db, &id, None, None, None, None, "offline").await;
    }
}

async fn dispatch_result(state: &AppState, value: &Value) {
    if value.get("type").and_then(|v| v.as_str()) != Some("result") {
        return;
    }
    let Some(request_id) = value.get("request_id").and_then(|v| v.as_str()) else { return };
    if let Some((_, tx)) = state.pending.remove(request_id) {
        let _ = tx.send(value.clone());
    }
}

#[derive(Serialize)]
struct NodeView { node_id: String, hostname: String, version: String, networks: Value, snapshot: Value, last_seen: i64, status: String }

async fn list_nodes(State(state): State<AppState>) -> Json<Value> {
    let rows: Vec<(String, String, String, String, String, i64, String)> = sqlx::query_as(
        "SELECT node_id, hostname, version, networks, snapshot, last_seen, status FROM nodes ORDER BY node_id",
    ).fetch_all(&state.db).await.unwrap_or_default();
    let nodes: Vec<NodeView> = rows.into_iter().map(|(node_id, hostname, version, networks, snapshot, last_seen, status)| NodeView {
        node_id, hostname, version,
        networks: serde_json::from_str(&networks).unwrap_or(Value::Null),
        snapshot: serde_json::from_str(&snapshot).unwrap_or(Value::Null),
        last_seen, status,
    }).collect();
    Json(json!({"nodes": nodes}))
}

async fn get_node(State(state): State<AppState>, Path(node_id): Path<String>) -> Json<Value> {
    let row: Option<(String, String, String, String, String, i64, String)> = sqlx::query_as(
        "SELECT node_id, hostname, version, networks, snapshot, last_seen, status FROM nodes WHERE node_id = ?",
    ).bind(&node_id).fetch_optional(&state.db).await.ok().flatten();
    match row {
        Some((node_id, hostname, version, networks, snapshot, last_seen, status)) => Json(json!({
            "node_id": node_id, "hostname": hostname, "version": version,
            "networks": serde_json::from_str::<Value>(&networks).unwrap_or(Value::Null),
            "snapshot": serde_json::from_str::<Value>(&snapshot).unwrap_or(Value::Null),
            "last_seen": last_seen, "status": status,
        })),
        None => Json(json!({"error": "not found"})),
    }
}

#[derive(serde::Deserialize)]
struct CommandBody {
    command: dokitier_controller::ConsoleCommand,
}

async fn post_command(
    State(state): State<AppState>,
    Path(node_id): Path<String>,
    Json(body): Json<CommandBody>,
) -> Json<Value> {
    let Some(tx) = state.agents.get(&node_id).map(|v| v.clone()) else {
        return Json(json!({"ok": false, "error": "node is offline"}));
    };
    let request_id = uuid::Uuid::new_v4().to_string();
    let (result_tx, result_rx) = tokio::sync::oneshot::channel();
    state.pending.insert(request_id.clone(), result_tx);
    let frame = json!({
        "type": "command",
        "request_id": request_id,
        "command": serde_json::to_value(&body.command).unwrap_or(Value::Null),
    });
    if tx.send(frame).await.is_err() {
        state.pending.remove(&request_id);
        return Json(json!({"ok": false, "error": "agent channel closed"}));
    }
    match tokio::time::timeout(std::time::Duration::from_secs(20), result_rx).await {
        Ok(Ok(value)) => Json(value),
        Ok(Err(_)) => Json(json!({"ok": false, "error": "agent result channel closed"})),
        Err(_) => {
            state.pending.remove(&request_id);
            Json(json!({"ok": false, "error": "agent command timeout"}))
        }
    }
}

async fn quic_accept_loop(state: AppState, endpoint: quinn::Endpoint) {
    while let Some(incoming) = endpoint.accept().await {
        let state = state.clone();
        tokio::spawn(async move {
            let Ok(conn) = incoming.await else { return };
            tracing::info!(remote = ?conn.remote_address(), "quic agent connected");
            let Ok((mut send, mut recv)) = conn.accept_bi().await else { tracing::warn!("quic stream open failed"); return };
            let Ok(hello) = read_frame(&mut recv).await else { return };
            let Ok(frame) = serde_json::from_value::<AgentFrame>(hello) else { return };
            let AgentFrame::Hello { node_id, token, hostname, version, networks } = &frame else { return };
            if !verify_node(&state.db, node_id, token).await {
                let _ = write_frame(&mut send, &json!({"type":"error","error":"unauthorized"})).await;
                return;
            }
            let (tx, mut rx) = tokio::sync::mpsc::channel::<Value>(32);
            state.agents.insert(node_id.clone(), tx);
            touch_node(&state.db, node_id, Some(hostname), Some(version), Some(&Value::Array(networks.clone())), None, "online").await;
            let writer = tokio::spawn(async move {
                while let Some(value) = rx.recv().await {
                    if write_frame(&mut send, &value).await.is_err() { break; }
                }
            });
            while let Ok(value) = read_frame(&mut recv).await {
                dispatch_result(&state, &value).await;
                if let Ok(frame) = serde_json::from_value::<AgentFrame>(value) {
                    let _ = handle_agent_frame(&state, frame).await;
                }
            }
            writer.abort();
            state.agents.remove(node_id);
            touch_node(&state.db, node_id, None, None, None, None, "offline").await;
        });
    }
}

async fn health() -> Json<Value> { Json(json!({"ok": true, "service": "dokitier-console"})) }

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().init();
    let mut listen = "127.0.0.1:12800".to_string();
    let mut agent_quic = "127.0.0.1:12803".to_string();
    let mut db_path = "board.db".to_string();
    let mut webui = "../dokitier-web/webui/dist".to_string();
    let mut add_nodes: Vec<(String, String)> = Vec::new();
    let args: Vec<String> = std::env::args().collect();
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--listen" => { listen = args[i + 1].clone(); i += 2; }
            "--agent-quic" => { agent_quic = args[i + 1].clone(); i += 2; }
            "--db" => { db_path = args[i + 1].clone(); i += 2; }
            "--webui" => { webui = args[i + 1].clone(); i += 2; }
            "--add-node" => { add_nodes.push((args[i + 1].clone(), args[i + 2].clone())); i += 3; }
            other => { eprintln!("unknown argument: {other}"); std::process::exit(2); }
        }
    }
    let db = SqlitePoolOptions::new().max_connections(5)
        .connect(&format!("sqlite://{db_path}?mode=rwc")).await?;
    init_db(&db).await?;
    for (node_id, token) in &add_nodes {
        sqlx::query("INSERT INTO nodes (node_id, token_hash) VALUES (?, ?) ON CONFLICT(node_id) DO UPDATE SET token_hash = excluded.token_hash")
            .bind(node_id).bind(hash_token(token)).execute(&db).await?;
        tracing::info!(%node_id, "registered node");
    }
    let state = AppState { db, webui: PathBuf::from(webui),
        agents: std::sync::Arc::new(dashmap::DashMap::new()),
        pending: std::sync::Arc::new(dashmap::DashMap::new()) };
    let _ = rustls::crypto::ring::default_provider().install_default();
    let certified = rcgen::generate_simple_self_signed(vec!["localhost".to_string()])?;
    let cert_der = rustls::pki_types::CertificateDer::from(certified.cert.der().to_vec());
    let key_der = rustls::pki_types::PrivateKeyDer::Pkcs8(
        rustls::pki_types::PrivatePkcs8KeyDer::from(certified.key_pair.serialize_der()),
    );
    let mut tls = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert_der], key_der)?;
    tls.alpn_protocols = vec![b"dokitier".to_vec()];
    let quic_cfg = quinn::ServerConfig::with_crypto(std::sync::Arc::new(
        quinn::crypto::rustls::QuicServerConfig::try_from(tls)?,
    ));
    let quic_endpoint = quinn::Endpoint::server(quic_cfg, agent_quic.parse()?)?;
    tracing::info!(%listen, %agent_quic, "console listeners ready (websocket + quic agent endpoints)");
    let quic_state = state.clone();
    tokio::spawn(async move { quic_accept_loop(quic_state, quic_endpoint).await });
    let app = Router::new()
        .route("/healthz", get(health))
        .route("/api/board/nodes", get(list_nodes))
        .route("/api/board/nodes/{node_id}", get(get_node))
        .route("/api/agent/ws", get(ws_agent))
        .route("/api/board/nodes/{node_id}/command", axum::routing::post(post_command))
        .fallback_service(ServeDir::new(&state.webui).append_index_html_on_directories(true))
        .with_state(state);
    let listener = TcpListener::bind(&listen).await?;
    tracing::info!("console listening on http://{listen}");
    axum::serve(listener, app).with_graceful_shutdown(async { let _ = tokio::signal::ctrl_c().await; }).await?;
    Ok(())
}
