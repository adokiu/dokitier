use std::{path::PathBuf, sync::atomic::{AtomicBool, Ordering}, time::{SystemTime, UNIX_EPOCH}};

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
use sqlx::{any::AnyPoolOptions, AnyPool};
use tokio::net::TcpListener;
use tower_http::services::ServeDir;

#[derive(Clone)]
struct AppState {
    db: AnyPool,
    postgres: bool,
    webui: PathBuf,
    agents: std::sync::Arc<dashmap::DashMap<String, tokio::sync::mpsc::Sender<Value>>>,
    pending: std::sync::Arc<dashmap::DashMap<String, tokio::sync::oneshot::Sender<Value>>>,
}

static POSTGRES: AtomicBool = AtomicBool::new(false);
static MYSQL: AtomicBool = AtomicBool::new(false);

fn is_mysql() -> bool { MYSQL.load(Ordering::Relaxed) }

fn q(sql: &str) -> String {
    sql_for(POSTGRES.load(Ordering::Relaxed), sql)
}

fn database_url(input: &str) -> String {
    if input.contains("://") {
        input.to_string()
    } else {
        format!("sqlite://{input}?mode=rwc")
    }
}

/// PostgreSQL uses $1 placeholders, SQLite/MySQL use ?.
fn sql_for(postgres: bool, sql: &str) -> String {
    if !postgres {
        return sql.to_string();
    }
    let mut out = String::with_capacity(sql.len());
    let mut n = 0;
    for ch in sql.chars() {
        if ch == '?' {
            n += 1;
            out.push('$');
            out.push_str(&n.to_string());
        } else {
            out.push(ch);
        }
    }
    out
}

fn now() -> i64 { SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() as i64 }

async fn init_db(db: &AnyPool) -> anyhow::Result<()> {
    let nodes_ddl = if is_mysql() {
        "CREATE TABLE IF NOT EXISTS nodes (
            node_id VARCHAR(128) PRIMARY KEY,
            token_hash VARCHAR(128) NOT NULL,
            hostname VARCHAR(255) NOT NULL DEFAULT '',
            version VARCHAR(64) NOT NULL DEFAULT '',
            networks LONGTEXT,
            snapshot LONGTEXT,
            last_seen BIGINT NOT NULL DEFAULT 0,
            status VARCHAR(32) NOT NULL DEFAULT 'offline'
        )"
    } else {
        "CREATE TABLE IF NOT EXISTS nodes (
            node_id TEXT PRIMARY KEY,
            token_hash TEXT NOT NULL,
            hostname TEXT NOT NULL DEFAULT '',
            version TEXT NOT NULL DEFAULT '',
            networks TEXT NOT NULL DEFAULT '[]',
            snapshot TEXT NOT NULL DEFAULT 'null',
            last_seen INTEGER NOT NULL DEFAULT 0,
            status TEXT NOT NULL DEFAULT 'offline'
        )"
    };
    sqlx::query(nodes_ddl).execute(db).await?;
    let versions_ddl = if is_mysql() {
        "CREATE TABLE IF NOT EXISTS config_versions (
            id BIGINT AUTO_INCREMENT PRIMARY KEY,
            node_id VARCHAR(128) NOT NULL,
            network VARCHAR(255) NOT NULL,
            hash VARCHAR(128) NOT NULL,
            toml LONGTEXT NOT NULL,
            created BIGINT NOT NULL
        )"
    } else if POSTGRES.load(Ordering::Relaxed) {
        "CREATE TABLE IF NOT EXISTS config_versions (
            id BIGSERIAL PRIMARY KEY,
            node_id TEXT NOT NULL,
            network TEXT NOT NULL,
            hash TEXT NOT NULL,
            toml TEXT NOT NULL,
            created BIGINT NOT NULL
        )"
    } else {
        "CREATE TABLE IF NOT EXISTS config_versions (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            node_id TEXT NOT NULL,
            network TEXT NOT NULL,
            hash TEXT NOT NULL,
            toml TEXT NOT NULL,
            created INTEGER NOT NULL
        )"
    };
    sqlx::query(versions_ddl).execute(db).await?;
    Ok(())
}

async fn verify_node(db: &AnyPool, node_id: &str, token: &str) -> bool {
    let row: Option<(String,)> = sqlx::query_as(&q("SELECT token_hash FROM nodes WHERE node_id = ?"))
        .bind(node_id).fetch_optional(db).await.ok().flatten();
    row.is_some_and(|(hash,)| hash == hash_token(token))
}

async fn touch_node(db: &AnyPool, node_id: &str, hostname: Option<&str>, version: Option<&str>,
                    networks: Option<&Value>, snapshot: Option<&Value>, status: &str) {
    let _ = sqlx::query(&q("UPDATE nodes SET last_seen = ?, status = ?,
            hostname = COALESCE(?, hostname), version = COALESCE(?, version),
            networks = COALESCE(?, networks), snapshot = COALESCE(?, snapshot)
         WHERE node_id = ?")).bind(now()).bind(status).bind(hostname).bind(version)
     .bind(networks.map(|v| v.to_string())).bind(snapshot.map(|v| v.to_string()))
     .bind(node_id).execute(db).await;
}

async fn merge_snapshot(state: &AppState, node_id: &str, data: &Value) -> Value {
    let existing: Option<(String,)> = sqlx::query_as(&q("SELECT snapshot FROM nodes WHERE node_id = ?"))
        .bind(node_id).fetch_optional(&state.db).await.ok().flatten();
    let mut merged_instances: Vec<Value> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut add = |instances: Option<&Vec<Value>>| {
        if let Some(list) = instances {
            for inst in list {
                let key = inst.get("instance_id").and_then(|v| v.as_str()).unwrap_or_default().to_string();
                if seen.insert(key) {
                    merged_instances.push(inst.clone());
                }
            }
        }
    };
    if let Some((snap,)) = existing {
        let old: Value = serde_json::from_str(&snap).unwrap_or(Value::Null);
        add(old.get("instances").and_then(|v| v.as_array()));
    }
    add(data.get("instances").and_then(|v| v.as_array()));
    json!({"ts": now(), "instances": merged_instances})
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
            let merged = merge_snapshot(state, &node_id, &data).await;
            let hostname = data.pointer("/instances/0/hostname").and_then(|v| v.as_str());
            let version = data.pointer("/instances/0/version").and_then(|v| v.as_str());
            touch_node(&state.db, &node_id, hostname, version, None, Some(&merged), "online").await; Ok(())
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

fn frame_network(frame: &AgentFrame) -> Option<String> {
    match frame {
        AgentFrame::Hello { networks, .. } => networks
            .first()
            .and_then(|n| n.get("name"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        _ => None,
    }
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
                    let network = frame_network(&frame).unwrap_or_else(|| "default".to_string());
                    let key = format!("{}:{}", id, network);
                    node_id = Some(id.clone());
                    state.agents.insert(key, tx.clone());
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
        let prefix = format!("{id}:");
        let keys: Vec<String> = state.agents.iter().filter(|e| e.key().starts_with(&prefix)).map(|e| e.key().clone()).collect();
        for key in keys { state.agents.remove(&key); }
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
    let rows: Vec<(String, String, String, String, String, i64, String)> = sqlx::query_as(&q(
        "SELECT node_id, hostname, version, networks, snapshot, last_seen, status FROM nodes ORDER BY node_id",
    )).fetch_all(&state.db).await.unwrap_or_default();
    let nodes: Vec<NodeView> = rows.into_iter().map(|(node_id, hostname, version, networks, snapshot, last_seen, status)| NodeView {
        node_id, hostname, version,
        networks: serde_json::from_str(&networks).unwrap_or(Value::Null),
        snapshot: serde_json::from_str(&snapshot).unwrap_or(Value::Null),
        last_seen, status,
    }).collect();
    Json(json!({"nodes": nodes}))
}

async fn get_node(State(state): State<AppState>, Path(node_id): Path<String>) -> Json<Value> {
    let row: Option<(String, String, String, String, String, i64, String)> = sqlx::query_as(&q(
        "SELECT node_id, hostname, version, networks, snapshot, last_seen, status FROM nodes WHERE node_id = ?",
    )).bind(&node_id).fetch_optional(&state.db).await.ok().flatten();
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
    #[serde(default)]
    network: Option<String>,
}

async fn post_command(
    State(state): State<AppState>,
    Path(node_id): Path<String>,
    Json(body): Json<CommandBody>,
) -> Json<Value> {
    let key = match &body.network {
        Some(network) => format!("{node_id}:{network}"),
        None => state
            .agents
            .iter()
            .find(|entry| entry.key().starts_with(&format!("{node_id}:")))
            .map(|entry| entry.key().clone())
            .unwrap_or_default(),
    };
    let Some(tx) = state.agents.get(&key).map(|v| v.clone()) else {
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
    let apply_toml = match &body.command {
        dokitier_controller::ConsoleCommand::ApplyConfig { toml } => Some(toml.clone()),
        _ => None,
    };
    match tokio::time::timeout(std::time::Duration::from_secs(20), result_rx).await {
        Ok(Ok(value)) => {
            if value.get("ok").and_then(|v| v.as_bool()) == Some(true) {
                if let Some(toml) = apply_toml {
                    let _ = sqlx::query(&q(
                        "INSERT INTO config_versions (node_id, network, hash, toml, created) VALUES (?, ?, ?, ?, ?)",
                    ))
                    .bind(&node_id)
                    .bind(body.network.clone().unwrap_or_default())
                    .bind(hash_token(&toml))
                    .bind(&toml)
                    .bind(now())
                    .execute(&state.db)
                    .await;
                }
            }
            Json(value)
        }
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
            let network = networks.first().and_then(|n| n.get("name")).and_then(|v| v.as_str()).unwrap_or("default");
            state.agents.insert(format!("{node_id}:{network}"), tx);
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
            let prefix = format!("{node_id}:");
            let keys: Vec<String> = state.agents.iter().filter(|e| e.key().starts_with(&prefix)).map(|e| e.key().clone()).collect();
            for key in keys { state.agents.remove(&key); }
            touch_node(&state.db, node_id, None, None, None, None, "offline").await;
        });
    }
}

async fn board_topology(State(state): State<AppState>) -> Json<Value> {
    let rows: Vec<(String, String)> = sqlx::query_as("SELECT node_id, snapshot FROM nodes")
        .fetch_all(&state.db)
        .await
        .unwrap_or_default();
    let mut nodes: std::collections::BTreeMap<u32, Value> = std::collections::BTreeMap::new();
    let mut edges: Vec<Value> = Vec::new();
    for (node_id, snapshot) in &rows {
        let Ok(value) = serde_json::from_str::<Value>(snapshot) else { continue };
        for inst in value.get("instances").and_then(|v| v.as_array()).into_iter().flatten() {
            let network = inst.get("network_name").and_then(|v| v.as_str()).unwrap_or_default();
            let Some(topo) = inst.get("topology") else { continue };
            for n in topo.get("nodes").and_then(|v| v.as_array()).into_iter().flatten() {
                if let Some(peer_id) = n.get("peer_id").and_then(|v| v.as_u64()) {
                    let mut node = n.clone();
                    node["network_name"] = json!(network);
                    node["agent_node_id"] = json!(node_id);
                    nodes.entry(peer_id as u32).or_insert(node);
                }
            }
            for e in topo.get("edges").and_then(|v| v.as_array()).into_iter().flatten() {
                let mut edge = e.clone();
                edge["network_name"] = json!(network);
                let tunnel_types: Vec<Value> = e.get("tunnels").and_then(|v| v.as_array()).into_iter().flatten()
                    .filter_map(|t| t.get("type").cloned()).collect();
                edge["tunnel_types"] = json!(tunnel_types);
                edges.push(edge);
            }
        }
    }
    Json(json!({"graph": {"nodes": nodes.values().cloned().collect::<Vec<_>>(), "edges": edges}}))
}

async fn board_overview(State(state): State<AppState>) -> Json<Value> {
    let rows: Vec<(String, String)> = sqlx::query_as("SELECT status, snapshot FROM nodes")
        .fetch_all(&state.db)
        .await
        .unwrap_or_default();
    let total = rows.len();
    let online = rows.iter().filter(|(status, _)| status == "online").count();
    let mut networks: std::collections::BTreeMap<String, Value> = std::collections::BTreeMap::new();
    for (_, snapshot) in &rows {
        let Ok(value) = serde_json::from_str::<Value>(snapshot) else { continue };
        for inst in value.get("instances").and_then(|v| v.as_array()).into_iter().flatten() {
            let name = inst.get("network_name").and_then(|v| v.as_str()).unwrap_or_default();
            if name.is_empty() { continue; }
            let entry = networks.entry(name.to_string()).or_insert_with(|| json!({
                "network_name": name, "nodes": 0, "p2p_groups": [],
            }));
            entry["nodes"] = json!(entry["nodes"].as_i64().unwrap_or(0) + 1);
            if let Some(group) = inst.get("p2p_group").and_then(|v| v.as_str()) {
                let groups = entry["p2p_groups"].as_array().cloned().unwrap_or_default();
                if !groups.iter().any(|g| g.as_str() == Some(group)) {
                    entry["p2p_groups"] = json!(groups.into_iter().chain(std::iter::once(json!(group))).collect::<Vec<_>>());
                }
            }
        }
    }
    Json(json!({
        "nodes": total,
        "online": online,
        "networks": networks.values().cloned().collect::<Vec<_>>(),
    }))
}

async fn overview(State(state): State<AppState>) -> Json<Value> {
    let count: (i64,) = sqlx::query_as(&q("SELECT COUNT(*) FROM nodes")).fetch_one(&state.db).await.unwrap_or((0,));
    Json(json!({"instances": count.0, "nodes": count.0, "links": 0, "routing": {}}))
}

async fn login() -> Json<Value> { Json(json!({"ok": true})) }
async fn logout() -> Json<Value> { Json(json!({"ok": true})) }

async fn list_versions(
    State(state): State<AppState>,
    Path(node_id): Path<String>,
) -> Json<Value> {
    let rows: Vec<(i64, String, String, i64)> = sqlx::query_as(&q(
        "SELECT id, network, hash, created FROM config_versions WHERE node_id = ? ORDER BY id DESC LIMIT 50",
    )).bind(&node_id).fetch_all(&state.db).await.unwrap_or_default();
    Json(json!({"versions": rows.into_iter().map(|(id, network, hash, created)| json!({
        "id": id, "network": network, "hash": hash, "created": created,
    })).collect::<Vec<_>>()}))
}

async fn get_version(
    State(state): State<AppState>,
    Path((node_id, version_id)): Path<(String, i64)>,
) -> Json<Value> {
    let row: Option<(String, String, String, i64)> = sqlx::query_as(&q(
        "SELECT network, hash, toml, created FROM config_versions WHERE node_id = ? AND id = ?",
    )).bind(&node_id).bind(version_id).fetch_optional(&state.db).await.ok().flatten();
    match row {
        Some((network, hash, toml, created)) => Json(json!({
            "network": network, "hash": hash, "toml": toml, "created": created,
        })),
        None => Json(json!({"error": "not found"})),
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
    sqlx::any::install_default_drivers();
    POSTGRES.store(db_path.starts_with("postgres://") || db_path.starts_with("postgresql://"), Ordering::Relaxed);
    MYSQL.store(db_path.starts_with("mysql://"), Ordering::Relaxed);
    let postgres = db_path.starts_with("postgres://") || db_path.starts_with("postgresql://");
    let db = AnyPoolOptions::new().max_connections(5)
        .connect(&format!("sqlite://{db_path}?mode=rwc")).await?;
    init_db(&db).await?;
    for (node_id, token) in &add_nodes {
        let upsert = if is_mysql() {
            "INSERT INTO nodes (node_id, token_hash) VALUES (?, ?) ON DUPLICATE KEY UPDATE token_hash = VALUES(token_hash)"
        } else {
            "INSERT INTO nodes (node_id, token_hash) VALUES (?, ?) ON CONFLICT(node_id) DO UPDATE SET token_hash = excluded.token_hash"
        };
        sqlx::query(upsert)
            .bind(node_id).bind(hash_token(token)).execute(&db).await?;
        tracing::info!(%node_id, "registered node");
    }
    let state = AppState { db, postgres, webui: PathBuf::from(webui),
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
        .route("/api/overview", get(overview))
        .route("/api/board/overview", get(board_overview))
        .route("/api/board/topology", get(board_topology))
        .route("/api/login", axum::routing::post(login))
        .route("/api/logout", axum::routing::post(logout))
        .route("/api/board/nodes", get(list_nodes))
        .route("/api/board/nodes/{node_id}", get(get_node))
        .route("/api/agent/ws", get(ws_agent))
        .route("/api/board/nodes/{node_id}/versions", get(list_versions))
        .route("/api/board/nodes/{node_id}/versions/{version_id}", get(get_version))
        .route("/api/board/nodes/{node_id}/command", axum::routing::post(post_command))
        .fallback_service(ServeDir::new(&state.webui).append_index_html_on_directories(true))
        .with_state(state);
    let listener = TcpListener::bind(&listen).await?;
    tracing::info!("console listening on http://{listen}");
    axum::serve(listener, app).with_graceful_shutdown(async { let _ = tokio::signal::ctrl_c().await; }).await?;
    Ok(())
}
