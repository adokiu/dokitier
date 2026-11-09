use std::{
    convert::Infallible,
    pin::Pin,
    sync::atomic::{AtomicBool, Ordering},
    task::{Context, Poll},
    time::{SystemTime, UNIX_EPOCH},
};

use axum::{
    extract::{ws::{Message, WebSocket, WebSocketUpgrade}, Path, State},
    http::{header, HeaderValue, StatusCode, Uri},
    response::{sse::{Event, KeepAlive, Sse}, IntoResponse, Response},
    routing::get,
    Json, Router,
};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use futures::{Stream, StreamExt};
use dokitier_controller::{hash_token, proof, read_frame, write_frame, AgentFrame, ConsoleFrame, SessionCipher};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::{any::AnyPoolOptions, AnyPool, Row};
use tokio::net::TcpListener;
use tower_http::services::ServeDir;

/// The web UI is embedded into the console binary at build time, so a single
/// `dokitier-console` executable is enough to deploy (no external dist dir).
const INSTALL_SH: &str = include_str!("../../scripts/install.sh");

#[derive(rust_embed::RustEmbed)]
#[folder = "../web/webui/dist"]
struct WebAssets;

fn asset_content_type(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or_default() {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "map" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "ico" => "image/x-icon",
        "webp" => "image/webp",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "txt" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

async fn serve_webui(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };
    if let Some(asset) = WebAssets::get(path) {
        let content_type = asset_content_type(path);
        return (
            [(header::CONTENT_TYPE, HeaderValue::from_static(content_type))],
            asset.data.into_owned(),
        )
            .into_response();
    }
    // Hash-routed SPA: unknown paths fall back to the entry page.
    match WebAssets::get("index.html") {
        Some(index) => (
            [(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/html; charset=utf-8"),
            )],
            index.data.into_owned(),
        )
            .into_response(),
        None => (StatusCode::NOT_FOUND, "not found").into_response(),
    }
}

/// One TCP port serves both the web UI and the agent websocket, in plaintext
/// (`ws://`/`http://`) and TLS (`wss://`/`https://`): the first byte of a
/// connection decides the mode (TLS ClientHello starts with `0x16`).
enum MixedStream {
    Plain(tokio::net::TcpStream),
    Tls(tokio_rustls::server::TlsStream<tokio::net::TcpStream>),
}

impl AsyncRead for MixedStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        match &mut *self {
            MixedStream::Plain(stream) => Pin::new(stream).poll_read(cx, buf),
            MixedStream::Tls(stream) => Pin::new(stream).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for MixedStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        match &mut *self {
            MixedStream::Plain(stream) => Pin::new(stream).poll_write(cx, buf),
            MixedStream::Tls(stream) => Pin::new(stream).poll_write(cx, buf),
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match &mut *self {
            MixedStream::Plain(stream) => Pin::new(stream).poll_flush(cx),
            MixedStream::Tls(stream) => Pin::new(stream).poll_flush(cx),
        }
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match &mut *self {
            MixedStream::Plain(stream) => Pin::new(stream).poll_shutdown(cx),
            MixedStream::Tls(stream) => Pin::new(stream).poll_shutdown(cx),
        }
    }
}

struct MixedListener {
    listener: TcpListener,
    tls: tokio_rustls::TlsAcceptor,
}

impl axum::serve::Listener for MixedListener {
    type Io = MixedStream;
    type Addr = std::net::SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        loop {
            let (stream, addr) = match self.listener.accept().await {
                Ok(pair) => pair,
                Err(err) => {
                    tracing::warn!(?err, "console accept failed");
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                    continue;
                }
            };
            let _ = stream.set_nodelay(true);
            let mut first = [0u8; 1];
            match stream.peek(&mut first).await {
                Ok(0) | Err(_) => continue,
                Ok(_) if first[0] == 0x16 => match self.tls.accept(stream).await {
                    Ok(tls) => return (MixedStream::Tls(tls), addr),
                    Err(err) => {
                        tracing::debug!(?err, %addr, "console TLS handshake failed");
                        continue;
                    }
                },
                Ok(_) => return (MixedStream::Plain(stream), addr),
            }
        }
    }

    fn local_addr(&self) -> std::io::Result<Self::Addr> {
        self.listener.local_addr()
    }
}

#[derive(Clone)]
struct AppState {
    db: AnyPool,
    sessions: std::sync::Arc<dashmap::DashMap<String, String>>,
    postgres: bool,
    agents: std::sync::Arc<dashmap::DashMap<String, tokio::sync::mpsc::Sender<Value>>>,
    pending: std::sync::Arc<dashmap::DashMap<String, tokio::sync::oneshot::Sender<Value>>>,
    last_traffic: std::sync::Arc<dashmap::DashMap<String, (i64, u64, u64)>>,
    edge_traffic: std::sync::Arc<dashmap::DashMap<String, (i64, u64, u64)>>,
    edge_rates: std::sync::Arc<dashmap::DashMap<String, (f64, f64)>>,
    node_rt: std::sync::Arc<dashmap::DashMap<String, NodeRt>>,
    rt_flush: std::sync::Arc<dashmap::DashMap<String, i64>>,
    geo_attempts: std::sync::Arc<dashmap::DashMap<String, i64>>,
}

/// Live node state, authoritative for every realtime API. Only the periodic
/// flusher writes this to the database (batch, every 60s per node).
#[derive(Clone, Default)]
struct NodeRt {
    ts: i64,
    last_seen: i64,
    online: bool,
    hostname: String,
    version: String,
    networks: Value,
    snapshot: Value,
    system: Value,
    public_ipv4: Option<String>,
    public_ipv6: Option<String>,
    country_code: Option<String>,
    city: Option<String>,
    lat: Option<f64>,
    lon: Option<f64>,
    cpu_pct: f32,
    mem_pct: f32,
    mem_used: i64,
    mem_total: i64,
    rx_bps: f64,
    tx_bps: f64,
    traffic: Value,
    listeners: Vec<String>,
    prev_bytes: Option<(i64, u64, u64)>,
    pending_links: Vec<(String, i64, i64, Option<String>, Option<String>, i64, Option<f64>, Option<f64>)>,
}

static POSTGRES: AtomicBool = AtomicBool::new(false);
static MYSQL: AtomicBool = AtomicBool::new(false);
static DB_KIND: std::sync::OnceLock<String> = std::sync::OnceLock::new();
static CONSOLE_START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();

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

/// Fresh snapshots (JSON strings) from the in-memory runtime registry.
fn rt_snapshot_strings(state: &AppState) -> Vec<String> {
    let now_ts = now();
    state
        .node_rt
        .iter()
        .filter(|e| now_ts - e.value().ts <= RT_FRESH_SECS)
        .map(|e| e.value().snapshot.to_string())
        .collect()
}

/// (node_id, status, snapshot_json) for realtime endpoints.
fn rt_nodes_status_snapshot(state: &AppState) -> Vec<(String, String, String)> {
    let now_ts = now();
    state
        .node_rt
        .iter()
        .map(|e| {
            let rt = e.value();
            let status = if now_ts - rt.ts <= RT_FRESH_SECS
                && rt.online
                && agent_channel_present(state, e.key())
            {
                "online"
            } else {
                "offline"
            };
            (e.key().clone(), status.to_string(), rt.snapshot.to_string())
        })
        .collect()
}

/// (node_id, status, snapshot_json, traffic_json) for realtime endpoints.
fn rt_nodes_full(state: &AppState) -> Vec<(String, String, String, String)> {
    let now_ts = now();
    state
        .node_rt
        .iter()
        .map(|e| {
            let rt = e.value();
            let status = if now_ts - rt.ts <= RT_FRESH_SECS
                && rt.online
                && agent_channel_present(state, e.key())
            {
                "online"
            } else {
                "offline"
            };
            (e.key().clone(), status.to_string(), rt.snapshot.to_string(), rt.traffic.to_string())
        })
        .collect()
}

fn now() -> i64 { SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() as i64 }

/// Agents send a snapshot every second; a node counts as live only while its
/// latest snapshot is at most this old.
const RT_FRESH_SECS: i64 = 5;

/// The control plane can only push commands while the agent session is
/// registered. Online state and command availability must agree, so online
/// requires both a fresh snapshot and a registered channel.
fn agent_channel_present(state: &AppState, node_id: &str) -> bool {
    let prefix = format!("{node_id}:");
    state.agents.iter().any(|entry| entry.key().starts_with(&prefix))
}

async fn init_db(db: &AnyPool) -> anyhow::Result<()> {
    let nodes_ddl = if is_mysql() {
        "CREATE TABLE IF NOT EXISTS nodes (
            node_id VARCHAR(128) PRIMARY KEY,
            token_hash VARCHAR(128) NOT NULL,
            token VARCHAR(255),
            hostname VARCHAR(255) NOT NULL DEFAULT '',
            version VARCHAR(64) NOT NULL DEFAULT '',
            networks LONGTEXT,
            snapshot LONGTEXT,
            last_seen BIGINT NOT NULL DEFAULT 0,
            status VARCHAR(32) NOT NULL DEFAULT 'offline',
            public_ipv4 VARCHAR(64),
            public_ipv6 VARCHAR(128),
            country_code VARCHAR(8),
            city VARCHAR(128),
            latitude DOUBLE,
            longitude DOUBLE,
            rx_bps DOUBLE,
            tx_bps DOUBLE,
            alias VARCHAR(255),
            cpu_pct DOUBLE,
            mem_pct DOUBLE,
            mem_used BIGINT,
            mem_total BIGINT
        )"
    } else {
        "CREATE TABLE IF NOT EXISTS nodes (
            node_id TEXT PRIMARY KEY,
            token_hash TEXT NOT NULL,
            token TEXT,
            hostname TEXT NOT NULL DEFAULT '',
            version TEXT NOT NULL DEFAULT '',
            networks TEXT NOT NULL DEFAULT '[]',
            snapshot TEXT NOT NULL DEFAULT 'null',
            last_seen INTEGER NOT NULL DEFAULT 0,
            status TEXT NOT NULL DEFAULT 'offline',
            public_ipv4 TEXT,
            public_ipv6 TEXT,
            country_code TEXT,
            city TEXT,
            latitude REAL,
            longitude REAL,
            rx_bps REAL,
            tx_bps REAL,
            alias TEXT,
            cpu_pct REAL,
            mem_pct REAL,
            mem_used INTEGER,
            mem_total INTEGER
        )"
    };
    sqlx::query(nodes_ddl).execute(db).await?;
    let settings_ddl = if is_mysql() {
        "CREATE TABLE IF NOT EXISTS settings (k VARCHAR(128) PRIMARY KEY, v TEXT)"
    } else {
        "CREATE TABLE IF NOT EXISTS settings (k TEXT PRIMARY KEY, v TEXT)"
    };
    sqlx::query(settings_ddl).execute(db).await?;
    let instances_ddl = if is_mysql() {
        "CREATE TABLE IF NOT EXISTS network_instances (node_id VARCHAR(128), network VARCHAR(128), instance_id VARCHAR(64), created BIGINT, PRIMARY KEY (node_id, network))"
    } else {
        "CREATE TABLE IF NOT EXISTS network_instances (node_id TEXT, network TEXT, instance_id TEXT, created BIGINT, PRIMARY KEY (node_id, network))"
    };
    sqlx::query(instances_ddl).execute(db).await?;
    let _ = sqlx::query("ALTER TABLE nodes ADD COLUMN token TEXT").execute(db).await;
    for column in [
        "public_ipv4 TEXT", "public_ipv6 TEXT", "country_code TEXT", "city TEXT", "rx_bps REAL", "tx_bps REAL", "node_uuid TEXT", "system_info TEXT", "traffic TEXT", "bandwidth_mbps REAL", "bandwidth_up_mbps REAL", "bandwidth_down_mbps REAL", "bandwidth_intranet_down_mbps REAL", "bandwidth_internet_down_mbps REAL", "shared_listeners TEXT",
        "alias TEXT", "cpu_pct REAL", "mem_pct REAL", "mem_used BIGINT", "mem_total BIGINT",
    ] {
        let _ = sqlx::query(&format!("ALTER TABLE nodes ADD COLUMN {column}")).execute(db).await;
    }
    // geolocation columns: postgres needs double precision (REAL decodes as f32
    // and would break f64 deserialisation), sqlite/mysql are fine with REAL/DOUBLE
    if POSTGRES.load(Ordering::Relaxed) {
        for column in ["city TEXT", "latitude DOUBLE PRECISION", "longitude DOUBLE PRECISION"] {
            let _ = sqlx::query(&format!("ALTER TABLE nodes ADD COLUMN {column}")).execute(db).await;
        }
        // migrate the earlier REAL columns if they already exist
        for column in ["latitude", "longitude"] {
            let _ = sqlx::query(&format!("ALTER TABLE nodes ALTER COLUMN {column} TYPE DOUBLE PRECISION")).execute(db).await;
        }
    } else {
        for column in ["city TEXT", "latitude REAL", "longitude REAL"] {
            let _ = sqlx::query(&format!("ALTER TABLE nodes ADD COLUMN {column}")).execute(db).await;
        }
    }

    for column in ["a_uuid TEXT", "b_uuid TEXT"] {
        let _ = sqlx::query(&format!("ALTER TABLE link_metrics ADD COLUMN {column}")).execute(db).await;
    }
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
    let metrics_ddl = if is_mysql() {
        "CREATE TABLE IF NOT EXISTS link_metrics (
            id BIGINT AUTO_INCREMENT PRIMARY KEY,
            network VARCHAR(255) NOT NULL,
            a BIGINT NOT NULL,
            b BIGINT NOT NULL,
            a_uuid VARCHAR(64),
            b_uuid VARCHAR(64),
            ts BIGINT NOT NULL,
            latency_ms DOUBLE,
            loss_pct DOUBLE
        )"
    } else if POSTGRES.load(Ordering::Relaxed) {
        "CREATE TABLE IF NOT EXISTS link_metrics (
            id BIGSERIAL PRIMARY KEY,
            network TEXT NOT NULL,
            a BIGINT NOT NULL,
            b BIGINT NOT NULL,
            a_uuid VARCHAR(64),
            b_uuid VARCHAR(64),
            ts BIGINT NOT NULL,
            latency_ms DOUBLE PRECISION,
            loss_pct DOUBLE PRECISION
        )"
    } else {
        "CREATE TABLE IF NOT EXISTS link_metrics (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            network TEXT NOT NULL,
            a INTEGER NOT NULL,
            b INTEGER NOT NULL,
            a_uuid TEXT,
            b_uuid TEXT,
            ts INTEGER NOT NULL,
            latency_ms REAL,
            loss_pct REAL
        )"
    };
    sqlx::query(metrics_ddl).execute(db).await?;
    // query + retention indexes (network/pair for history, ts for pruning)
    let index_stmts = [
        "CREATE INDEX idx_link_metrics_pair_ts ON link_metrics(network, a_uuid, b_uuid, ts)",
        "CREATE INDEX idx_link_metrics_ab_ts ON link_metrics(network, a, b, ts)",
        "CREATE INDEX idx_link_metrics_ts ON link_metrics(ts)",
    ];
    for stmt in index_stmts {
        let sql = if is_mysql() {
            // MySQL has no IF NOT EXISTS for indexes; ignore duplicates
            stmt.to_string()
        } else {
            stmt.replace("CREATE INDEX idx", "CREATE INDEX IF NOT EXISTS idx")
        };
        let _ = sqlx::query(&sql).execute(db).await;
    }
    let samples_ddl = if POSTGRES.load(Ordering::Relaxed) {
        "CREATE TABLE IF NOT EXISTS bandwidth_samples (ts BIGINT PRIMARY KEY, rx_bps DOUBLE PRECISION NOT NULL, tx_bps DOUBLE PRECISION NOT NULL)"
    } else if is_mysql() {
        "CREATE TABLE IF NOT EXISTS bandwidth_samples (ts BIGINT PRIMARY KEY, rx_bps DOUBLE NOT NULL, tx_bps DOUBLE NOT NULL)"
    } else {
        "CREATE TABLE IF NOT EXISTS bandwidth_samples (ts INTEGER PRIMARY KEY, rx_bps REAL NOT NULL, tx_bps REAL NOT NULL)"
    };
    sqlx::query(samples_ddl).execute(db).await?;
    let _ = sqlx::query("ALTER TABLE users ADD COLUMN totp_secret TEXT").execute(db).await;
    let audit_ddl = if is_mysql() {
        "CREATE TABLE IF NOT EXISTS audit_logs (
            id BIGINT AUTO_INCREMENT PRIMARY KEY,
            ts BIGINT NOT NULL,
            action VARCHAR(64) NOT NULL,
            node_id VARCHAR(128) NOT NULL,
            network VARCHAR(255) NOT NULL,
            detail TEXT
        )"
    } else if POSTGRES.load(Ordering::Relaxed) {
        "CREATE TABLE IF NOT EXISTS audit_logs (
            id BIGSERIAL PRIMARY KEY,
            ts BIGINT NOT NULL,
            action TEXT NOT NULL,
            node_id TEXT NOT NULL,
            network TEXT NOT NULL,
            detail TEXT
        )"
    } else {
        "CREATE TABLE IF NOT EXISTS audit_logs (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            ts INTEGER NOT NULL,
            action TEXT NOT NULL,
            node_id TEXT NOT NULL,
            network TEXT NOT NULL,
            detail TEXT
        )"
    };
    sqlx::query(audit_ddl).execute(db).await?;
    let users_ddl = if is_mysql() {
        "CREATE TABLE IF NOT EXISTS users (
            id BIGINT AUTO_INCREMENT PRIMARY KEY,
            username VARCHAR(128) NOT NULL UNIQUE,
            password_hash TEXT NOT NULL,
            role VARCHAR(32) NOT NULL DEFAULT 'admin',
            created BIGINT NOT NULL
        )"
    } else if POSTGRES.load(Ordering::Relaxed) {
        "CREATE TABLE IF NOT EXISTS users (
            id BIGSERIAL PRIMARY KEY,
            username TEXT NOT NULL UNIQUE,
            password_hash TEXT NOT NULL,
            role TEXT NOT NULL DEFAULT 'admin',
            created BIGINT NOT NULL
        )"
    } else {
        "CREATE TABLE IF NOT EXISTS users (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            username TEXT NOT NULL UNIQUE,
            password_hash TEXT NOT NULL,
            role TEXT NOT NULL DEFAULT 'admin',
            created INTEGER NOT NULL
        )"
    };
    sqlx::query(users_ddl).execute(db).await?;
    Ok(())
}

async fn node_token(db: &AnyPool, node_id: &str) -> Option<String> {
    let row: Option<(Option<String>,)> = sqlx::query_as(&q("SELECT token FROM nodes WHERE node_id = ?"))
        .bind(node_id).fetch_optional(db).await.ok().flatten();
    row.and_then(|(token,)| token)
}

async fn verify_proof(db: &AnyPool, node_id: &str, nonce: &str, ts: i64, client_proof: &str) -> Option<String> {
    let token = node_token(db, node_id).await?;
    let expected = proof(&token, "client", node_id, nonce, ts);
    (expected == client_proof && (now() - ts).abs() < 120).then_some(token)
}

async fn verify_node(db: &AnyPool, node_id: &str, token: &str) -> bool {
    let row: Option<(String,)> = sqlx::query_as(&q("SELECT token_hash FROM nodes WHERE node_id = ?"))
        .bind(node_id).fetch_optional(db).await.ok().flatten();
    row.is_some_and(|(hash,)| hash == hash_token(token))
}

/// Merge one agent snapshot into the in-memory runtime registry. This is the
/// only place realtime values are produced; nothing here touches SQL.
fn apply_snapshot_to_rt(state: &AppState, node_id: &str, data: &Value) {
    let now_ts = now();
    let mut rx_total = 0u64;
    let mut tx_total = 0u64;
    let mut by_network: std::collections::BTreeMap<String, u64> = std::collections::BTreeMap::new();
    let mut pending: Vec<(String, i64, i64, Option<String>, Option<String>, i64, Option<f64>, Option<f64>)> = Vec::new();
    let mut nets: Vec<Value> = Vec::new();
    if let Some(instances) = data.get("instances").and_then(|v| v.as_array()) {
        for inst in instances {
            let network = inst.get("network_name").and_then(|v| v.as_str()).unwrap_or_default();
            if !network.is_empty() && !nets.iter().any(|v| v.as_str() == Some(network)) {
                nets.push(json!(network));
            }
            let Some(topo) = inst.get("topology") else { continue };
            let mut peer_uuid: std::collections::HashMap<i64, String> = std::collections::HashMap::new();
            for n in topo.get("nodes").and_then(|v| v.as_array()).into_iter().flatten() {
                let id = n.get("peer_id").and_then(|v| v.as_u64()).unwrap_or_default() as i64;
                if let Some(uuid) = n.get("instance_id").and_then(|v| v.as_str()).filter(|v| !v.is_empty()) {
                    peer_uuid.insert(id, uuid.to_string());
                }
            }
            for edge in topo.get("edges").and_then(|v| v.as_array()).into_iter().flatten() {
                let a = edge.get("a").and_then(|v| v.as_u64()).unwrap_or_default() as i64;
                let b = edge.get("b").and_then(|v| v.as_u64()).unwrap_or_default() as i64;
                let tunnels = edge.get("tunnels").and_then(|v| v.as_array()).cloned().unwrap_or_default();
                for tunnel in &tunnels {
                    let rx = tunnel.get("rx_bytes").and_then(|v| v.as_u64()).unwrap_or(0);
                    let tx = tunnel.get("tx_bytes").and_then(|v| v.as_u64()).unwrap_or(0);
                    rx_total += rx;
                    tx_total += tx;
                    if !network.is_empty() {
                        *by_network.entry(network.to_string()).or_insert(0) += rx + tx;
                    }
                }
                let latency = tunnels.iter().filter_map(|t| t.get("latency_ms").and_then(|v| v.as_f64())).reduce(f64::min);
                let loss = tunnels.iter().filter_map(|t| t.get("loss_pct").and_then(|v| v.as_f64())).reduce(|x, y| x + y)
                    .map(|v| v / tunnels.len().max(1) as f64);
                let (a_uuid, b_uuid) = match (peer_uuid.get(&a), peer_uuid.get(&b)) {
                    (Some(ua), Some(ub)) if ua <= ub => (Some(ua.clone()), Some(ub.clone())),
                    (Some(ua), Some(ub)) => (Some(ub.clone()), Some(ua.clone())),
                    _ => (peer_uuid.get(&a).cloned(), peer_uuid.get(&b).cloned()),
                };
                pending.push((network.to_string(), a, b, a_uuid, b_uuid, now_ts, latency, loss));
            }
        }
    }
    let merged = json!({"ts": now_ts, "instances": data.get("instances").cloned().unwrap_or(json!([]))});
    let mut rt = state.node_rt.entry(node_id.to_string()).or_default();
    let (prev_ts, prev_rx, prev_tx) = rt.prev_bytes.unwrap_or((now_ts, 0, 0));
    let elapsed = (now_ts - prev_ts).max(1) as f64;
    let mut rx_bps = 0.0;
    let mut tx_bps = 0.0;
    if rt.prev_bytes.is_some() {
        rx_bps = rx_total.saturating_sub(prev_rx) as f64 / elapsed;
        tx_bps = tx_total.saturating_sub(prev_tx) as f64 / elapsed;
    }
    rt.ts = now_ts;
    rt.last_seen = now_ts;
    rt.online = true;
    rt.hostname = data.pointer("/instances/0/hostname").and_then(|v| v.as_str()).unwrap_or("").to_string();
    rt.version = data.pointer("/instances/0/version").and_then(|v| v.as_str()).unwrap_or("").to_string();
    if !nets.is_empty() {
        rt.networks = Value::Array(nets);
    }
    rt.snapshot = merged;
    if let Some(sys) = data.get("system").filter(|v| !v.is_null()) {
        rt.system = sys.clone();
        if let Some(listener) = sys.get("shared_listener").and_then(|v| v.as_str()).filter(|v| !v.is_empty()) {
            rt.listeners = listener.split(',').map(str::trim).filter(|v| !v.is_empty()).map(|v| v.to_string()).collect();
        }
    }
    rt.public_ipv4 = data.get("public_ipv4").and_then(|v| v.as_str()).map(|v| v.to_string()).or(rt.public_ipv4.clone());
    rt.public_ipv6 = data.get("public_ipv6").and_then(|v| v.as_str()).map(|v| v.to_string()).or(rt.public_ipv6.clone());
    rt.cpu_pct = data.get("cpu_pct").and_then(|v| v.as_f64()).unwrap_or(rt.cpu_pct as f64) as f32;
    rt.mem_pct = data.get("mem_pct").and_then(|v| v.as_f64()).unwrap_or(rt.mem_pct as f64) as f32;
    if let Some(v) = data.get("mem_used").and_then(|v| v.as_u64()) { rt.mem_used = v as i64; }
    if let Some(v) = data.get("mem_total").and_then(|v| v.as_u64()) { rt.mem_total = v as i64; }
    rt.rx_bps = rx_bps;
    rt.tx_bps = tx_bps;
    rt.traffic = json!(by_network);
    rt.prev_bytes = Some((now_ts, rx_total, tx_total));
    rt.pending_links.extend(pending);
    if rt.pending_links.len() > 5000 {
        let drop = rt.pending_links.len() - 5000;
        rt.pending_links.drain(0..drop);
    }
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

async fn merge_snapshot(_state: &AppState, _node_id: &str, data: &Value) -> Value {
    // The agent snapshot already carries every running network instance; do not
    // merge instances from older snapshots or deleted networks would linger.
    let instances: Vec<Value> = data
        .get("instances")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    json!({ "ts": now(), "instances": instances })
}

async fn update_public_ip(db: &AnyPool, node_id: &str, v4: Option<&str>, v6: Option<&str>) {
    if v4.is_none() && v6.is_none() { return; }
    let _ = sqlx::query(&q("UPDATE nodes SET public_ipv4 = COALESCE(?, public_ipv4), public_ipv6 = COALESCE(?, public_ipv6) WHERE node_id = ?"))
        .bind(v4).bind(v6).bind(node_id).execute(db).await;
}

const GEO_PROVIDERS: [&str; 5] = ["ip9.com.cn", "ipwho.is", "ipinfo.io", "ip.sb", "ipapi.co"];

async fn get_setting(db: &AnyPool, key: &str) -> Option<String> {
    let row: Option<(String,)> = sqlx::query_as(&q("SELECT v FROM settings WHERE k = ?"))
        .bind(key).fetch_optional(db).await.ok().flatten();
    row.map(|(v,)| v)
}

async fn set_setting(db: &AnyPool, key: &str, value: &str) {
    let _ = sqlx::query(&q("DELETE FROM settings WHERE k = ?")).bind(key).execute(db).await;
    let _ = sqlx::query(&q("INSERT INTO settings (k, v) VALUES (?, ?)")).bind(key).bind(value).execute(db).await;
}

async fn ensure_instance_id(db: &AnyPool, node_id: &str, network: &str) -> String {
    let existing: Option<(String,)> = sqlx::query_as(
        &q("SELECT instance_id FROM network_instances WHERE node_id = ? AND network = ?"),
    )
    .bind(node_id)
    .bind(network)
    .fetch_optional(db)
    .await
    .ok()
    .flatten();
    if let Some((id,)) = existing {
        return id;
    }
    let id = uuid::Uuid::new_v4().to_string();
    let _ = sqlx::query(&q(
        "INSERT INTO network_instances (node_id, network, instance_id, created) VALUES (?, ?, ?, ?)",
    ))
    .bind(node_id)
    .bind(network)
    .bind(&id)
    .bind(now())
    .execute(db)
    .await;
    let stored: Option<(String,)> = sqlx::query_as(
        &q("SELECT instance_id FROM network_instances WHERE node_id = ? AND network = ?"),
    )
    .bind(node_id)
    .bind(network)
    .fetch_optional(db)
    .await
    .ok()
    .flatten();
    stored.map(|(v,)| v).unwrap_or(id)
}

fn inject_instance_name(text: &str, network: &str, node_id: &str) -> String {
    let Ok(mut value) = toml::from_str::<toml::Value>(text) else {
        return text.to_string();
    };
    let Some(table) = value.as_table_mut() else {
        return text.to_string();
    };
    let missing = table
        .get("instance_name")
        .and_then(|v| v.as_str())
        .map(|v| v.trim().is_empty())
        .unwrap_or(true);
    if missing {
        table.insert(
            "instance_name".to_string(),
            toml::Value::String(format!("{network}-{}", node_id.replace(':', "-"))),
        );
    }
    toml::to_string(&value).unwrap_or_else(|_| text.to_string())
}

fn inject_instance_id(text: &str, instance_id: &str) -> String {
    let Ok(mut value) = toml::from_str::<toml::Value>(text) else {
        return text.to_string();
    };
    let Some(table) = value.as_table_mut() else {
        return text.to_string();
    };
    table.insert(
        "instance_id".to_string(),
        toml::Value::String(instance_id.to_string()),
    );
    toml::to_string(&value).unwrap_or_else(|_| text.to_string())
}

#[derive(Clone, Debug)]
struct GeoInfo {
    country: Option<String>,
    city: Option<String>,
    lat: Option<f64>,
    lon: Option<f64>,
}

fn geo_f64(value: &Value, keys: &[&str]) -> Option<f64> {
    for key in keys {
        if let Some(v) = value.get(*key) {
            if let Some(n) = v.as_f64() {
                return Some(n);
            }
            if let Some(text) = v.as_str() {
                if let Ok(n) = text.parse::<f64>() {
                    return Some(n);
                }
            }
        }
    }
    None
}

fn geo_str(value: &Value, keys: &[&str]) -> Option<String> {
    for key in keys {
        if let Some(s) = value.get(*key).and_then(|v| v.as_str()) {
            if !s.trim().is_empty() {
                return Some(s.trim().to_string());
            }
        }
    }
    None
}

const GEO_RESOLVER_VERSION: &str = "ip9-v1";
const DEFAULT_GEO_PROVIDER: &str = "ip9.com.cn";

fn provider_url(provider: &str, ip: &str) -> String {
    match provider {
        "ip9.com.cn" => format!("https://ip9.com.cn/get?ip={ip}"),
        "ipinfo.io" => format!("https://ipinfo.io/{ip}/json"),
        "ip.sb" => format!("https://api.ip.sb/geoip/{ip}"),
        "ipapi.co" => format!("https://ipapi.co/{ip}/json/"),
        _ => format!("https://ipwho.is/{ip}"),
    }
}

/// Location from the configured provider. ip9.com.cn is the default because it
/// resolves Chinese cities at city level (e.g. Qingdao instead of Jinan).
async fn fetch_geo(client: &reqwest::Client, provider: &str, ip: &str) -> Option<GeoInfo> {
    let value: Value = client
        .get(provider_url(provider, ip))
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    if let Some(success) = value.get("success").and_then(|v| v.as_bool()) {
        if !success {
            return None;
        }
    }
    if provider == "ip9.com.cn" {
        if value.get("ret").and_then(|v| v.as_i64()) != Some(200) {
            return None;
        }
        let data = value.get("data")?;
        return Some(GeoInfo {
            country: geo_str(data, &["country_code"]).map(|s| s.to_lowercase()),
            city: geo_str(data, &["city"]),
            lat: geo_f64(data, &["lat", "latitude"]),
            lon: geo_f64(data, &["lng", "longitude", "lon"]),
        });
    }
    let country = geo_str(&value, &["country_code", "countryCode", "country"]).map(|s| s.to_lowercase());
    let city = geo_str(&value, &["city"]);
    let (mut lat, mut lon) = (
        geo_f64(&value, &["latitude", "lat"]),
        geo_f64(&value, &["longitude", "lon", "lng"]),
    );
    if lat.is_none() || lon.is_none() {
        if let Some(loc) = value.get("loc").and_then(|v| v.as_str()) {
            let mut parts = loc.split(',');
            lat = parts.next().and_then(|v| v.trim().parse::<f64>().ok());
            lon = parts.next().and_then(|v| v.trim().parse::<f64>().ok());
        }
    }
    if country.is_none() && city.is_none() && lat.is_none() {
        return None;
    }
    Some(GeoInfo { country, city, lat, lon })
}

async fn lookup_and_store_geo(state: AppState, node_id: String, ip: String) {
    let client = match reqwest::Client::builder().timeout(std::time::Duration::from_secs(8)).build() {
        Ok(client) => client,
        Err(_) => return,
    };
    let provider = get_setting(&state.db, "geo_provider")
        .await
        .unwrap_or_else(|| DEFAULT_GEO_PROVIDER.to_string());
    let Some(info) = fetch_geo(&client, &provider, &ip).await else { return };
    if let Some(code) = info.country.clone() {
        if code.len() == 2 {
            let _ = sqlx::query(&q("UPDATE nodes SET country_code = ? WHERE node_id = ?"))
                .bind(&code).bind(&node_id).execute(&state.db).await;
        }
    }
    let _ = sqlx::query(&q("UPDATE nodes SET city = COALESCE(?, city), latitude = COALESCE(?, latitude), longitude = COALESCE(?, longitude) WHERE node_id = ?"))
        .bind(info.city.clone()).bind(info.lat).bind(info.lon).bind(&node_id)
        .execute(&state.db).await;
    if let Some(mut rt) = state.node_rt.get_mut(&node_id) {
        if info.country.is_some() {
            rt.country_code = info.country.clone();
        }
        if info.city.is_some() {
            rt.city = info.city.clone();
        }
        if info.lat.is_some() {
            rt.lat = info.lat;
        }
        if info.lon.is_some() {
            rt.lon = info.lon;
        }
    }
}

fn default_routing_params() -> Value {
    json!({
        "latency_weight": 1.0,
        "loss_weight": 20.0,
        "bandwidth_weight": 0.5,
        "reference_bandwidth_mbps": 100.0,
        "utilization_weight": 10.0,
        "hop_weight": 0.05,
        "loss_gain": 1.0,
        "tcp_loss_weight": 1.0,
        "udp_loss_weight": 1.0,
        "kcp_loss_weight": 1.0,
        "quic_loss_weight": 1.0,
        "wg_loss_weight": 1.0,
        "min_bandwidth_mbps": 0.0,
        "weighted": true,
        "probe_enabled": true,
        "probe_interval_secs": 3600,
        "probe_payload_bytes": 262144,
        "probe_repeats": 2,
        "probe_max_concurrent": 2,
        "probe_idle_utilization_pct": 20.0,
        "probe_timeout_ms": 5000,
        "probe_max_range_mbps": 5000.0,
    })
}

async fn stored_routing_params(db: &AnyPool) -> Value {
    let raw = get_setting(db, "routing_params").await;
    match raw.and_then(|text| serde_json::from_str::<Value>(&text).ok()) {
        Some(mut value) => {
            let defaults = default_routing_params();
            if let (Some(target), Some(source)) = (value.as_object_mut(), defaults.as_object()) {
                for (key, def) in source {
                    target.entry(key.clone()).or_insert_with(|| def.clone());
                }
            }
            value
        }
        None => default_routing_params(),
    }
}

fn routing_params_valid(value: &Value) -> bool {
    let Some(object) = value.as_object() else { return false };
    for key in [
        "latency_weight", "loss_weight", "bandwidth_weight", "reference_bandwidth_mbps",
        "utilization_weight", "hop_weight", "loss_gain", "tcp_loss_weight",
        "udp_loss_weight", "kcp_loss_weight", "quic_loss_weight", "wg_loss_weight",
        "min_bandwidth_mbps",
        "probe_interval_secs",
        "probe_payload_bytes",
        "probe_repeats",
        "probe_max_concurrent",
        "probe_idle_utilization_pct",
        "probe_timeout_ms",
        "probe_max_range_mbps",
    ] {
        let Some(number) = object.get(key).and_then(|v| v.as_f64()) else { return false };
        if !number.is_finite() || number < 0.0 { return false; }
    }
    object
        .get("reference_bandwidth_mbps")
        .and_then(|v| v.as_f64())
        .map(|v| v > 0.0)
        .unwrap_or(false)
}

fn routing_frame(request_id: &str, params: &Value) -> Value {
    json!({
        "type": "command",
        "request_id": request_id,
        "command": {"kind": "apply_routing", "params": params},
    })
}

async fn push_routing_to_node(state: &AppState, node_id: &str) {
    let params = stored_routing_params(&state.db).await;
    let prefix = format!("{node_id}:");
    let Some(entry) = state.agents.iter().find(|e| e.key().starts_with(&prefix)) else { return };
    let request_id = uuid::Uuid::new_v4().to_string();
    let (tx, rx) = tokio::sync::oneshot::channel();
    state.pending.insert(request_id.clone(), tx);
    let frame = routing_frame(&request_id, &params);
    let sender = entry.value().clone();
    drop(entry);
    if sender.send(frame).await.is_ok() {
        tokio::spawn(async move {
            let _ = tokio::time::timeout(std::time::Duration::from_secs(15), rx).await;
        });
    } else {
        state.pending.remove(&request_id);
    }
}

fn process_rss_bytes() -> Option<u64> {
    #[cfg(target_os = "linux")]
    if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
        for line in status.lines() {
            if let Some(rest) = line.strip_prefix("VmRSS:") {
                if let Some(kb) = rest.split_whitespace().next().and_then(|v| v.parse::<u64>().ok()) {
                    return Some(kb * 1024);
                }
            }
        }
    }
    #[cfg(unix)]
    unsafe {
        let mut usage: libc::rusage = std::mem::zeroed();
        if libc::getrusage(libc::RUSAGE_SELF, &mut usage) == 0 {
            let rss = usage.ru_maxrss as u64;
            #[cfg(target_os = "macos")]
            return Some(rss);
            #[cfg(not(target_os = "macos"))]
            return Some(rss * 1024);
        }
    }
    let output = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    let kb: u64 = text.trim().parse().ok()?;
    Some(kb * 1024)
}

async fn database_size_bytes(db: &AnyPool) -> Option<i64> {
    let kind = DB_KIND.get().map(String::as_str).unwrap_or("");
    if kind == "postgres" {
        return sqlx::query_scalar::<_, i64>("SELECT pg_database_size(current_database())")
            .fetch_optional(db)
            .await
            .ok()
            .flatten();
    }
    if kind == "mysql" {
        return sqlx::query_scalar::<_, i64>(
            "SELECT COALESCE(SUM(data_length + index_length), 0) FROM information_schema.tables WHERE table_schema = DATABASE()",
        )
        .fetch_optional(db)
        .await
        .ok()
        .flatten();
    }
    let pages: Option<i64> = sqlx::query_scalar("PRAGMA page_count").fetch_optional(db).await.ok().flatten();
    let page_size: Option<i64> = sqlx::query_scalar("PRAGMA page_size").fetch_optional(db).await.ok().flatten();
    match (pages, page_size) {
        (Some(pages), Some(size)) => Some(pages * size),
        _ => None,
    }
}

fn spawn_bandwidth_sampler(state: AppState) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            let now_ts = now();
            let totals: (f64, f64) = state
                .node_rt
                .iter()
                .filter(|e| e.value().online && now_ts - e.value().ts <= RT_FRESH_SECS)
                .fold((0.0, 0.0), |acc, e| (acc.0 + e.value().rx_bps, acc.1 + e.value().tx_bps));
            let rx = totals.0;
            let tx = totals.1;
            {
                let ts = now();
                let _ = sqlx::query(&q("INSERT INTO bandwidth_samples (ts, rx_bps, tx_bps) VALUES (?, ?, ?)"))
                    .bind(ts)
                    .bind(rx)
                    .bind(tx)
                    .execute(&state.db)
                    .await;
                let _ = sqlx::query(&q("DELETE FROM bandwidth_samples WHERE ts < ?"))
                    .bind(ts - 7 * 24 * 3600)
                    .execute(&state.db)
                    .await;
            }
        }
    });
}

#[derive(serde::Deserialize)]
struct BandwidthHistoryQuery {
    #[serde(default = "default_bw_range")]
    range: String,
}

fn default_bw_range() -> String { "1h".to_string() }

async fn board_bandwidth_history(State(state): State<AppState>, axum::extract::Query(query): axum::extract::Query<BandwidthHistoryQuery>) -> Json<Value> {
    let (step, span) = match query.range.as_str() {
        "60s" => (1, 60),
        "24h" => (300, 24 * 3600),
        "7d" => (1800, 7 * 24 * 3600),
        "30d" => (7200, 30 * 24 * 3600),
        _ => (60, 3600),
    };
    let since = now() - span;
    let rows: Vec<(i64, f64, f64)> = sqlx::query_as(
        &q("SELECT ts, rx_bps, tx_bps FROM bandwidth_samples WHERE ts >= ? ORDER BY ts"),
    )
    .bind(since)
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();
    let mut buckets: std::collections::BTreeMap<i64, (f64, f64, i64)> = std::collections::BTreeMap::new();
    for (ts, rx, tx) in rows {
        let bucket = (ts / step) * step;
        let entry = buckets.entry(bucket).or_insert((0.0, 0.0, 0));
        entry.0 += rx;
        entry.1 += tx;
        entry.2 += 1;
    }
    let points: Vec<Value> = buckets
        .into_iter()
        .map(|(ts, (rx, tx, count))| {
            json!({"ts": ts, "rx_bps": rx / count as f64, "tx_bps": tx / count as f64})
        })
        .collect();
    Json(json!({"points": points}))
}

#[derive(serde::Deserialize)]
struct PrecheckBody {
    network: String,
    toml: String,
}

async fn precheck_config(Json(body): Json<PrecheckBody>) -> Json<Value> {
    let mut errors: Vec<String> = Vec::new();
    if let Err(err) = validate_network_toml(&body.network, &body.toml) {
        errors.push(err);
    }
    match toml::from_str::<toml::Value>(&body.toml) {
        Ok(value) => {
            if let Some(listeners) = value.get("listeners").and_then(|v| v.as_array()) {
                for listener in listeners {
                    let Some(text) = listener.as_str() else {
                        errors.push("listener must be a string".to_string());
                        continue;
                    };
                    if url::Url::parse(text).is_err() {
                        errors.push(format!("invalid listener url: {text}"));
                    }
                }
            }
        }
        Err(err) => errors.push(format!("invalid TOML: {err}")),
    }
    Json(json!({"ok": errors.is_empty(), "errors": errors}))
}

async fn delete_network_on_node(state: &AppState, node_id: &str, network: &str) -> Value {
    let prefix = format!("{node_id}:");
    let key = state.agents.iter().find(|entry| entry.key().starts_with(&prefix)).map(|entry| entry.key().clone()).unwrap_or_default();
    if key.is_empty() {
        return json!({"ok": false, "error": "node is offline"});
    }
    let Some(tx) = state.agents.get(&key).map(|v| v.clone()) else {
        return json!({"ok": false, "error": "node is offline"});
    };
    let command = dokitier_controller::ConsoleCommand::DeleteNetwork { network: network.to_string() };
    let request_id = uuid::Uuid::new_v4().to_string();
    let (result_tx, result_rx) = tokio::sync::oneshot::channel();
    state.pending.insert(request_id.clone(), result_tx);
    let frame = json!({"type": "command", "request_id": request_id, "command": serde_json::to_value(&command).unwrap_or(Value::Null)});
    if tx.send(frame).await.is_err() {
        state.pending.remove(&request_id);
        return json!({"ok": false, "error": "agent channel closed"});
    }
    match tokio::time::timeout(std::time::Duration::from_secs(20), result_rx).await {
        Ok(Ok(value)) => {
            let _ = sqlx::query(&q("DELETE FROM network_instances WHERE node_id = ? AND network = ?"))
                .bind(node_id).bind(network).execute(&state.db).await;
            value
        }
        Ok(Err(_)) => json!({"ok": false, "error": "agent result channel closed"}),
        Err(_) => {
            state.pending.remove(&request_id);
            json!({"ok": false, "error": "agent command timeout"})
        }
    }
}

async fn apply_new_network(state: &AppState, node_id: &str, network: &str, toml: &str) -> Value {
    let instance_id = ensure_instance_id(&state.db, node_id, network).await;
    let mut toml = inject_instance_id(toml, &instance_id);
    toml = inject_instance_name(&toml, network, node_id);
    let acl_chains = load_acl_chains(&state.db).await;
    if let Ok(mut value) = toml::from_str::<toml::Value>(&toml) {
        if let Some(table) = value.as_table_mut() {
            inject_acl(table, &acl_chains);
            if let Ok(text) = toml::to_string(&value) { toml = text; }
        }
    }
    let key = {
        let exact = format!("{node_id}:{network}");
        let prefix = format!("{node_id}:");
        let mut found = None;
        // the agent may be in the middle of reconnecting; give it a few seconds
        for _ in 0..15 {
            if state.agents.contains_key(&exact) {
                found = Some(exact.clone());
                break;
            }
            if let Some(entry) = state.agents.iter().find(|entry| entry.key().starts_with(&prefix)) {
                found = Some(entry.key().clone());
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
        match found {
            Some(key) => key,
            None => return json!({"ok": false, "error": "控制面通道不可用：节点实时数据在线，但 agent 会话未建立（可能正在重连）"}),
        }
    };
    let Some(tx) = state.agents.get(&key).map(|v| v.clone()) else {
        return json!({"ok": false, "error": "node is offline"});
    };
    let command = dokitier_controller::ConsoleCommand::ApplyNetwork {
        network: network.to_string(),
        toml: toml.clone(),
    };
    let request_id = uuid::Uuid::new_v4().to_string();
    let (result_tx, result_rx) = tokio::sync::oneshot::channel();
    state.pending.insert(request_id.clone(), result_tx);
    let frame = json!({"type": "command", "request_id": request_id, "command": serde_json::to_value(&command).unwrap_or(Value::Null)});
    if tx.send(frame).await.is_err() {
        state.pending.remove(&request_id);
        return json!({"ok": false, "error": "agent channel closed"});
    }
    match tokio::time::timeout(std::time::Duration::from_secs(30), result_rx).await {
        Ok(Ok(value)) => {
            if value.get("ok").and_then(|v| v.as_bool()) == Some(true) {
                let _ = sqlx::query(&q(
                    "INSERT INTO config_versions (node_id, network, hash, toml, created) VALUES (?, ?, ?, ?, ?)",
                ))
                .bind(node_id).bind(network).bind(hash_token(&toml)).bind(&toml).bind(now())
                .execute(&state.db).await;
                let _ = sqlx::query(&q("INSERT INTO audit_logs (ts, action, node_id, network, detail) VALUES (?, ?, ?, ?, ?)"))
                    .bind(now()).bind("network.create").bind(node_id).bind(network).bind(hash_token(&toml))
                    .execute(&state.db).await;
            }
            value
        }
        Ok(Err(_)) => json!({"ok": false, "error": "agent result channel closed"}),
        Err(_) => {
            state.pending.remove(&request_id);
            json!({"ok": false, "error": "agent command timeout"})
        }
    }
}

#[derive(serde::Deserialize)]
struct CreateNetworkNode {
    node_id: String,
    #[serde(default)]
    p2p_group: Option<String>,
}

#[derive(serde::Deserialize)]
struct CreateNetworkBody {
    name: String,
    secret: String,
    #[serde(default = "default_ip_mode")]
    ip_mode: String,
    #[serde(default)]
    subnet: Option<String>,
    #[serde(default)]
    nodes: Vec<CreateNetworkNode>,
    #[serde(default)]
    auto_connect: bool,
    #[serde(default)]
    no_tun: bool,
}

fn default_ip_mode() -> String { "auto".to_string() }

async fn create_network(State(state): State<AppState>, Json(body): Json<CreateNetworkBody>) -> Json<Value> {
    let name = body.name.trim().to_string();
    if name.len() < 2 || name.len() > 64 || !name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')) {
        return Json(json!({"ok": false, "error": "网络名称只能包含字母/数字/-/_/.，长度 2-64"}));
    }
    if body.secret.len() < 16 {
        return Json(json!({"ok": false, "error": "网络密钥至少 16 位"}));
    }
    if body.nodes.is_empty() {
        return Json(json!({"ok": false, "error": "至少选择一个节点"}));
    }
    let existing: Vec<(String,)> = sqlx::query_as("SELECT DISTINCT network FROM config_versions")
        .fetch_all(&state.db).await.unwrap_or_default();
    if existing.iter().any(|(network,)| network == &name) {
        return Json(json!({"ok": false, "error": "网络名称已存在"}));
    }
    // Collect used /24 subnets from the in-memory runtime snapshots.
    let node_ids: Vec<(String,)> = sqlx::query_as("SELECT node_id FROM nodes")
        .fetch_all(&state.db).await.unwrap_or_default();
    let now_ts = now();
    let mut used_subnets = std::collections::HashSet::new();
    let mut node_info: std::collections::HashMap<String, (String, String, String)> = std::collections::HashMap::new();
    for (node_id,) in &node_ids {
        let (status, snapshot, system_info) = match state.node_rt.get(node_id) {
            Some(rt) => (
                if now_ts - rt.ts <= RT_FRESH_SECS && rt.online && agent_channel_present(&state, node_id) {
                    "online".to_string()
                } else {
                    "offline".to_string()
                },
                rt.snapshot.to_string(),
                rt.system.to_string(),
            ),
            None => ("offline".to_string(), "null".to_string(), "{}".to_string()),
        };
        node_info.insert(node_id.clone(), (status, snapshot.clone(), system_info));
        if let Ok(value) = serde_json::from_str::<Value>(&snapshot) {
            for inst in value.get("instances").and_then(|v| v.as_array()).into_iter().flatten() {
                if let Some(ip) = inst.get("ipv4").and_then(|v| v.as_str()) {
                    if let Some((a, b, c, _)) = parse_ipv4(ip) {
                        used_subnets.insert(format!("{a}.{b}.{c}.0/24"));
                    }
                }
            }
        }
    }
    // downstream code works on the same row shape as before
    let rows: Vec<(String, String, String, String)> = node_info
        .iter()
        .map(|(id, (status, snapshot, system))| (id.clone(), status.clone(), snapshot.clone(), system.clone()))
        .collect();
    let mode = body.ip_mode.as_str();
    let subnet = if mode == "manual" {
        None
    } else if let Some(subnet) = body.subnet.clone().filter(|s| !s.trim().is_empty()) {
        if parse_cidr24(&subnet).is_none() {
            return Json(json!({"ok": false, "error": "网段必须是 /24 CIDR"}));
        }
        if used_subnets.contains(&subnet.trim().to_string()) {
            return Json(json!({"ok": false, "error": "网段与其他网络冲突"}));
        }
        Some(subnet.trim().to_string())
    } else if mode == "dhcp" {
        None
    } else {
        let mut chosen = None;
        for third in 200..255 {
            let candidate = format!("10.{third}.0.0/24");
            if !used_subnets.contains(&candidate) { chosen = Some(candidate); break; }
        }
        let Some(chosen) = chosen else {
            return Json(json!({"ok": false, "error": "没有可用的自动网段"}));
        };
        Some(chosen)
    };
    // Unique listener port across all existing networks on the same node.
    let mut used_ports = std::collections::HashSet::new();
    for (_, _, snapshot, _) in &rows {
        if let Ok(value) = serde_json::from_str::<Value>(snapshot) {
            for inst in value.get("instances").and_then(|v| v.as_array()).into_iter().flatten() {
                if let Some(listeners) = inst.get("listeners").and_then(|v| v.as_array()) {
                    for listener in listeners {
                        if let Some(text) = listener.as_str() {
                            if let Some(port) = text.rsplit(':').next().and_then(|p| p.parse::<u16>().ok()) {
                                used_ports.insert(port);
                            }
                        }
                    }
                }
            }
        }
    }
    // Also reserve ports used by already stored (not yet applied) configs.
    let stored: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT node_id, network, toml FROM config_versions ORDER BY id DESC",
    ).fetch_all(&state.db).await.unwrap_or_default();
    let mut seen_networks = std::collections::HashSet::new();
    for (_, network, toml) in stored {
        if !seen_networks.insert(network) { continue; }
        if let Ok(value) = toml::from_str::<toml::Value>(&toml) {
            if let Some(listeners) = value.get("listeners").and_then(|v| v.as_array()) {
                for listener in listeners {
                    if let Some(text) = listener.as_str() {
                        if let Some(port) = text.rsplit(':').next().and_then(|p| p.parse::<u16>().ok()) {
                            used_ports.insert(port);
                        }
                    }
                }
            }
        }
    }
    let shared_port = rows.iter().find_map(|(_, _, _, system_info)| {
        serde_json::from_str::<Value>(system_info).ok().and_then(|value| {
            value.get("shared_listener").and_then(|v| v.as_str()).and_then(|addr| {
                addr.rsplit(':').next().and_then(|port| port.trim_end_matches('/').parse::<u16>().ok())
            })
        })
    });
    let Some(port) = shared_port else {
        return Json(json!({
            "ok": false,
            "error": "节点未启用 core 级共享监听（[controller] shared_listeners）"
        }));
    };
    let mut underlay: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let mut peer_ids: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
    let mut quality: std::collections::HashMap<(u32, u32), (f64, f64)> = std::collections::HashMap::new();
    for (node_id, _, snapshot, system_info) in &rows {
        if let Ok(value) = serde_json::from_str::<Value>(system_info) {
            if let Some(ip) = value
                .pointer("/network/interfaces/0/ipv4/0")
                .and_then(|v| v.as_str())
                .map(|s| s.split('/').next().unwrap_or(s).to_string())
            {
                underlay.insert(node_id.clone(), ip);
            }
        }
        if let Ok(value) = serde_json::from_str::<Value>(snapshot) {
            if let Some(pid) = value.pointer("/instances/0/peer_id").and_then(|v| v.as_u64()) {
                peer_ids.insert(node_id.clone(), pid as u32);
            }
            if let Some(instances) = value.get("instances").and_then(|v| v.as_array()) {
                for inst in instances {
                    for edge in inst.pointer("/topology/edges").and_then(|v| v.as_array()).into_iter().flatten() {
                        let (Some(a), Some(b)) = (
                            edge.get("a").and_then(|v| v.as_u64()).map(|v| v as u32),
                            edge.get("b").and_then(|v| v.as_u64()).map(|v| v as u32),
                        ) else { continue };
                        let Some(tunnel) = edge.get("tunnels").and_then(|v| v.as_array()).and_then(|v| v.first()) else { continue };
                        let latency = tunnel.get("latency_ms").and_then(|v| v.as_f64()).unwrap_or(9999.0);
                        let loss = tunnel.get("loss_pct").and_then(|v| v.as_f64()).unwrap_or(100.0);
                        quality.entry((a.min(b), a.max(b))).or_insert((latency, loss));
                    }
                }
            }
        }
    }
    // Node-level shared listeners (core shared public port) must be used as
    // peer endpoints, otherwise auto-created networks would point at the
    // per-network listener port that the shared listener rewrite removes.
    let shared_rows: Vec<(String, Option<String>)> =
        sqlx::query_as("SELECT node_id, shared_listeners FROM nodes")
            .fetch_all(&state.db)
            .await
            .unwrap_or_default();
    let mut shared_endpoint: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for (node_id, shared) in shared_rows {
        let shared = shared.unwrap_or_default();
        let Some(first) = shared.split(',').map(str::trim).find(|value| !value.is_empty()) else { continue };
        let proto = first.split_once("://").map(|(proto, _)| proto).unwrap_or("tcp");
        let Some(port) = first.rsplit(':').next().and_then(|port| port.parse::<u16>().ok()) else { continue };
        if let Some(ip) = underlay.get(&node_id) {
            let host = if ip.contains(':') { format!("[{ip}]") } else { ip.clone() };
            shared_endpoint.insert(node_id, format!("{proto}://{host}:{port}"));
        }
    }
    let base = subnet.as_ref().and_then(|s| s.trim_end_matches("/24").parse::<std::net::Ipv4Addr>().ok());
    let mut created = Vec::new();
    let mut applied = Vec::new();
    let mut failed = Vec::new();
    for (index, node) in body.nodes.iter().enumerate() {
        let Some((status, snapshot, system_info)) = node_info.get(&node.node_id) else {
            failed.push(json!({"node_id": node.node_id, "error": "node not found"}));
            continue;
        };
        if status != "online" {
            failed.push(json!({"node_id": node.node_id, "error": "node offline"}));
            continue;
        }
        let underlay_ip = serde_json::from_str::<Value>(system_info).ok()
            .and_then(|v| v.pointer("/network/interfaces/0/ipv4/0").and_then(|v| v.as_str()).map(|s| s.split('/').next().unwrap_or(s).to_string()))
            .or_else(|| serde_json::from_str::<Value>(snapshot).ok()
                .and_then(|v| v.pointer("/instances/0/public_ipv4").and_then(|v| v.as_str()).map(|s| s.to_string())));
        let ipv4 = match (mode, base) {
            ("auto", Some(base)) => {
                let octets = base.octets();
                Some(format!("{}.{}.{}.{}/24", octets[0], octets[1], octets[2], index + 2))
            }
            _ => None,
        };
        let mut lines = Vec::new();
        lines.push(format!("instance_name = \"{}-{}\"", name, node.node_id.replace(':', "-")));
        if let Some(ipv4) = &ipv4 { lines.push(format!("ipv4 = \"{ipv4}\"")); }
        if let Some(group) = node.p2p_group.clone().filter(|g| !g.is_empty()) { lines.push(format!("p2p_group = \"{group}\"")); }
        if mode == "dhcp" {
            if let Some(subnet) = &subnet { lines.push(format!("dhcp = \"{subnet}\"")); } else { lines.push("dhcp = true".to_string()); }
        }
        if shared_port.is_none() {
            lines.push(format!("listeners = [\"tcp://0.0.0.0:{port}\"]"));
        }
        lines.push(String::new());
        lines.push("[network_identity]".to_string());
        lines.push(format!("network_name = \"{name}\""));
        lines.push(format!("network_secret = \"{}\"", body.secret.replace('"', "\\\"")));
        if body.no_tun {
            lines.push(String::new());
            lines.push("[flags]".to_string());
            lines.push("no_tun = true".to_string());
        }
        if body.auto_connect {
            // Peers must use the underlay address, never the VPN IP.
            let mut candidates: Vec<(String, String, f64, f64)> = Vec::new();
            for other in &body.nodes {
                if other.node_id == node.node_id { continue; }
                let Some(other_ip) = underlay.get(&other.node_id).cloned() else { continue };
                let endpoint = shared_endpoint
                    .get(&other.node_id)
                    .cloned()
                    .unwrap_or_else(|| format!("tcp://{other_ip}:{port}"));
                let (latency, loss) = match (peer_ids.get(&node.node_id), peer_ids.get(&other.node_id)) {
                    (Some(a), Some(b)) => quality.get(&((*a).min(*b), (*a).max(*b))).copied().unwrap_or((9999.0, 100.0)),
                    _ => (9999.0, 100.0),
                };
                candidates.push((other.node_id.clone(), endpoint, latency, loss));
            }
            candidates.sort_by(|a, b| a.2.partial_cmp(&b.2).unwrap_or(std::cmp::Ordering::Equal).then(a.3.partial_cmp(&b.3).unwrap_or(std::cmp::Ordering::Equal)));
            let prefer = node.p2p_group.clone().filter(|g| !g.is_empty());
            let mut chosen: Vec<&(String, String, f64, f64)> = Vec::new();
            for candidate in &candidates {
                if let Some(group) = &prefer {
                    if let Some(other) = body.nodes.iter().find(|other| other.node_id == candidate.0) {
                        if other.p2p_group.as_deref() != Some(group.as_str()) { continue; }
                    }
                }
                chosen.push(candidate);
                if chosen.len() >= 2 { break; }
            }
            if chosen.is_empty() {
                if let Some(first) = body.nodes.first().filter(|other| other.node_id != node.node_id) {
                    if let Some(ip) = underlay.get(&first.node_id).cloned() {
                        let endpoint = shared_endpoint
                            .get(&first.node_id)
                            .cloned()
                            .unwrap_or_else(|| format!("tcp://{ip}:{port}"));
                        lines.push(String::new());
                        lines.push("[[peer]]".to_string());
                        lines.push(format!("uri = \"{endpoint}\""));
                    }
                }
            } else {
                for (_, endpoint, _, _) in chosen {
                    lines.push(String::new());
                    lines.push("[[peer]]".to_string());
                    lines.push(format!("uri = \"{endpoint}\""));
                }
            }
        }
        let toml_text = lines.join("\n");
        let _ = sqlx::query(&q("INSERT INTO config_versions (node_id, network, hash, toml, created) VALUES (?, ?, ?, ?, ?)"))
            .bind(&node.node_id).bind(&name).bind(hash_token(&toml_text)).bind(&toml_text).bind(now())
            .execute(&state.db).await;
        created.push(json!({"node_id": node.node_id, "ipv4": ipv4, "listener_port": port, "toml": toml_text}));
        if body.auto_connect {
            let mut result = apply_new_network(&state, &node.node_id, &name, &toml_text).await;
            let failed_text = result.get("error").and_then(|v| v.as_str()).unwrap_or("").to_ascii_lowercase();
            if result.get("ok").and_then(|v| v.as_bool()) != Some(true)
                && (failed_text.contains("tun") || failed_text.contains("permitted") || failed_text.contains("permission"))
                && !toml_text.contains("no_tun = true")
            {
                // Auto-fallback: retry without TUN when the node cannot create it.
                let fallback = if toml_text.contains("[flags]") {
                    toml_text.replace("[flags]", "[flags]\nno_tun = true")
                } else {
                    format!("{toml_text}\n\n[flags]\nno_tun = true\n")
                };
                let retry = apply_new_network(&state, &node.node_id, &name, &fallback).await;
                if retry.get("ok").and_then(|v| v.as_bool()) == Some(true) {
                    result = json!({"ok": true, "fallback_no_tun": true});
                    let _ = sqlx::query(&q("UPDATE config_versions SET toml = ?, hash = ? WHERE node_id = ? AND network = ?"))
                        .bind(&fallback).bind(hash_token(&fallback)).bind(&node.node_id).bind(&name)
                        .execute(&state.db).await;
                }
            }
            if result.get("ok").and_then(|v| v.as_bool()) == Some(true) { applied.push(node.node_id.clone()); }
            else { failed.push(json!({"node_id": node.node_id, "error": result.get("error").cloned().unwrap_or(Value::Null)})); }
        }
        let _ = underlay_ip;
    }
    if body.auto_connect && failed.is_empty() && applied.len() > 1 {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            let mut all_connected = true;
            for node in &applied {
                let snapshot = state.node_rt.get(node).map(|rt| rt.snapshot.clone());
                let connected = snapshot
                    .and_then(|value| Ok::<Value, ()>(value).ok())
                    .map(|value| {
                        value.get("instances").and_then(|v| v.as_array()).into_iter().flatten().any(|inst| {
                            inst.get("network_name").and_then(|v| v.as_str()) == Some(name.as_str())
                                && inst.pointer("/topology/edges").and_then(|v| v.as_array()).map(|e| !e.is_empty()).unwrap_or(false)
                        })
                    })
                    .unwrap_or(false);
                if !connected { all_connected = false; break; }
            }
            if all_connected { break; }
            if tokio::time::Instant::now() >= deadline {
                failed.push(json!({"node_id": "mesh", "error": "节点之间未建立连接（检查 TUN/端口/密钥）"}));
                break;
            }
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
    }
    if body.auto_connect && !failed.is_empty() {
        // Partial rollout must not leave a half-created network behind.
        for node in &applied {
            let _ = delete_network_on_node(&state, node, &name).await;
        }
        let _ = sqlx::query(&q("DELETE FROM config_versions WHERE network = ?"))
            .bind(&name).execute(&state.db).await;
        let _ = sqlx::query(&q("DELETE FROM network_instances WHERE network = ?"))
            .bind(&name).execute(&state.db).await;
        created.clear();
        applied.clear();
    }
    Json(json!({
        "ok": failed.is_empty(),
        "network": name,
        "listener_port": port,
        "subnet": subnet,
        "created": created,
        "applied": applied,
        "failed": failed,
    }))
}

fn parse_ipv4(text: &str) -> Option<(u8, u8, u8, u8)> {
    let ip = text.split('/').next()?;
    let mut parts = ip.split('.');
    Some((parts.next()?.parse().ok()?, parts.next()?.parse().ok()?, parts.next()?.parse().ok()?, parts.next()?.parse().ok()?))
}

fn parse_cidr24(text: &str) -> Option<()> {
    let (ip, prefix) = text.trim().split_once('/')?;
    if prefix != "24" { return None; }
    let (a, b, c, d) = parse_ipv4(ip)?;
    if d != 0 { return None; }
    let _ = (a, b, c);
    Some(())
}

fn existing_link_quality(rows: &[(String, String, String, String)], a: &str, b: &str) -> (f64, f64) {
    for (node_id, _, snapshot, _) in rows {
        if node_id != a && node_id != b { continue; }
        let Ok(value) = serde_json::from_str::<Value>(snapshot) else { continue };
        for inst in value.get("instances").and_then(|v| v.as_array()).into_iter().flatten() {
            for edge in inst.pointer("/topology/edges").and_then(|v| v.as_array()).into_iter().flatten() {
                if let Some(tunnel) = edge.get("tunnels").and_then(|v| v.as_array()).and_then(|v| v.first()) {
                    let latency = tunnel.get("latency_ms").and_then(|v| v.as_f64()).unwrap_or(9999.0);
                    let loss = tunnel.get("loss_pct").and_then(|v| v.as_f64()).unwrap_or(100.0);
                    return (latency, loss);
                }
            }
        }
    }
    (9999.0, 100.0)
}

async fn board_networks(State(state): State<AppState>) -> Json<Value> {
    let rows: Vec<(String, String, String, String)> = rt_nodes_full(&state);
    let versions: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT node_id, network, toml FROM config_versions ORDER BY id DESC",
    )
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();
    let mut latest: std::collections::HashMap<(String, String), String> = std::collections::HashMap::new();
    for (node, network, toml) in versions {
        latest.entry((node, network)).or_insert(toml);
    }
    let mut networks: std::collections::BTreeMap<String, Value> = std::collections::BTreeMap::new();
    for (node_id, status, snapshot, traffic) in rows {
        let traffic: Value = serde_json::from_str(&traffic).unwrap_or(Value::Null);
        let Ok(snapshot) = serde_json::from_str::<Value>(&snapshot) else { continue };
        for inst in snapshot.get("instances").and_then(|v| v.as_array()).into_iter().flatten() {
            let Some(name) = inst.get("network_name").and_then(|v| v.as_str()) else { continue };
            // Ignore stale snapshot entries whose config has been deleted.
            if !latest.contains_key(&(node_id.clone(), name.to_string())) { continue; }
            let entry = networks.entry(name.to_string()).or_insert_with(|| json!({
                "network": name,
                "nodes_total": 0,
                "nodes_online": 0,
                "subnets": [],
                "p2p_groups": [],
                "listeners": [],
                "link_count": 0,
                "latency_sum": 0.0,
                "loss_sum": 0.0,
                "sample_count": 0,
                "util_sum": 0.0,
                "util_count": 0,
                "traffic": 0.0,
                "acl_rules": 0,
                "nodes": [],
            }));
            let online = status == "online";
            entry["nodes_total"] = json!(entry["nodes_total"].as_u64().unwrap_or(0) + 1);
            if online { entry["nodes_online"] = json!(entry["nodes_online"].as_u64().unwrap_or(0) + 1); }
            if let Some(traffic_value) = traffic.get(name).and_then(|v| v.as_u64()) {
                entry["traffic"] = json!(entry["traffic"].as_f64().unwrap_or(0.0) + traffic_value as f64);
            }
            if let Some(ip) = inst.get("ipv4").and_then(|v| v.as_str()) {
                if let Some(list) = entry["subnets"].as_array_mut() {
                    let subnet = ip.split('.').take(3).collect::<Vec<_>>().join(".") + ".0/24";
                    if !list.iter().any(|v| v.as_str() == Some(subnet.as_str())) { list.push(json!(subnet)); }
                }
            }
            if let Some(group) = inst.get("p2p_group").and_then(|v| v.as_str()).filter(|g| !g.is_empty()) {
                if let Some(list) = entry["p2p_groups"].as_array_mut() {
                    if !list.iter().any(|v| v.as_str() == Some(group)) { list.push(json!(group)); }
                }
            }
            for edge in inst.pointer("/topology/edges").and_then(|v| v.as_array()).into_iter().flatten() {
                entry["link_count"] = json!(entry["link_count"].as_u64().unwrap_or(0) + 1);
                if let Some(tunnel) = edge.get("tunnels").and_then(|v| v.as_array()).and_then(|v| v.first()) {
                    if let Some(lat) = tunnel.get("latency_ms").and_then(|v| v.as_f64()) {
                        entry["latency_sum"] = json!(entry["latency_sum"].as_f64().unwrap_or(0.0) + lat);
                    }
                    if let Some(loss) = tunnel.get("loss_pct").and_then(|v| v.as_f64()) {
                        entry["loss_sum"] = json!(entry["loss_sum"].as_f64().unwrap_or(0.0) + loss);
                    }
                    entry["sample_count"] = json!(entry["sample_count"].as_u64().unwrap_or(0) + 1);
                    if let Some(util) = tunnel.get("utilization").and_then(|v| v.as_f64()) {
                        entry["util_sum"] = json!(entry["util_sum"].as_f64().unwrap_or(0.0) + util);
                        entry["util_count"] = json!(entry["util_count"].as_u64().unwrap_or(0) + 1);
                    }
                }
            }
            if let Some(toml_text) = latest.get(&(node_id.clone(), name.to_string())) {
                if let Ok(value) = toml::from_str::<toml::Value>(toml_text) {
                    if let Some(listeners) = value.get("listeners").and_then(|v| v.as_array()) {
                        if let Some(list) = entry["listeners"].as_array_mut() {
                            for listener in listeners {
                                if let Some(text) = listener.as_str() {
                                    if !list.iter().any(|v| v.as_str() == Some(text)) { list.push(json!(text)); }
                                }
                            }
                        }
                    }
                    if let Some(rules) = value
                        .get("acl")
                        .and_then(|v| v.get("acl_v1"))
                        .and_then(|v| v.get("chains"))
                        .and_then(|v| v.as_array())
                    {
                        let count: usize = rules.iter().map(|chain| {
                            chain.get("rules").and_then(|v| v.as_array()).map(|r| r.len()).unwrap_or(0)
                        }).sum();
                        entry["acl_rules"] = json!(count);
                    }
                }
            }
            let ipv4 = inst.get("ipv4").cloned().unwrap_or(Value::Null);
            if let Some(list) = entry["nodes"].as_array_mut() {
                list.push(json!({"node_id": node_id, "status": status, "ipv4": ipv4}));
            }
        }
    }
    let items: Vec<Value> = networks.into_values().map(|mut entry| {
        let samples = entry["sample_count"].as_u64().unwrap_or(0).max(1);
        let lat = entry["latency_sum"].as_f64().unwrap_or(0.0) / samples as f64;
        let loss = entry["loss_sum"].as_f64().unwrap_or(0.0) / samples as f64;
        entry["avg_latency_ms"] = json!(lat);
        entry["avg_loss_pct"] = json!(loss);
        let util_count = entry["util_count"].as_u64().unwrap_or(0).max(1);
        let util = entry["util_sum"].as_f64().unwrap_or(0.0) / util_count as f64;
        entry["load_pct"] = json!((util * 100.0).clamp(0.0, 100.0));
        entry.as_object_mut().map(|obj| {
            obj.remove("latency_sum"); obj.remove("loss_sum"); obj.remove("sample_count");
            obj.remove("util_sum"); obj.remove("util_count");
        });
        entry
    }).collect();
    Json(json!({"networks": items}))
}

async fn board_drift(State(state): State<AppState>) -> Json<Value> {
    let rows: Vec<(String, String, String)> = rt_nodes_status_snapshot(&state);
    let instances: Vec<(String, String, String)> =
        sqlx::query_as("SELECT node_id, network, instance_id FROM network_instances")
            .fetch_all(&state.db)
            .await
            .unwrap_or_default();
    let expected: std::collections::HashMap<(String, String), String> = instances
        .into_iter()
        .map(|(node, network, id)| ((node, network), id))
        .collect();
    let mut drift = Vec::new();
    for (node_id, status, snapshot) in rows {
        let Ok(value) = serde_json::from_str::<Value>(&snapshot) else { continue };
        let reported: std::collections::HashMap<String, String> = value
            .get("instances")
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
            .filter_map(|inst| {
                Some((
                    inst.get("network_name")?.as_str()?.to_string(),
                    inst.get("instance_id")?.as_str()?.to_string(),
                ))
            })
            .collect();
        for ((node, network), expected_id) in expected.iter() {
            if node != &node_id || expected_id.is_empty() {
                continue;
            }
            match reported.get(network) {
                None => drift.push(json!({"node_id": node_id, "network": network, "kind": "missing", "expected": expected_id, "reported": null, "status": status})),
                Some(actual) if actual != expected_id => drift.push(json!({"node_id": node_id, "network": network, "kind": "mismatch", "expected": expected_id, "reported": actual, "status": status})),
                _ => {}
            }
        }
    }
    Json(json!({"count": drift.len(), "items": drift}))
}

async fn get_perf(State(state): State<AppState>) -> Json<Value> {
    let db_size = database_size_bytes(&state.db).await;
    let nodes: i64 = sqlx::query_scalar("SELECT count(*) FROM nodes").fetch_optional(&state.db).await.ok().flatten().unwrap_or(0);
    let config_versions: i64 = sqlx::query_scalar("SELECT count(*) FROM config_versions").fetch_optional(&state.db).await.ok().flatten().unwrap_or(0);
    let link_rows: i64 = sqlx::query_scalar("SELECT count(*) FROM link_metrics").fetch_optional(&state.db).await.ok().flatten().unwrap_or(0);
    let audit_rows: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_logs").fetch_optional(&state.db).await.ok().flatten().unwrap_or(0);
    let uptime = CONSOLE_START.get().map(|start| start.elapsed().as_secs()).unwrap_or(0);
    Json(json!({
        "db_kind": DB_KIND.get().cloned().unwrap_or_default(),
        "db_size": db_size,
        "nodes": nodes,
        "config_versions": config_versions,
        "link_metrics": link_rows,
        "audit_logs": audit_rows,
        "rss_bytes": process_rss_bytes(),
        "uptime_secs": uptime,
    }))
}

async fn load_acl_chains(db: &AnyPool) -> Vec<Value> {
    let raw: Option<String> = sqlx::query_scalar("SELECT v FROM settings WHERE k = 'acl_rules'")
        .fetch_optional(db)
        .await
        .ok()
        .flatten();
    raw.and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|value| value.get("chains").and_then(|v| v.as_array()).cloned())
        .unwrap_or_default()
}

fn inject_acl(table: &mut toml::value::Table, chains: &[Value]) {
    if chains.is_empty() {
        return;
    }
    let mut acl_v1 = toml::value::Table::new();
    let mut chain_array = Vec::new();
    for chain in chains {
        let mut chain_table = toml::value::Table::new();
        chain_table.insert(
            "name".to_string(),
            toml::Value::String(chain.get("name").and_then(|v| v.as_str()).unwrap_or("global").to_string()),
        );
        chain_table.insert(
            "chain_type".to_string(),
            toml::Value::Integer(chain.get("chain_type").and_then(|v| v.as_i64()).unwrap_or(1)),
        );
        chain_table.insert(
            "default_action".to_string(),
            toml::Value::Integer(chain.get("default_action").and_then(|v| v.as_i64()).unwrap_or(1)),
        );
        chain_table.insert(
            "enabled".to_string(),
            toml::Value::Boolean(chain.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true)),
        );
        let mut rules = Vec::new();
        for rule in chain.get("rules").and_then(|v| v.as_array()).into_iter().flatten() {
            let mut rule_table = toml::value::Table::new();
            rule_table.insert(
                "name".to_string(),
                toml::Value::String(rule.get("name").and_then(|v| v.as_str()).unwrap_or("rule").to_string()),
            );
            rule_table.insert(
                "enabled".to_string(),
                toml::Value::Boolean(rule.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true)),
            );
            rule_table.insert(
                "action".to_string(),
                toml::Value::Integer(rule.get("action").and_then(|v| v.as_i64()).unwrap_or(0)),
            );
            rule_table.insert(
                "protocol".to_string(),
                toml::Value::Integer(rule.get("protocol").and_then(|v| v.as_i64()).unwrap_or(5)),
            );
            rule_table.insert(
                "priority".to_string(),
                toml::Value::Integer(rule.get("priority").and_then(|v| v.as_i64()).unwrap_or(100)),
            );
            for key in ["source_ips", "destination_ips", "ports"] {
                let values: Vec<toml::Value> = rule
                    .get(key)
                    .and_then(|v| v.as_array())
                    .into_iter()
                    .flatten()
                    .filter_map(|value| value.as_str().map(|text| toml::Value::String(text.to_string())))
                    .collect();
                if !values.is_empty() {
                    rule_table.insert(key.to_string(), toml::Value::Array(values));
                }
            }
            rules.push(toml::Value::Table(rule_table));
        }
        chain_table.insert("rules".to_string(), toml::Value::Array(rules));
        chain_array.push(toml::Value::Table(chain_table));
    }
    acl_v1.insert("chains".to_string(), toml::Value::Array(chain_array));
    let mut acl = toml::value::Table::new();
    acl.insert("acl_v1".to_string(), toml::Value::Table(acl_v1));
    table.insert("acl".to_string(), toml::Value::Table(acl));
}

async fn get_acl(State(state): State<AppState>) -> Json<Value> {
    Json(json!({"chains": load_acl_chains(&state.db).await}))
}

async fn set_acl(State(state): State<AppState>, Json(body): Json<Value>) -> Json<Value> {
    let Some(chains) = body.get("chains").and_then(|v| v.as_array()) else {
        return Json(json!({"ok": false, "error": "chains array is required"}));
    };
    for chain in chains {
        if chain.get("name").and_then(|v| v.as_str()).is_none() {
            return Json(json!({"ok": false, "error": "chain name is required"}));
        }
        let chain_type = chain.get("chain_type").and_then(|v| v.as_i64()).unwrap_or(1);
        if !(1..=3).contains(&chain_type) {
            return Json(json!({"ok": false, "error": "invalid chain_type"}));
        }
        for rule in chain.get("rules").and_then(|v| v.as_array()).into_iter().flatten() {
            let action = rule.get("action").and_then(|v| v.as_i64()).unwrap_or(0);
            if !(0..=2).contains(&action) {
                return Json(json!({"ok": false, "error": "invalid action"}));
            }
            let protocol = rule.get("protocol").and_then(|v| v.as_i64()).unwrap_or(5);
            if !(1..=5).contains(&protocol) {
                return Json(json!({"ok": false, "error": "invalid protocol"}));
            }
        }
    }
    let payload = json!({"chains": chains}).to_string();
    let sql = if state.postgres {
        "INSERT INTO settings (k, v) VALUES ('acl_rules', ?) ON CONFLICT(k) DO UPDATE SET v = excluded.v"
    } else {
        "INSERT OR REPLACE INTO settings (k, v) VALUES ('acl_rules', ?)"
    };
    let _ = sqlx::query(&q(sql)).bind(&payload).execute(&state.db).await;
    // Convert the global ACL into every node's latest config version so the
    // node config sidebar and the applied runtime config stay identical.
    let rows: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT node_id, network, toml FROM config_versions ORDER BY id DESC",
    )
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();
    let mut seen = std::collections::HashSet::new();
    for (node_id, network, toml) in rows {
        if !seen.insert((node_id.clone(), network.clone())) {
            continue;
        }
        let Ok(mut value) = toml::from_str::<toml::Value>(&toml) else { continue };
        let Some(table) = value.as_table_mut() else { continue };
        inject_acl(table, chains);
        let Ok(text) = toml::to_string(&value) else { continue };
        let hash = format!("{:x}", Sha256::digest(text.as_bytes()));
        let _ = sqlx::query(&q(
            "INSERT INTO config_versions (node_id, network, hash, toml, created) VALUES (?, ?, ?, ?, ?)",
        ))
        .bind(&node_id)
        .bind(&network)
        .bind(&hash)
        .bind(&text)
        .bind(chrono::Utc::now().timestamp())
        .execute(&state.db)
        .await;
    }
    let node_ids: Vec<(String,)> = sqlx::query_as("SELECT node_id FROM nodes").fetch_all(&state.db).await.unwrap_or_default();
    for (node_id,) in node_ids {
        spawn_reconcile_inner(state.clone(), node_id, true);
    }
    Json(json!({"ok": true}))
}

async fn totp_setup(State(state): State<AppState>) -> Json<Value> {
    let secret = base32_encode(uuid::Uuid::new_v4().as_bytes());
    let sql = if state.postgres {
        "INSERT INTO settings (k, v) VALUES ('totp_pending', ?) ON CONFLICT(k) DO UPDATE SET v = excluded.v"
    } else {
        "INSERT OR REPLACE INTO settings (k, v) VALUES ('totp_pending', ?)"
    };
    let _ = sqlx::query(&q(&sql)).bind(&secret).execute(&state.db).await;
    let url = format!("otpauth://totp/DokiTier:admin?secret={secret}&issuer=DokiTier&algorithm=SHA256&digits=6&period=30");
    Json(json!({"secret": secret, "url": url}))
}

async fn totp_verify_setup(State(state): State<AppState>, Json(body): Json<Value>) -> Json<Value> {
    let code = body.get("code").and_then(|v| v.as_str()).unwrap_or_default();
    let pending: Option<String> = sqlx::query_scalar("SELECT v FROM settings WHERE k = 'totp_pending'")
        .fetch_optional(&state.db).await.ok().flatten();
    let Some(secret) = pending.filter(|s| !s.is_empty()) else {
        return Json(json!({"ok": false, "error": "no pending totp setup"}));
    };
    if !totp_verify(&secret, code) {
        return Json(json!({"ok": false, "error": "invalid code"}));
    }
    let _ = sqlx::query(&q("UPDATE users SET totp_secret = ? WHERE username = 'admin'"))
        .bind(&secret).execute(&state.db).await;
    let sql = if state.postgres {
        "INSERT INTO settings (k, v) VALUES ('security_totp', '1') ON CONFLICT(k) DO UPDATE SET v = '1'"
    } else {
        "INSERT OR REPLACE INTO settings (k, v) VALUES ('security_totp', '1')"
    };
    let _ = sqlx::query(&q(sql)).execute(&state.db).await;
    Json(json!({"ok": true}))
}

async fn totp_disable(State(state): State<AppState>, Json(body): Json<Value>) -> Json<Value> {
    let enabled: Option<String> = sqlx::query_scalar("SELECT v FROM settings WHERE k = 'security_totp'")
        .fetch_optional(&state.db).await.ok().flatten();
    if enabled.as_deref() == Some("1") {
        let secret: Option<String> = sqlx::query_scalar(&q("SELECT totp_secret FROM users WHERE username = 'admin'"))
            .fetch_optional(&state.db).await.ok().flatten();
        if let Some(secret) = secret.filter(|s| !s.is_empty()) {
            let code = body.get("code").and_then(|v| v.as_str()).unwrap_or_default();
            if !totp_verify(&secret, code) {
                return Json(json!({"ok": false, "error": "invalid code"}));
            }
        }
    }
    let _ = sqlx::query("UPDATE users SET totp_secret = NULL WHERE username = 'admin'").execute(&state.db).await;
    let sql = if state.postgres {
        "INSERT INTO settings (k, v) VALUES ('security_totp', '0') ON CONFLICT(k) DO UPDATE SET v = '0'"
    } else {
        "INSERT OR REPLACE INTO settings (k, v) VALUES ('security_totp', '0')"
    };
    let _ = sqlx::query(&q(sql)).execute(&state.db).await;
    Json(json!({"ok": true}))
}

async fn get_security_settings(State(state): State<AppState>) -> Json<Value> {
    let trust = sqlx::query_scalar::<_, String>("SELECT v FROM settings WHERE k = 'security_trust_proxy'")
        .fetch_optional(&state.db).await.ok().flatten();
    let limit = sqlx::query_scalar::<_, String>("SELECT v FROM settings WHERE k = 'security_rate_limit'")
        .fetch_optional(&state.db).await.ok().flatten();
    let totp = sqlx::query_scalar::<_, String>("SELECT v FROM settings WHERE k = 'security_totp'")
        .fetch_optional(&state.db).await.ok().flatten();
    Json(json!({
        "trust_proxy": trust.as_deref() != Some("0"),
        "rate_limit": limit.as_deref() != Some("0"),
        "totp": totp.as_deref() == Some("1"),
    }))
}

async fn set_security_settings(State(state): State<AppState>, Json(body): Json<Value>) -> Json<Value> {
    for (key, value) in [
        ("security_trust_proxy", body.get("trust_proxy").and_then(|v| v.as_bool())),
        ("security_rate_limit", body.get("rate_limit").and_then(|v| v.as_bool())),
        ("security_totp", body.get("totp").and_then(|v| v.as_bool())),
    ] {
        if let Some(value) = value {
            let flag = if value { "1" } else { "0" };
            let sql = if state.postgres {
                "INSERT INTO settings (k, v) VALUES ('".to_string() + key + "', ?) ON CONFLICT(k) DO UPDATE SET v = excluded.v"
            } else {
                "INSERT OR REPLACE INTO settings (k, v) VALUES ('".to_string() + key + "', ?)"
            };
            let _ = sqlx::query(&q(&sql)).bind(flag).execute(&state.db).await;
        }
    }
    Json(json!({"ok": true}))
}

async fn get_routing_settings(State(state): State<AppState>) -> Json<Value> {
    Json(stored_routing_params(&state.db).await)
}

async fn set_routing_settings(State(state): State<AppState>, Json(body): Json<Value>) -> Json<Value> {
    if !routing_params_valid(&body) {
        return Json(json!({"ok": false, "error": "invalid routing params"}));
    }
    set_setting(&state.db, "routing_params", &body.to_string()).await;
    // online set comes from the memory registry (realtime), not SQL
    let now_ts = now();
    let nodes: Vec<String> = state
        .node_rt
        .iter()
        .filter(|e| e.value().online && now_ts - e.value().ts <= RT_FRESH_SECS)
        .map(|e| e.key().clone())
        .collect();
    for node_id in nodes {
        push_routing_to_node(&state, &node_id).await;
    }
    Json(json!({"ok": true, "params": body}))
}

async fn get_geo_setting(State(state): State<AppState>) -> Json<Value> {
    let provider = get_setting(&state.db, "geo_provider")
        .await
        .unwrap_or_else(|| DEFAULT_GEO_PROVIDER.to_string());
    Json(json!({"provider": provider, "options": GEO_PROVIDERS}))
}

#[derive(serde::Deserialize)]
struct GeoSettingBody {
    provider: String,
}

async fn set_geo_setting(State(state): State<AppState>, Json(body): Json<GeoSettingBody>) -> Json<Value> {
    if !GEO_PROVIDERS.contains(&body.provider.as_str()) {
        return Json(json!({"ok": false, "error": "unsupported geo provider"}));
    }
    set_setting(&state.db, "geo_provider", &body.provider).await;
    state.geo_attempts.clear();
    spawn_geo_reresolve_all(state.clone());
    Json(json!({"ok": true, "provider": body.provider}))
}

async fn update_traffic(state: &AppState, node_id: &str, data: &Value) {
    let mut rx_total = 0u64;
    let mut tx_total = 0u64;
    let mut by_network: std::collections::BTreeMap<String, u64> = std::collections::BTreeMap::new();
    if let Some(instances) = data.get("instances").and_then(|v| v.as_array()) {
        for inst in instances {
            let network = inst.get("network_name").and_then(|v| v.as_str()).unwrap_or_default();
            let Some(topo) = inst.get("topology") else { continue };
            for edge in topo.get("edges").and_then(|v| v.as_array()).into_iter().flatten() {
                for tunnel in edge.get("tunnels").and_then(|v| v.as_array()).into_iter().flatten() {
                    let bytes = tunnel.get("rx_bytes").and_then(|v| v.as_u64()).unwrap_or(0)
                        + tunnel.get("tx_bytes").and_then(|v| v.as_u64()).unwrap_or(0);
                    rx_total += tunnel.get("rx_bytes").and_then(|v| v.as_u64()).unwrap_or(0);
                    tx_total += tunnel.get("tx_bytes").and_then(|v| v.as_u64()).unwrap_or(0);
                    if !network.is_empty() {
                        *by_network.entry(network.to_string()).or_insert(0) += bytes;
                    }
                }
            }
        }
    }
    let traffic = serde_json::to_string(&by_network).unwrap_or_else(|_| "{}".to_string());
    let now = now();
    let mut rx_bps = 0.0;
    let mut tx_bps = 0.0;
    if let Some(prev) = state.last_traffic.get(node_id) {
        let (prev_ts, prev_rx, prev_tx) = *prev;
        let elapsed = (now - prev_ts).max(1) as f64;
        rx_bps = rx_total.saturating_sub(prev_rx) as f64 / elapsed;
        tx_bps = tx_total.saturating_sub(prev_tx) as f64 / elapsed;
    }
    state.last_traffic.insert(node_id.to_string(), (now, rx_total, tx_total));
    let _ = sqlx::query(&q("UPDATE nodes SET rx_bps = ?, tx_bps = ?, traffic = ? WHERE node_id = ?"))
        .bind(rx_bps).bind(tx_bps).bind(&traffic).bind(node_id).execute(&state.db).await;
}

fn spawn_reconcile(state: AppState, node_id: String) {
    spawn_reconcile_inner(state, node_id, false);
}

fn spawn_reconcile_inner(state: AppState, node_id: String, force: bool) {
    tokio::spawn(async move {
        // give the core a moment to load persisted networks and report a snapshot
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        let rows = sqlx::query(&q(
            "SELECT network, toml FROM config_versions WHERE node_id = ? ORDER BY created DESC",
        ))
        .bind(&node_id)
        .fetch_all(&state.db)
        .await
        .unwrap_or_default();
        let mut desired: std::collections::HashMap<String, String> = std::collections::HashMap::new();
        for row in rows {
            let network: String = row.try_get("network").unwrap_or_default();
            let toml: String = row.try_get("toml").unwrap_or_default();
            if !network.is_empty() {
                desired.entry(network).or_insert(toml);
            }
        }
        if desired.is_empty() {
            return;
        }
        let instances = sqlx::query(&q(
            "SELECT network, instance_id FROM network_instances WHERE node_id = ?",
        ))
        .bind(&node_id)
        .fetch_all(&state.db)
        .await
        .unwrap_or_default();
        let expected: std::collections::HashMap<String, String> = instances
            .into_iter()
            .map(|row| {
                (
                    row.try_get::<String, _>("network").unwrap_or_default(),
                    row.try_get::<String, _>("instance_id").unwrap_or_default(),
                )
            })
            .collect();
        let bandwidth: Option<(Option<f32>, Option<f32>, Option<f32>, Option<f32>)> = sqlx::query_as(
            &q("SELECT bandwidth_up_mbps, bandwidth_down_mbps, bandwidth_intranet_down_mbps, bandwidth_internet_down_mbps FROM nodes WHERE node_id = ?"),
        )
        .bind(&node_id)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten();
        let snapshot: String = state
            .node_rt
            .get(&node_id)
            .map(|rt| rt.snapshot.to_string())
            .unwrap_or_default();
        let current: std::collections::HashMap<String, String> = serde_json::from_str::<Value>(&snapshot)
            .ok()
            .and_then(|v| v.get("instances").and_then(|i| i.as_array()).cloned())
            .map(|instances| {
                instances
                    .into_iter()
                    .filter_map(|inst| {
                        let name = inst.get("network_name").and_then(|v| v.as_str())?.to_string();
                        let id = inst.get("instance_id").and_then(|v| v.as_str())?.to_string();
                        Some((name, id))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let acl_chains = load_acl_chains(&state.db).await;
        for (network, toml) in &desired {
            let mut toml = toml.clone();
            if let Some((up, down, intranet, internet)) = bandwidth {
                if up.is_some() || down.is_some() || intranet.is_some() || internet.is_some() {
                    if let Ok(mut value) = toml::from_str::<toml::Value>(&toml) {
                        if let Some(table) = value.as_table_mut() {
                            if let Some(up) = up {
                                table.insert("node_bandwidth_up_mbps".to_string(), toml::Value::Float(f64::from(up)));
                            }
                            if let Some(down) = down {
                                table.insert("node_bandwidth_down_mbps".to_string(), toml::Value::Float(f64::from(down)));
                            }
                            if let Some(cap) = intranet {
                                table.insert("node_bandwidth_intranet_down_mbps".to_string(), toml::Value::Float(f64::from(cap)));
                            }
                            if let Some(cap) = internet {
                                table.insert("node_bandwidth_internet_down_mbps".to_string(), toml::Value::Float(f64::from(cap)));
                            }
                            if let Ok(text) = toml::to_string(&value) {
                                toml = text;
                            }
                        }
                    }
                }
            }
            if !acl_chains.is_empty() {
                if let Ok(mut value) = toml::from_str::<toml::Value>(&toml) {
                    if let Some(table) = value.as_table_mut() {
                        inject_acl(table, &acl_chains);
                        if let Ok(text) = toml::to_string(&value) {
                            toml = text;
                        }
                    }
                }
            }
            let expected_id = expected.get(network).cloned().unwrap_or_default();
            let current_id = current.get(network).cloned().unwrap_or_default();
            if !force && !current_id.is_empty() && current_id == expected_id {
                continue;
            }
            let key = format!("{node_id}:{network}");
            let key = if state.agents.contains_key(&key) {
                key
            } else {
                let prefix = format!("{node_id}:");
                match state.agents.iter().find(|e| e.key().starts_with(&prefix)) {
                    Some(entry) => entry.key().clone(),
                    None => continue,
                }
            };
            let Some(tx) = state.agents.get(&key).map(|v| v.clone()) else { continue };
            let request_id = uuid::Uuid::new_v4().to_string();
            let (result_tx, result_rx) = tokio::sync::oneshot::channel();
            state.pending.insert(request_id.clone(), result_tx);
            let frame = json!({
                "type": "command",
                "request_id": request_id,
                "command": {"kind": "apply_network", "network": network, "toml": toml},
            });
            if tx.send(frame).await.is_ok() {
                tokio::spawn(async move {
                    let _ = tokio::time::timeout(std::time::Duration::from_secs(30), result_rx).await;
                });
            } else {
                state.pending.remove(&request_id);
            }
        }
        // console is authoritative: remove networks the core runs but console does not know
        for (network, _) in &current {
            if desired.contains_key(network) {
                continue;
            }
            let key = format!("{node_id}:{network}");
            let key = if state.agents.contains_key(&key) {
                key
            } else {
                let prefix = format!("{node_id}:");
                match state.agents.iter().find(|e| e.key().starts_with(&prefix)) {
                    Some(entry) => entry.key().clone(),
                    None => continue,
                }
            };
            let Some(tx) = state.agents.get(&key).map(|v| v.clone()) else { continue };
            let request_id = uuid::Uuid::new_v4().to_string();
            let (result_tx, result_rx) = tokio::sync::oneshot::channel();
            state.pending.insert(request_id.clone(), result_tx);
            let frame = json!({
                "type": "command",
                "request_id": request_id,
                "command": {"kind": "delete_network", "network": network},
            });
            if tx.send(frame).await.is_ok() {
                tokio::spawn(async move {
                    let _ = tokio::time::timeout(std::time::Duration::from_secs(30), result_rx).await;
                });
            } else {
                state.pending.remove(&request_id);
            }
        }
    });
}

async fn handle_agent_frame(state: &AppState, frame: AgentFrame) -> Result<(), String> {
    match frame {
        AgentFrame::Hello { node_id, token, hostname, version, networks, public_ipv4, public_ipv6, instance_id, system, .. } => {
            if !verify_node(&state.db, &node_id, &token).await {
                return Err("unknown node or invalid token".into());
            }
            eprintln!("HELLO {node_id} v4={:?} v6={:?}", public_ipv4, public_ipv6);
            {
                let mut rt = state.node_rt.entry(node_id.clone()).or_default();
                if !hostname.is_empty() { rt.hostname = hostname.clone(); }
                if !version.is_empty() { rt.version = version.clone(); }
                if !networks.is_empty() { rt.networks = Value::Array(networks.clone()); }
                rt.online = true;
                rt.last_seen = now();
            }
            if let Some(info) = system.as_ref().filter(|v| !v.is_null()) {
                let _ = sqlx::query(&q("UPDATE nodes SET system_info = ? WHERE node_id = ?"))
                    .bind(info.to_string()).bind(&node_id).execute(&state.db).await;
                if let Some(shared) = info.get("shared_listener").and_then(|v| v.as_str()).filter(|v| !v.is_empty()) {
                    let existing: Option<String> = sqlx::query_scalar(&q("SELECT shared_listeners FROM nodes WHERE node_id = ?"))
                        .bind(&node_id).fetch_optional(&state.db).await.ok().flatten();
                    if existing.filter(|v| !v.trim().is_empty()).is_none() {
                        let _ = sqlx::query(&q("UPDATE nodes SET shared_listeners = ? WHERE node_id = ?"))
                            .bind(shared).bind(&node_id).execute(&state.db).await;
                    }
                }
            }
            if let Some(uuid) = instance_id.as_deref().filter(|v| !v.is_empty()) {
                let _ = sqlx::query(&q("UPDATE nodes SET node_uuid = ? WHERE node_id = ?"))
                    .bind(uuid).bind(&node_id).execute(&state.db).await;
            }
            update_public_ip(&state.db, &node_id, public_ipv4.as_deref(), public_ipv6.as_deref()).await;
            spawn_reconcile(state.clone(), node_id.clone());
            {
                let state = state.clone();
                let node_id = node_id.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                    push_routing_to_node(&state, &node_id).await;
                    let stored: Option<String> = sqlx::query_scalar(&q("SELECT shared_listeners FROM nodes WHERE node_id = ?"))
                        .bind(&node_id).fetch_optional(&state.db).await.ok().flatten();
                    if let Some(stored) = stored.filter(|v| !v.trim().is_empty()) {
                        let listeners: Vec<String> = stored.split(',').map(str::trim).filter(|v| !v.is_empty()).map(|v| v.to_string()).collect();
                        let _ = send_shared_listeners(&state, &node_id, listeners).await;
                    }
                });
            }
            if let Some(ip) = public_ipv4.clone().or(public_ipv6.clone()) {
                let db = state.db.clone();
                let id = node_id.clone();
                let state_for_geo = state.clone();
                tokio::spawn(async move { lookup_and_store_geo(state_for_geo, id, ip).await });
            }
            Ok(())
        }
        AgentFrame::Heartbeat { node_id, .. } => {
            if let Some(mut rt) = state.node_rt.get_mut(&node_id) {
                rt.last_seen = now();
                rt.online = true;
            }
            Ok(())
        }
        AgentFrame::Snapshot { node_id, data } => {
            // Memory only: the runtime registry is the authoritative source for
            // every realtime API; SQL is written by the periodic batch flusher.
            apply_snapshot_to_rt(state, &node_id, &data);
            let rt = state
                .node_rt
                .get(&node_id)
                .map(|r| (r.public_ipv4.clone(), r.public_ipv6.clone()));
            if let Some((v4, v6)) = rt {
                if let Some(ip) = v4.or(v6) {
                    let due = state.geo_attempts.get(&node_id).map(|v| now() - *v > 300).unwrap_or(true);
                    if due {
                        state.geo_attempts.insert(node_id.clone(), now());
                        let db = state.db.clone();
                        let id = node_id.clone();
                        let state_for_geo = state.clone();
                tokio::spawn(async move { lookup_and_store_geo(state_for_geo, id, ip).await });
                    }
                }
            }
            Ok(())
        }
        AgentFrame::Result { node_id, .. } => {
            if let Some(mut rt) = state.node_rt.get_mut(&node_id) {
                rt.online = true;
                rt.last_seen = now();
            }
            Ok(())
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
    let nonce = uuid::Uuid::new_v4().to_string();
    let ts = now();
    let challenge = serde_json::json!({"type":"challenge","nonce":nonce,"ts":ts});
    send_json(&mut socket, challenge).await;
    let mut node_id: Option<String> = None;
    let mut cipher: Option<SessionCipher> = None;
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
                let mut text = text;
                if let Some(c) = &cipher {
                    if let Ok(mut value) = serde_json::from_str::<Value>(&text) {
                        if value.get("type").and_then(|v| v.as_str()) == Some("enc") {
                            value = c.decrypt(&value).unwrap_or(Value::Null);
                            text = value.to_string();
                        }
                    }
                }
                if let Ok(value) = serde_json::from_str::<Value>(&text) {
                    dispatch_result(&state, &value).await;
                }
                let Ok(frame) = serde_json::from_str::<AgentFrame>(&text) else { continue };
                if let AgentFrame::Hello { node_id: id, proof: client_proof, .. } = &frame {
                    let Some(token) = verify_proof(&state.db, id, &nonce, ts, client_proof).await else {
                        let _ = socket.send(Message::Text(json!({"type":"error","error":"invalid node proof"}).to_string().into())).await;
                        break;
                    };
                    let server_proof = proof(&token, "server", id, &nonce, ts);
                    send_json(&mut socket, json!({"type":"challenge_ok","proof":server_proof})).await;
                    cipher = Some(SessionCipher::new(&token, &nonce, ts));
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
                let payload = match &cipher { Some(c) => c.encrypt(&value), None => value };
                send_json(&mut socket, payload).await;
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
        if let Some(mut rt) = state.node_rt.get_mut(&id) {
            rt.online = false;
            rt.last_seen = now();
        }
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
struct NodeView {
    node_id: String,
    hostname: String,
    version: String,
    networks: Value,
    snapshot: Value,
    last_seen: i64,
    status: String,
    public_ipv4: Option<String>,
    public_ipv6: Option<String>,
    country_code: Option<String>,
    city: Option<String>,
    latitude: Option<f64>,
    longitude: Option<f64>,
    rx_bps: Option<f32>,
    tx_bps: Option<f32>,
    alias: Option<String>,
    cpu_pct: Option<f32>,
    mem_pct: Option<f32>,
    mem_used: Option<i64>,
    mem_total: Option<i64>,
    node_uuid: Option<String>,
    system_info: Value,
    traffic: Value,
    bandwidth_mbps: Option<f32>,
    bandwidth_up_mbps: Option<f32>,
    bandwidth_down_mbps: Option<f32>,
    bandwidth_intranet_down_mbps: Option<f32>,
    bandwidth_internet_down_mbps: Option<f32>,
    shared_listeners: Option<String>,
    instance_ids: Value,
}

async fn nodes_payload(state: &AppState) -> Value {
    // Static inventory (alias, uuid, country, geo, system info, configured
    // bandwidth/listeners) comes from SQL; every realtime field comes from the
    // in-memory runtime registry. Columns are read one by one so a single
    // unknown/legacy column can never blank the whole node list.
    let static_rows = match sqlx::query(&q(
        "SELECT node_id, alias, country_code, node_uuid, city, latitude, longitude, \
                bandwidth_mbps, bandwidth_up_mbps, bandwidth_down_mbps, \
                bandwidth_intranet_down_mbps, bandwidth_internet_down_mbps, \
                shared_listeners, system_info FROM nodes",
    ))
    .fetch_all(&state.db)
    .await
    {
        Ok(rows) => rows,
        Err(err) => {
            tracing::warn!(?err, "node static metadata query failed");
            Vec::new()
        }
    };
    let instance_rows = sqlx::query(&q("SELECT node_id, network, instance_id FROM network_instances"))
        .fetch_all(&state.db)
        .await
        .unwrap_or_default();
    let mut instance_map: std::collections::HashMap<String, Vec<Value>> = std::collections::HashMap::new();
    for row in instance_rows {
        let node_id: String = row.try_get("node_id").unwrap_or_default();
        let network: String = row.try_get("network").unwrap_or_default();
        let instance_id: String = row.try_get("instance_id").unwrap_or_default();
        instance_map.entry(node_id).or_default().push(json!({"network": network, "instance_id": instance_id}));
    }
    struct Static {
        alias: Option<String>,
        country: Option<String>,
        uuid: Option<String>,
        city: Option<String>,
        lat: Option<f64>,
        lon: Option<f64>,
        bw: Option<f32>,
        bw_up: Option<f32>,
        bw_down: Option<f32>,
        bw_intra: Option<f32>,
        bw_inet: Option<f32>,
        listeners: Option<String>,
        system: Value,
    }
    let mut statics: std::collections::BTreeMap<String, Static> = std::collections::BTreeMap::new();
    for row in static_rows {
        let node_id: String = row.try_get("node_id").unwrap_or_default();
        if node_id.is_empty() {
            continue;
        }
        let system_info: String = row.try_get("system_info").unwrap_or_default();
        statics.insert(
            node_id,
            Static {
                alias: row.try_get("alias").ok(),
                country: row.try_get("country_code").ok(),
                uuid: row.try_get("node_uuid").ok(),
                city: row.try_get("city").ok(),
                lat: row.try_get("latitude").ok().flatten(),
                lon: row.try_get("longitude").ok().flatten(),
                bw: row.try_get("bandwidth_mbps").ok().flatten(),
                bw_up: row.try_get("bandwidth_up_mbps").ok().flatten(),
                bw_down: row.try_get("bandwidth_down_mbps").ok().flatten(),
                bw_intra: row.try_get("bandwidth_intranet_down_mbps").ok().flatten(),
                bw_inet: row.try_get("bandwidth_internet_down_mbps").ok().flatten(),
                listeners: row.try_get("shared_listeners").ok().flatten(),
                system: serde_json::from_str(&system_info).unwrap_or(Value::Null),
            },
        );
    }
    let mut ids: std::collections::BTreeSet<String> = statics.keys().cloned().collect();
    ids.extend(state.node_rt.iter().map(|e| e.key().clone()));
    let now_ts = now();
    let mut nodes = Vec::new();
    for node_id in ids {
        let meta = statics.get(&node_id);
        let rt = state.node_rt.get(&node_id).map(|r| r.clone());
        let live = rt.filter(|r| now_ts - r.ts <= RT_FRESH_SECS);
        let channel_online = agent_channel_present(state, &node_id);
        let status = if live.is_some() && channel_online {
            "online".to_string()
        } else {
            "offline".to_string()
        };
        let system_info = match meta.map(|m| m.system.clone()) {
            Some(value) if !value.is_null() => value,
            _ => live.as_ref().map(|r| r.system.clone()).unwrap_or(Value::Null),
        };
        nodes.push(NodeView {
            node_id: node_id.clone(),
            hostname: live.as_ref().map(|r| r.hostname.clone()).unwrap_or_default(),
            version: live.as_ref().map(|r| r.version.clone()).unwrap_or_default(),
            networks: live.as_ref().map(|r| r.networks.clone()).unwrap_or(Value::Null),
            snapshot: live.as_ref().map(|r| r.snapshot.clone()).unwrap_or(Value::Null),
            last_seen: live.as_ref().map(|r| r.last_seen).unwrap_or(0),
            status,
            public_ipv4: live.as_ref().and_then(|r| r.public_ipv4.clone()),
            public_ipv6: live.as_ref().and_then(|r| r.public_ipv6.clone()),
            country_code: meta
                .and_then(|m| m.country.clone())
                .or_else(|| live.as_ref().and_then(|r| r.country_code.clone())),
            city: meta
                .and_then(|m| m.city.clone())
                .or_else(|| live.as_ref().and_then(|r| r.city.clone())),
            latitude: meta
                .and_then(|m| m.lat)
                .or_else(|| live.as_ref().and_then(|r| r.lat)),
            longitude: meta
                .and_then(|m| m.lon)
                .or_else(|| live.as_ref().and_then(|r| r.lon)),
            rx_bps: live.as_ref().map(|r| r.rx_bps as f32),
            tx_bps: live.as_ref().map(|r| r.tx_bps as f32),
            alias: meta.and_then(|m| m.alias.clone()),
            cpu_pct: live.as_ref().map(|r| r.cpu_pct),
            mem_pct: live.as_ref().map(|r| r.mem_pct),
            mem_used: live.as_ref().map(|r| r.mem_used),
            mem_total: live.as_ref().map(|r| r.mem_total),
            node_uuid: meta.and_then(|m| m.uuid.clone()),
            system_info,
            traffic: live.as_ref().map(|r| r.traffic.clone()).unwrap_or(Value::Null),
            bandwidth_mbps: meta.and_then(|m| m.bw),
            bandwidth_up_mbps: meta.and_then(|m| m.bw_up),
            bandwidth_down_mbps: meta.and_then(|m| m.bw_down),
            bandwidth_intranet_down_mbps: meta.and_then(|m| m.bw_intra),
            bandwidth_internet_down_mbps: meta.and_then(|m| m.bw_inet),
            shared_listeners: meta
                .and_then(|m| m.listeners.clone())
                .filter(|v| !v.trim().is_empty())
                .or_else(|| live.as_ref().map(|r| r.listeners.join(",")).filter(|v| !v.is_empty())),
            instance_ids: json!(instance_map.get(&node_id).cloned().unwrap_or_default()),
        });
    }
    json!({"nodes": nodes})
}

/// Lightweight node list for the 1s realtime channel: identical live fields,
/// but the per-node snapshot (full topology) and system info are fetched on
/// demand by the detail endpoint. Keeps the list page smooth without slowing
/// the refresh cadence.
async fn nodes_payload_lite(state: &AppState) -> Value {
    // configured VPN address per (node, network): the list must show the address
    // the console pushed even when a snapshot does not carry it yet
    let version_rows: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT node_id, network, toml FROM config_versions ORDER BY id DESC",
    )
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();
    let mut configured_ip: std::collections::HashMap<(String, String), String> =
        std::collections::HashMap::new();
    for (node_id, network, toml) in version_rows {
        let key = (node_id, network);
        if configured_ip.contains_key(&key) {
            continue;
        }
        if let Ok(value) = toml::from_str::<toml::Value>(&toml) {
            if let Some(ipv4) = value.get("ipv4").and_then(|v| v.as_str()).filter(|v| !v.is_empty()) {
                configured_ip.insert(key, ipv4.to_string());
            }
        }
    }
    let mut full = nodes_payload(state).await;
    let Some(nodes) = full.get_mut("nodes").and_then(|v| v.as_array_mut()) else { return full };
    for node in nodes.iter_mut() {
        let node_id = node.get("node_id").and_then(|v| v.as_str()).unwrap_or_default().to_string();
        let snapshot = node.get("snapshot").cloned().unwrap_or(Value::Null);
        node.as_object_mut().map(|obj| obj.remove("snapshot"));
        node.as_object_mut().map(|obj| obj.remove("system_info"));
        let mut instances: Vec<Value> = Vec::new();
        for inst in snapshot.get("instances").and_then(|v| v.as_array()).into_iter().flatten() {
            let network = inst.get("network_name").and_then(|v| v.as_str()).unwrap_or_default().to_string();
            let topo = inst.get("topology");
            let ipv4 = inst
                .get("ipv4")
                .and_then(|v| v.as_str())
                .map(|v| v.to_string())
                .or_else(|| configured_ip.get(&(node_id.clone(), network.clone())).cloned());
            instances.push(json!({
                "network_name": network,
                "instance_id": inst.get("instance_id"),
                "ipv4": ipv4,
                "ipv6": inst.get("ipv6"),
                "p2p_group": inst.get("p2p_group"),
                "peer_count": topo.and_then(|t| t.get("nodes")).and_then(|v| v.as_array()).map(|v| v.len()).unwrap_or(0),
                "edge_count": topo.and_then(|t| t.get("edges")).and_then(|v| v.as_array()).map(|v| v.len()).unwrap_or(0),
            }));
        }
        // networks that are configured but not reported yet still get an entry so
        // the list can show the assigned VPN IP
        for ((cfg_node, network), ipv4) in configured_ip.iter() {
            if cfg_node != &node_id {
                continue;
            }
            if instances.iter().any(|i| i.get("network_name").and_then(|v| v.as_str()) == Some(network.as_str())) {
                continue;
            }
            instances.push(json!({
                "network_name": network,
                "instance_id": Value::Null,
                "ipv4": ipv4,
                "ipv6": Value::Null,
                "p2p_group": Value::Null,
                "peer_count": 0,
                "edge_count": 0,
            }));
        }
        node["instances"] = json!(instances);
    }
    full
}


#[derive(serde::Deserialize)]
struct AddNodeBody {
    node_id: String,
    #[serde(default)]
    alias: Option<String>,
}

fn valid_node_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

fn random_token() -> String {
    use rand::Rng;
    rand::thread_rng()
        .sample_iter(&rand::distributions::Alphanumeric)
        .take(32)
        .map(char::from)
        .collect::<String>()
        .to_lowercase()
}

async fn add_node(State(state): State<AppState>, Json(body): Json<AddNodeBody>) -> Json<Value> {
    let node_id = body.node_id.trim().to_string();
    if !valid_node_id(&node_id) {
        return Json(json!({"ok": false, "error": "节点 ID 只能包含字母、数字、- _ .，长度 1-64"}));
    }
    let exists: Option<(String,)> = sqlx::query_as(&q("SELECT node_id FROM nodes WHERE node_id = ?"))
        .bind(&node_id)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten();
    if exists.is_some() {
        return Json(json!({"ok": false, "error": "节点 ID 已存在"}));
    }
    let token = random_token();
    let alias = body.alias.as_deref().map(str::trim).filter(|v| !v.is_empty());
    let res = sqlx::query(&q(
        "INSERT INTO nodes (node_id, token_hash, token, alias, status) VALUES (?, ?, ?, ?, 'offline')",
    ))
    .bind(&node_id)
    .bind(hash_token(&token))
    .bind(&token)
    .bind(alias)
    .execute(&state.db)
    .await;
    if res.is_err() {
        return Json(json!({"ok": false, "error": "创建节点失败"}));
    }
    Json(json!({"ok": true, "node_id": node_id, "token": token}))
}

async fn get_node_token(State(state): State<AppState>, Path(node_id): Path<String>) -> Json<Value> {
    let row: Option<(Option<String>,)> = sqlx::query_as(&q("SELECT token FROM nodes WHERE node_id = ?"))
        .bind(&node_id)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten();
    match row.and_then(|(token,)| token) {
        Some(token) => Json(json!({"ok": true, "node_id": node_id, "token": token})),
        None => Json(json!({"ok": false, "error": "节点不存在或没有 token"})),
    }
}

async fn rotate_node_token(State(state): State<AppState>, Path(node_id): Path<String>) -> Json<Value> {
    let exists: Option<(String,)> = sqlx::query_as(&q("SELECT node_id FROM nodes WHERE node_id = ?"))
        .bind(&node_id)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten();
    if exists.is_none() {
        return Json(json!({"ok": false, "error": "节点不存在"}));
    }
    let token = random_token();
    let _ = sqlx::query(&q("UPDATE nodes SET token_hash = ?, token = ? WHERE node_id = ?"))
        .bind(hash_token(&token))
        .bind(&token)
        .bind(&node_id)
        .execute(&state.db)
        .await;
    Json(json!({"ok": true, "node_id": node_id, "token": token}))
}

async fn install_script() -> impl axum::response::IntoResponse {
    (
        [(axum::http::header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        INSTALL_SH,
    )
}

async fn list_nodes(State(state): State<AppState>) -> Json<Value> {
    Json(nodes_payload_lite(&state).await)
}

#[derive(serde::Deserialize)]
#[serde(untagged)]
enum NumOrStr {
    Num(f64),
    Str(String),
    Null,
}

impl NumOrStr {
    fn as_text(&self) -> String {
        match self {
            NumOrStr::Num(v) => v.to_string(),
            NumOrStr::Str(v) => v.clone(),
            NumOrStr::Null => String::new(),
        }
    }

    fn as_number(&self) -> Option<Result<f64, ()>> {
        match self {
            NumOrStr::Num(v) => Some(Ok(*v)),
            NumOrStr::Str(text) => {
                let trimmed = text.trim();
                if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed.parse::<f64>().map_err(|_| ()))
                }
            }
            NumOrStr::Null => None,
        }
    }
}

#[derive(serde::Deserialize)]
struct NodeSettingsBody {
    alias: Option<String>,
    country_code: Option<NumOrStr>,
    latitude: Option<NumOrStr>,
    longitude: Option<NumOrStr>,
    bandwidth_mbps: Option<f32>,
    bandwidth_up_mbps: Option<f32>,
    bandwidth_down_mbps: Option<f32>,
    bandwidth_intranet_down_mbps: Option<f32>,
    bandwidth_internet_down_mbps: Option<f32>,
    shared_listeners: Option<String>,
}

async fn send_shared_listeners(state: &AppState, node_id: &str, listeners: Vec<String>) -> Value {
    let prefix = format!("{node_id}:");
    let key = state.agents.iter().find(|entry| entry.key().starts_with(&prefix)).map(|entry| entry.key().clone()).unwrap_or_default();
    if key.is_empty() {
        return json!({"ok": false, "error": "node is offline"});
    }
    let Some(tx) = state.agents.get(&key).map(|v| v.clone()) else {
        return json!({"ok": false, "error": "node is offline"});
    };
    let command = dokitier_controller::ConsoleCommand::ApplySharedListeners { listeners };
    let request_id = uuid::Uuid::new_v4().to_string();
    let (result_tx, result_rx) = tokio::sync::oneshot::channel();
    state.pending.insert(request_id.clone(), result_tx);
    let frame = json!({"type":"command","request_id":request_id,"command":serde_json::to_value(&command).unwrap_or(Value::Null)});
    if tx.send(frame).await.is_err() {
        state.pending.remove(&request_id);
        return json!({"ok": false, "error": "agent channel closed"});
    }
    match tokio::time::timeout(std::time::Duration::from_secs(20), result_rx).await {
        Ok(Ok(value)) => value,
        Ok(Err(_)) => json!({"ok": false, "error": "agent result channel closed"}),
        Err(_) => { state.pending.remove(&request_id); json!({"ok": false, "error": "agent command timeout"}) }
    }
}

fn spawn_node_geo_refresh(state: AppState, node_id: String) {
    tokio::spawn(async move {
        let row: Option<(Option<String>, Option<String>)> = sqlx::query_as(
            "SELECT public_ipv4, public_ipv6 FROM nodes WHERE node_id = ?",
        )
        .bind(&node_id)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten();
        if let Some((v4, v6)) = row {
            if let Some(ip) = v4.or(v6) {
                lookup_and_store_geo(state, node_id, ip).await;
            }
        }
    });
}

async fn set_node_settings(
    State(state): State<AppState>,
    Path(node_id): Path<String>,
    Json(body): Json<NodeSettingsBody>,
) -> Json<Value> {
    // 1) persist every field first; the core push happens afterwards so the
    // pushed config always carries the values that were just saved
    if let Some(alias) = body.alias.as_deref() {
        let _ = sqlx::query(&q("UPDATE nodes SET alias = ? WHERE node_id = ?"))
            .bind(alias.trim())
            .bind(&node_id)
            .execute(&state.db)
            .await;
    }
    if let Some(country) = body.country_code.as_ref() {
        let code = country.as_text().trim().to_lowercase();
        if code.is_empty() {
            spawn_node_geo_refresh(state.clone(), node_id.clone());
        } else if code.len() == 2 && code.chars().all(|c| c.is_ascii_alphabetic()) {
            let _ = sqlx::query(&q("UPDATE nodes SET country_code = ? WHERE node_id = ?"))
                .bind(&code)
                .bind(&node_id)
                .execute(&state.db)
                .await;
            if let Some(mut rt) = state.node_rt.get_mut(&node_id) {
                rt.country_code = Some(code);
            }
        } else {
            return Json(json!({"ok": false, "error": "国家代码必须是两位字母"}));
        }
    }
    if body.latitude.is_some() || body.longitude.is_some() {
        let parse = |value: &Option<NumOrStr>| -> Result<Option<f64>, ()> {
            let Some(value) = value else { return Ok(None) };
            match value.as_number() {
                None => Ok(None),
                Some(Ok(v)) => Ok(Some(v)),
                Some(Err(())) => Err(()),
            }
        };
        let (Ok(lat), Ok(lon)) = (parse(&body.latitude), parse(&body.longitude)) else {
            return Json(json!({"ok": false, "error": "经纬度必须是数字"}));
        };
        if lat.is_none() && lon.is_none() {
            spawn_node_geo_refresh(state.clone(), node_id.clone());
        } else {
            let _ = sqlx::query(&q("UPDATE nodes SET latitude = COALESCE(?, latitude), longitude = COALESCE(?, longitude) WHERE node_id = ?"))
                .bind(lat)
                .bind(lon)
                .bind(&node_id)
                .execute(&state.db)
                .await;
            if let Some(mut rt) = state.node_rt.get_mut(&node_id) {
                if lat.is_some() {
                    rt.lat = lat;
                }
                if lon.is_some() {
                    rt.lon = lon;
                }
            }
        }
    }
    let previous_caps: Option<(Option<f32>, Option<f32>, Option<f32>, Option<f32>, Option<f32>)> = sqlx::query_as(
        &q("SELECT bandwidth_mbps, bandwidth_up_mbps, bandwidth_down_mbps, bandwidth_intranet_down_mbps, bandwidth_internet_down_mbps FROM nodes WHERE node_id = ?"),
    )
    .bind(&node_id)
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten();
    if let Some(bw) = body.bandwidth_mbps {
        let _ = sqlx::query(&q("UPDATE nodes SET bandwidth_mbps = ?, bandwidth_up_mbps = ?, bandwidth_down_mbps = ? WHERE node_id = ?"))
            .bind(bw.max(0.0) as f32)
            .bind(bw.max(0.0) as f32)
            .bind(bw.max(0.0) as f32)
            .bind(&node_id)
            .execute(&state.db)
            .await;
    }
    if body.bandwidth_up_mbps.is_some() || body.bandwidth_down_mbps.is_some() {
        let _ = sqlx::query(&q("UPDATE nodes SET bandwidth_up_mbps = COALESCE(?, bandwidth_up_mbps), bandwidth_down_mbps = COALESCE(?, bandwidth_down_mbps) WHERE node_id = ?"))
            .bind(body.bandwidth_up_mbps.map(|v| v.max(0.0) as f32))
            .bind(body.bandwidth_down_mbps.map(|v| v.max(0.0) as f32))
            .bind(&node_id)
            .execute(&state.db)
            .await;
    }
    if body.bandwidth_intranet_down_mbps.is_some() || body.bandwidth_internet_down_mbps.is_some() {
        let _ = sqlx::query(&q("UPDATE nodes SET bandwidth_intranet_down_mbps = COALESCE(?, bandwidth_intranet_down_mbps), bandwidth_internet_down_mbps = COALESCE(?, bandwidth_internet_down_mbps) WHERE node_id = ?"))
            .bind(body.bandwidth_intranet_down_mbps.map(|v| v.max(0.0) as f32))
            .bind(body.bandwidth_internet_down_mbps.map(|v| v.max(0.0) as f32))
            .bind(&node_id)
            .execute(&state.db)
            .await;
    }

    // 2) core-level shared listeners: wait for the core callback
    let mut warning: Option<String> = None;
    if let Some(shared) = body.shared_listeners.as_deref() {
        let listeners: Vec<String> = shared
            .split(',')
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(|v| v.to_string())
            .collect();
        let normalized = listeners.join(",");
        let previous: Option<String> = sqlx::query_scalar(&q(
            "SELECT shared_listeners FROM nodes WHERE node_id = ?",
        ))
        .bind(&node_id)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten();
        let _ = sqlx::query(&q("UPDATE nodes SET shared_listeners = ? WHERE node_id = ?"))
            .bind(&normalized)
            .bind(&node_id)
            .execute(&state.db)
            .await;
        if previous.as_deref().unwrap_or("") != normalized {
            let result = send_shared_listeners(&state, &node_id, listeners).await;
            if result.get("ok").and_then(|v| v.as_bool()) != Some(true) {
                let reason = result
                    .get("error")
                    .and_then(|v| v.as_str())
                    .unwrap_or("节点未确认");
                warning = Some(format!("共享监听已保存，但{reason}，节点上线后自动生效"));
            }
        }
    }

    // 3) network config (bandwidth caps are injected into it): wait for the
    // core callback; a rejected config is a real error, offline/timeout is a
    // warning because the values are already persisted
    // only re-apply the network configs when a cap really changed: the UI sends
    // the whole form on every save, and an alias/geo-only save must not restart
    // every network instance on the node
    let (prev_bw, prev_up, prev_down, prev_intra, prev_inet) =
        previous_caps.unwrap_or((None, None, None, None, None));
    let changed = |new: Option<f32>, old: Option<f32>| match new {
        None => false,
        Some(v) => Some(v.max(0.0)) != old.map(|o| o.max(0.0)),
    };
    let needs_push = changed(body.bandwidth_mbps, prev_bw)
        || changed(body.bandwidth_up_mbps, prev_up)
        || changed(body.bandwidth_down_mbps, prev_down)
        || changed(body.bandwidth_intranet_down_mbps, prev_intra)
        || changed(body.bandwidth_internet_down_mbps, prev_inet);
    if needs_push {
        let result = push_node_configs_wait(&state, &node_id).await;
        if result.get("ok").and_then(|v| v.as_bool()) != Some(true) {
            let reason = result
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("core 未应用配置")
                .to_string();
            let transient = reason.contains("offline")
                || reason.contains("离线")
                || reason.contains("超时")
                || reason.contains("timeout")
                || reason.contains("closed")
                || reason.contains("断开");
            if transient {
                warning = Some(format!("配置已保存；{reason}，节点恢复后自动生效"));
            } else {
                return Json(json!({"ok": false, "error": format!("core 拒绝配置：{reason}")}));
            }
        }
    }

    match warning {
        Some(warning) => Json(json!({"ok": true, "warning": warning})),
        None => Json(json!({"ok": true})),
    }
}

#[derive(serde::Deserialize)]
struct AliasBody {
    alias: String,
}

async fn set_alias(State(state): State<AppState>, Path(node_id): Path<String>, Json(body): Json<AliasBody>) -> Json<Value> {
    let alias = body.alias.trim().to_string();
    let _ = sqlx::query(&q("UPDATE nodes SET alias = ? WHERE node_id = ?"))
        .bind(&alias).bind(&node_id).execute(&state.db).await;
    Json(json!({"ok": true, "node_id": node_id, "alias": alias}))
}

async fn delete_node(State(state): State<AppState>, Path(node_id): Path<String>) -> Json<Value> {
    if node_id.trim().is_empty() {
        return Json(json!({"ok": false, "error": "invalid node id"}));
    }
    // close any live agent channel for this node
    let prefix = format!("{node_id}:");
    let keys: Vec<String> = state
        .agents
        .iter()
        .filter(|entry| entry.key().starts_with(&prefix))
        .map(|entry| entry.key().clone())
        .collect();
    for key in keys {
        state.agents.remove(&key);
    }
    let _ = sqlx::query(&q("DELETE FROM config_versions WHERE node_id = ?"))
        .bind(&node_id)
        .execute(&state.db)
        .await;
    let _ = sqlx::query(&q("DELETE FROM network_instances WHERE node_id = ?"))
        .bind(&node_id)
        .execute(&state.db)
        .await;
    let deleted = sqlx::query(&q("DELETE FROM nodes WHERE node_id = ?"))
        .bind(&node_id)
        .execute(&state.db)
        .await
        .map(|r| r.rows_affected())
        .unwrap_or(0);
    state.node_rt.remove(&node_id);
    state.rt_flush.remove(&node_id);
    state.geo_attempts.remove(&node_id);
    if deleted == 0 {
        return Json(json!({"ok": false, "error": "节点不存在"}));
    }
    let _ = sqlx::query(&q("INSERT INTO audit_logs (ts, action, node_id, network, detail) VALUES (?, 'node.delete', ?, '', '')"))
        .bind(now())
        .bind(&node_id)
        .execute(&state.db)
        .await;
    Json(json!({"ok": true, "node_id": node_id}))
}

async fn get_node(State(state): State<AppState>, Path(node_id): Path<String>) -> Json<Value> {
    // same memory-backed payload as the node list; no realtime SQL read
    let payload = nodes_payload(&state).await;
    if let Some(node) = payload
        .get("nodes")
        .and_then(|v| v.as_array())
        .and_then(|nodes| {
            nodes
                .iter()
                .find(|n| n.get("node_id").and_then(|v| v.as_str()) == Some(node_id.as_str()))
                .cloned()
        })
    {
        return Json(node);
    }
    Json(json!({"error": "not found"}))
}

#[derive(serde::Deserialize)]
struct CommandBody {
    command: dokitier_controller::ConsoleCommand,
    #[serde(default)]
    network: Option<String>,
}

fn validate_network_toml(network: &str, toml_text: &str) -> Result<(), String> {
    if network.is_empty()
        || network.len() > 64
        || network == "."
        || network == ".."
        || network.chars().any(|c| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')))
    {
        return Err("invalid network name".into());
    }
    let value: toml::Value = toml::from_str(toml_text).map_err(|e| format!("invalid TOML: {e}"))?;
    let configured = value
        .get("network_identity")
        .and_then(|v| v.get("network_name"))
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    if configured != network {
        return Err(format!("network_name mismatch: request={network:?} config={configured:?}"));
    }
    if let Some(list) = value.get("listeners").and_then(|v| v.as_array()) {
        for item in list {
            let Some(text) = item.as_str() else { return Err("listener must be a string".into()) };
            let url = url::Url::parse(text).map_err(|e| format!("invalid listener {text}: {e}"))?;
            if !matches!(url.scheme(), "tcp" | "udp" | "wg" | "quic" | "ws" | "wss" | "faketcp") {
                return Err(format!("unsupported listener scheme: {}", url.scheme()));
            }
        }
    }
    if let Some(ip) = value.get("ipv4").and_then(|v| v.as_str()) {
        let addr = ip.split('/').next().unwrap_or(ip);
        addr.parse::<std::net::Ipv4Addr>().map_err(|e| format!("invalid ipv4 {ip}: {e}"))?;
    }
    if let Some(bw) = value.get("node_bandwidth_mbps") {
        let Some(mbps) = bw.as_float().or_else(|| bw.as_integer().map(|v| v as f64)) else {
            return Err("node_bandwidth_mbps must be a number".into());
        };
        if mbps <= 0.0 {
            return Err("node_bandwidth_mbps must be greater than 0".into());
        }
    }
    for key in ["ipv6", "ipv6_public_addr_prefix"] {
        if let Some(text) = value.get(key).and_then(|v| v.as_str()) {
            let addr = text.split('/').next().unwrap_or(text);
            addr.parse::<std::net::Ipv6Addr>().map_err(|e| format!("invalid {key} {text}: {e}"))?;
        }
    }
    for key in ["dhcp", "dhcpv6"] {
        let Some(raw) = value.get(key) else { continue };
        if let Some(text) = raw.as_str() {
            let Some((addr, prefix)) = text.split_once('/') else {
                return Err(format!("invalid {key} CIDR: {text}"));
            };
            if key == "dhcp" {
                addr.parse::<std::net::Ipv4Addr>().map_err(|e| format!("invalid {key} CIDR {text}: {e}"))?;
                let prefix: u8 = prefix.parse().map_err(|_| format!("invalid {key} prefix: {text}"))?;
                if prefix > 30 {
                    return Err(format!("{key} CIDR needs usable hosts (prefix <= 30): {text}"));
                }
            }
        } else if raw.is_bool() {
            continue;
        } else {
            return Err(format!("{key} must be a boolean or CIDR string"));
        }
    }
    for key in ["tcp_whitelist", "udp_whitelist"] {
        if let Some(list) = value.get(key).and_then(|v| v.as_array()) {
            for item in list {
                let Some(text) = item.as_str() else { return Err(format!("{key} entries must be strings")) };
                let ok = text.parse::<u16>().is_ok()
                    || text.split_once('-').map(|(a, b)| a.parse::<u16>().is_ok() && b.parse::<u16>().is_ok()).unwrap_or(false);
                if !ok {
                    return Err(format!("invalid {key} port: {text}"));
                }
            }
        }
    }
    if let Some(list) = value.get("exit_nodes").and_then(|v| v.as_array()) {
        for item in list {
            let Some(text) = item.as_str() else { return Err("exit_nodes entries must be strings".into()) };
            text.parse::<std::net::IpAddr>().map_err(|e| format!("invalid exit node {text}: {e}"))?;
        }
    }
    if let Some(chains) = value.get("acl").and_then(|v| v.get("acl_v1")).and_then(|v| v.get("chains")).and_then(|v| v.as_array()) {
        for chain in chains {
            let chain_type = chain.get("chain_type").and_then(|v| v.as_integer()).unwrap_or(1);
            if !(1..=3).contains(&chain_type) {
                return Err(format!("invalid acl chain_type: {chain_type}"));
            }
            let default_action = chain.get("default_action").and_then(|v| v.as_integer()).unwrap_or(0);
            if !(0..=2).contains(&default_action) {
                return Err(format!("invalid acl default_action: {default_action}"));
            }
            for rule in chain.get("rules").and_then(|v| v.as_array()).into_iter().flatten() {
                let action = rule.get("action").and_then(|v| v.as_integer()).unwrap_or(0);
                if !(0..=2).contains(&action) {
                    return Err(format!("invalid acl rule action: {action}"));
                }
                let protocol = rule.get("protocol").and_then(|v| v.as_integer()).unwrap_or(5);
                if !(0..=5).contains(&protocol) {
                    return Err(format!("invalid acl rule protocol: {protocol}"));
                }
                if let Some(ports) = rule.get("ports").and_then(|v| v.as_array()) {
                    for port in ports {
                        let Some(text) = port.as_str() else { return Err("acl ports must be strings".into()) };
                        let ok = text.parse::<u16>().is_ok()
                            || text.split_once('-').map(|(a, b)| a.parse::<u16>().is_ok() && b.parse::<u16>().is_ok()).unwrap_or(false);
                        if !ok {
                            return Err(format!("invalid acl port: {text}"));
                        }
                    }
                }
            }
        }
    }
    if let Some(portal) = value.get("vpn_portal_config") {
        if let Some(listen) = portal.get("wireguard_listen").and_then(|v| v.as_str()) {
            listen
                .parse::<std::net::SocketAddr>()
                .map_err(|e| format!("invalid vpn_portal_config.wireguard_listen {listen}: {e}"))?;
        } else if portal.get("enabled").and_then(|v| v.as_bool()) == Some(true) {
            return Err("vpn_portal_config.wireguard_listen is required when enabled".into());
        }
    }
    if let Some(list) = value.get("port_forward").and_then(|v| v.as_array()) {
        for item in list {
            for key in ["bind_addr", "dst_addr"] {
                let Some(text) = item.get(key).and_then(|v| v.as_str()) else {
                    return Err(format!("port_forward.{key} is required"));
                };
                text.parse::<std::net::SocketAddr>().map_err(|e| format!("invalid port_forward.{key} {text}: {e}"))?;
            }
        }
    }
    Ok(())
}

async fn prepare_node_network_toml(state: &AppState, node_id: &str, network: &str, toml: &str) -> String {
    let instance_id = ensure_instance_id(&state.db, node_id, network).await;
    let mut toml = inject_instance_id(toml, &instance_id);
    toml = inject_instance_name(&toml, network, node_id);
    let bandwidth: Option<(Option<f32>, Option<f32>, Option<f32>, Option<f32>)> = sqlx::query_as(
        &q("SELECT bandwidth_up_mbps, bandwidth_down_mbps, bandwidth_intranet_down_mbps, bandwidth_internet_down_mbps FROM nodes WHERE node_id = ?"),
    )
    .bind(node_id)
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten();
    let acl_chains = load_acl_chains(&state.db).await;
    if let Ok(mut value) = toml::from_str::<toml::Value>(&toml) {
        if let Some(table) = value.as_table_mut() {
            if let Some((up, down, intranet, internet)) = bandwidth {
                if let Some(up) = up { table.insert("node_bandwidth_up_mbps".to_string(), toml::Value::Float(f64::from(up))); }
                if let Some(down) = down { table.insert("node_bandwidth_down_mbps".to_string(), toml::Value::Float(f64::from(down))); }
                if let Some(cap) = intranet { table.insert("node_bandwidth_intranet_down_mbps".to_string(), toml::Value::Float(f64::from(cap))); }
                if let Some(cap) = internet { table.insert("node_bandwidth_internet_down_mbps".to_string(), toml::Value::Float(f64::from(cap))); }
            }
            inject_acl(table, &acl_chains);
            if let Ok(text) = toml::to_string(&value) { toml = text; }
        }
    }
    toml
}

/// Send one console command to a node and wait for the core's callback.
/// Returns the core's result value ({"ok":true,...} / {"ok":false,"error":...}).
async fn send_command_wait(
    state: &AppState,
    node_id: &str,
    network: Option<&str>,
    command: dokitier_controller::ConsoleCommand,
) -> Value {
    let prefix = format!("{node_id}:");
    let key = match network {
        Some(network) => {
            let exact = format!("{node_id}:{network}");
            if state.agents.contains_key(&exact) {
                exact
            } else {
                state
                    .agents
                    .iter()
                    .find(|entry| entry.key().starts_with(&prefix))
                    .map(|entry| entry.key().clone())
                    .unwrap_or_default()
            }
        }
        None => state
            .agents
            .iter()
            .find(|entry| entry.key().starts_with(&prefix))
            .map(|entry| entry.key().clone())
            .unwrap_or_default(),
    };
    if key.is_empty() {
        return json!({"ok": false, "error": "node is offline"});
    }
    let Some(tx) = state.agents.get(&key).map(|v| v.clone()) else {
        return json!({"ok": false, "error": "node is offline"});
    };
    let request_id = uuid::Uuid::new_v4().to_string();
    let (result_tx, result_rx) = tokio::sync::oneshot::channel();
    state.pending.insert(request_id.clone(), result_tx);
    let frame = json!({
        "type": "command",
        "request_id": request_id,
        "command": serde_json::to_value(&command).unwrap_or(Value::Null),
    });
    if tx.send(frame).await.is_err() {
        state.pending.remove(&request_id);
        return json!({"ok": false, "error": "agent channel closed"});
    }
    match tokio::time::timeout(std::time::Duration::from_secs(20), result_rx).await {
        Ok(Ok(value)) => value,
        Ok(Err(_)) => json!({"ok": false, "error": "agent result channel closed"}),
        Err(_) => {
            state.pending.remove(&request_id);
            json!({"ok": false, "error": "core 响应超时"})
        }
    }
}

/// Push every config of this node to its core and wait for the callbacks.
/// Used by settings saves so the UI can report the real apply result.
async fn push_node_configs_wait(state: &AppState, node_id: &str) -> Value {
    let rows = sqlx::query(&q(
        "SELECT network, toml FROM config_versions WHERE node_id = ? ORDER BY created DESC",
    ))
    .bind(node_id)
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();
    let mut latest: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for row in rows {
        let network: String = row.try_get("network").unwrap_or_default();
        let toml: String = row.try_get("toml").unwrap_or_default();
        if !network.is_empty() {
            latest.entry(network).or_insert(toml);
        }
    }
    if latest.is_empty() {
        return json!({"ok": true, "applied": true});
    }
    let mut errors: Vec<String> = Vec::new();
    for (network, toml) in latest {
        let prepared = prepare_node_network_toml(state, node_id, &network, &toml).await;
        let result = send_command_wait(
            state,
            node_id,
            Some(&network),
            dokitier_controller::ConsoleCommand::ApplyNetwork {
                network: network.clone(),
                toml: prepared,
            },
        )
        .await;
        if result.get("ok").and_then(|v| v.as_bool()) != Some(true) {
            let reason = result
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("core 未确认");
            errors.push(format!("{network}: {reason}"));
        }
    }
    if errors.is_empty() {
        json!({"ok": true, "applied": true})
    } else {
        json!({"ok": false, "error": errors.join("; ")})
    }
}

async fn post_command(
    State(state): State<AppState>,
    Path(node_id): Path<String>,
    Json(body): Json<CommandBody>,
) -> Json<Value> {
    let prefix = format!("{node_id}:");
    let find_any = || {
        state
            .agents
            .iter()
            .find(|entry| entry.key().starts_with(&prefix))
            .map(|entry| entry.key().clone())
            .unwrap_or_default()
    };
    let key = match &body.network {
        Some(network) => {
            let exact = format!("{node_id}:{network}");
            if state.agents.contains_key(&exact) { exact } else { find_any() }
        }
        None => find_any(),
    };
    let Some(tx) = state.agents.get(&key).map(|v| v.clone()) else {
        return Json(json!({"ok": false, "error": "node is offline"}));
    };
    match &body.command {
        dokitier_controller::ConsoleCommand::ApplyNetwork { network, toml } => {
            if let Err(err) = validate_network_toml(network, toml) {
                return Json(json!({"ok": false, "error": err}));
            }
        }
        dokitier_controller::ConsoleCommand::ApplyRouting { params } => {
            let value = serde_json::to_value(params).unwrap_or(Value::Null);
            if !routing_params_valid(&value) {
                return Json(json!({"ok": false, "error": "invalid routing params"}));
            }
        }
        dokitier_controller::ConsoleCommand::ApplyConfig { toml } => {
            if let Err(err) = toml::from_str::<toml::Value>(toml) {
                return Json(json!({"ok": false, "error": format!("invalid TOML: {err}")}));
            }
        }
        _ => {}
    }
    let mut command = body.command.clone();
    if let dokitier_controller::ConsoleCommand::ApplyNetwork { network, toml } = &mut command {
        let instance_id = ensure_instance_id(&state.db, &node_id, network).await;
        *toml = inject_instance_id(toml, &instance_id);
        *toml = inject_instance_name(toml, network, &node_id);
        let bandwidth: Option<(Option<f32>, Option<f32>, Option<f32>, Option<f32>)> = sqlx::query_as(
            &q("SELECT bandwidth_up_mbps, bandwidth_down_mbps, bandwidth_intranet_down_mbps, bandwidth_internet_down_mbps FROM nodes WHERE node_id = ?"),
        )
        .bind(&node_id)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten();
        if let Some((up, down, intranet, internet)) = bandwidth {
            if up.is_some() || down.is_some() || intranet.is_some() || internet.is_some() {
                if let Ok(mut value) = toml::from_str::<toml::Value>(toml) {
                    if let Some(table) = value.as_table_mut() {
                        if let Some(up) = up {
                            table
                                .entry("node_bandwidth_up_mbps".to_string())
                                .or_insert(toml::Value::Float(f64::from(up)));
                        }
                        if let Some(down) = down {
                            table
                                .entry("node_bandwidth_down_mbps".to_string())
                                .or_insert(toml::Value::Float(f64::from(down)));
                        }
                        if let Some(cap) = intranet {
                            table
                                .entry("node_bandwidth_intranet_down_mbps".to_string())
                                .or_insert(toml::Value::Float(f64::from(cap)));
                        }
                        if let Some(cap) = internet {
                            table
                                .entry("node_bandwidth_internet_down_mbps".to_string())
                                .or_insert(toml::Value::Float(f64::from(cap)));
                        }
                        if let Ok(text) = toml::to_string(&value) {
                            *toml = text;
                        }
                    }
                }
            }
        }
    }
    let request_id = uuid::Uuid::new_v4().to_string();
    let (result_tx, result_rx) = tokio::sync::oneshot::channel();
    state.pending.insert(request_id.clone(), result_tx);
    let frame = json!({
        "type": "command",
        "request_id": request_id,
        "command": serde_json::to_value(&command).unwrap_or(Value::Null),
    });
    if tx.send(frame).await.is_err() {
        state.pending.remove(&request_id);
        return Json(json!({"ok": false, "error": "agent channel closed"}));
    }
    let (apply_toml, apply_network) = match &command {
        dokitier_controller::ConsoleCommand::ApplyConfig { toml } => (Some(toml.clone()), body.network.clone()),
        dokitier_controller::ConsoleCommand::ApplyNetwork { network, toml } => (Some(toml.clone()), Some(network.clone())),
        _ => (None, None),
    };
    match tokio::time::timeout(std::time::Duration::from_secs(20), result_rx).await {
        Ok(Ok(value)) => {
            if value.get("ok").and_then(|v| v.as_bool()) == Some(true) {
                if let dokitier_controller::ConsoleCommand::DeleteNetwork { network } = &command {
                    let _ = sqlx::query(&q(
                        "DELETE FROM network_instances WHERE node_id = ? AND network = ?",
                    ))
                    .bind(&node_id)
                    .bind(network)
                    .execute(&state.db)
                    .await;
                }
                if let Some(toml) = apply_toml {
                    let _ = sqlx::query(&q(
                        "INSERT INTO config_versions (node_id, network, hash, toml, created) VALUES (?, ?, ?, ?, ?)",
                    ))
                    .bind(&node_id)
                    .bind(apply_network.clone().unwrap_or_default())
                    .bind(hash_token(&toml))
                    .bind(&toml)
                    .bind(now())
                    .execute(&state.db)
                    .await;
                    let _ = sqlx::query(&q("INSERT INTO audit_logs (ts, action, node_id, network, detail) VALUES (?, ?, ?, ?, ?)"))
                        .bind(now())
                        .bind("config.apply")
                        .bind(&node_id)
                        .bind(apply_network.clone().unwrap_or_default())
                        .bind(hash_token(&toml))
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
            let Ok((mut send, mut recv)) = conn.accept_bi().await else { eprintln!("quic stream open failed"); return };
            eprintln!("quic stream accepted");
            let Ok(_start) = read_frame(&mut recv).await else { eprintln!("quic start frame missing"); return; };
            let nonce = uuid::Uuid::new_v4().to_string();
            let ts = now();
            if write_frame(&mut send, &json!({"type":"challenge","nonce":nonce,"ts":ts})).await.is_err() { return; }
            let Ok(hello) = read_frame(&mut recv).await else { return };
            let Ok(frame) = serde_json::from_value::<AgentFrame>(hello) else { return };
            let AgentFrame::Hello { node_id, proof: client_proof, hostname, version, networks, .. } = &frame else { return };
            let Some(token) = verify_proof(&state.db, node_id, &nonce, ts, client_proof).await else {
                eprintln!("quic proof invalid for {node_id}");
                let _ = write_frame(&mut send, &json!({"type":"error","error":"invalid node proof"})).await;
                return;
            };
            eprintln!("quic proof ok for {node_id}");
            let server_proof = proof(&token, "server", node_id, &nonce, ts);
            if write_frame(&mut send, &json!({"type":"challenge_ok","proof":server_proof})).await.is_err() { return; }
            let cipher = SessionCipher::new(&token, &nonce, ts);
            let (tx, mut rx) = tokio::sync::mpsc::channel::<Value>(32);
            let network = networks.first().and_then(|n| n.get("name")).and_then(|v| v.as_str()).unwrap_or("default");
            state.agents.insert(format!("{node_id}:{network}"), tx);
            {
                let mut rt = state.node_rt.entry(node_id.clone()).or_default();
                if !hostname.is_empty() { rt.hostname = hostname.clone(); }
                if !version.is_empty() { rt.version = version.clone(); }
                if !networks.is_empty() { rt.networks = Value::Array(networks.clone()); }
                rt.online = true;
                rt.last_seen = now();
            }
            // run the same hello bookkeeping as the websocket path so quic agents
            // also get their system info / hostname / instance id persisted
            let _ = handle_agent_frame(&state, frame.clone()).await;
            let writer_cipher = SessionCipher::new(&token, &nonce, ts);
            let writer = tokio::spawn(async move {
                while let Some(value) = rx.recv().await {
                    if write_frame(&mut send, &writer_cipher.encrypt(&value)).await.is_err() { break; }
                }
            });
            while let Ok(value) = read_frame(&mut recv).await {
                let value = if value.get("type").and_then(|v| v.as_str()) == Some("enc") {
                    cipher.decrypt(&value).unwrap_or(Value::Null)
                } else { value };
                dispatch_result(&state, &value).await;
                if let Ok(frame) = serde_json::from_value::<AgentFrame>(value) {
                    let _ = handle_agent_frame(&state, frame).await;
                }
            }
            writer.abort();
            let prefix = format!("{node_id}:");
            let keys: Vec<String> = state.agents.iter().filter(|e| e.key().starts_with(&prefix)).map(|e| e.key().clone()).collect();
            for key in keys { state.agents.remove(&key); }
            if let Some(mut rt) = state.node_rt.get_mut(node_id) {
                rt.online = false;
                rt.last_seen = now();
            }
        });
    }
}

/// Persist one node's runtime state to SQL in a short transaction. Runs in a
/// background task with bounded concurrency: nothing here ever blocks the agent
/// websocket, the topology API or any other request (they all read memory).
async fn flush_one_node(state: AppState, node_id: String, rt: NodeRt) -> bool {
    let mut tx = match state.db.begin().await {
        Ok(tx) => tx,
        Err(_) => return false,
    };
    let st = if rt.online { "online" } else { "offline" };
    let res = sqlx::query(&q("UPDATE nodes SET last_seen = ?, status = ?, \
            hostname = COALESCE(NULLIF(?, ''), hostname), \
            version = COALESCE(NULLIF(?, ''), version), \
            networks = COALESCE(?, networks), snapshot = ?, \
            system_info = COALESCE(?, system_info), \
            public_ipv4 = COALESCE(?, public_ipv4), public_ipv6 = COALESCE(?, public_ipv6), \
            cpu_pct = ?, mem_pct = ?, mem_used = ?, mem_total = ?, \
            rx_bps = ?, tx_bps = ?, traffic = ? WHERE node_id = ?"))
        .bind(rt.last_seen)
        .bind(st)
        .bind(rt.hostname.clone())
        .bind(rt.version.clone())
        .bind(if rt.networks.is_null() { None } else { Some(rt.networks.to_string()) })
        .bind(rt.snapshot.to_string())
        .bind(if rt.system.is_null() { None } else { Some(rt.system.to_string()) })
        .bind(rt.public_ipv4.clone())
        .bind(rt.public_ipv6.clone())
        .bind(rt.cpu_pct)
        .bind(rt.mem_pct)
        .bind(rt.mem_used)
        .bind(rt.mem_total)
        .bind(rt.rx_bps)
        .bind(rt.tx_bps)
        .bind(rt.traffic.to_string())
        .bind(&node_id)
        .execute(&mut *tx)
        .await;
    if res.is_err() {
        let _ = tx.rollback().await;
        return false;
    }
    for (network, a, b, a_uuid, b_uuid, ts, latency, loss) in &rt.pending_links {
        let ins = sqlx::query(&q("INSERT INTO link_metrics (network, a, b, a_uuid, b_uuid, ts, latency_ms, loss_pct) VALUES (?, ?, ?, ?, ?, ?, ?, ?)"))
            .bind(network)
            .bind(a)
            .bind(b)
            .bind(a_uuid.clone())
            .bind(b_uuid.clone())
            .bind(ts)
            .bind(latency)
            .bind(loss)
            .execute(&mut *tx)
            .await;
        if ins.is_err() {
            let _ = tx.rollback().await;
            return false;
        }
    }
    tx.commit().await.is_ok()
}

/// Downsample link history and enforce the 30 day retention window.
/// Tier 1: older than 1h -> 60s buckets, tier 2: older than 24h -> 5min,
/// tier 3: older than 7d -> 30min. Rows already on a bucket boundary are left
/// alone so repeated runs stay cheap.
async fn compact_link_metrics(state: &AppState) {
    let now_ts = now();
    let oldest = now_ts - 30 * 24 * 3600;
    // retention: nothing older than 30 days
    let _ = sqlx::query("DELETE FROM link_metrics WHERE ts < ?")
        .bind(oldest)
        .execute(&state.db)
        .await;
    let tiers: [(i64, i64); 3] = [
        (now_ts - 3600, 60),
        (now_ts - 24 * 3600, 300),
        (now_ts - 7 * 24 * 3600, 1800),
    ];
    for (cutoff, step) in tiers {
        type Row = (String, i64, i64, Option<String>, Option<String>, i64, Option<f64>, Option<f64>);
        let rows: Vec<Row> = sqlx::query_as(
            "SELECT network, a, b, MAX(a_uuid), MAX(b_uuid), (ts / ?) * ? AS bucket, AVG(latency_ms), AVG(loss_pct) \
             FROM link_metrics WHERE ts < ? AND ts >= ? AND ts % ? <> 0 \
             GROUP BY network, a, b, bucket",
        )
        .bind(step)
        .bind(step)
        .bind(cutoff)
        .bind(oldest)
        .bind(step)
        .fetch_all(&state.db)
        .await
        .unwrap_or_default();
        if rows.is_empty() {
            continue;
        }
        let Ok(mut tx) = state.db.begin().await else { continue };
        let del = sqlx::query(&q(
            "DELETE FROM link_metrics WHERE ts < ? AND ts >= ? AND ts % ? <> 0",
        ))
        .bind(cutoff)
        .bind(oldest)
        .bind(step)
        .execute(&mut *tx)
        .await;
        if del.is_err() {
            let _ = tx.rollback().await;
            continue;
        }
        let mut ok = true;
        for (network, a, b, a_uuid, b_uuid, bucket, latency, loss) in rows {
            let ins = sqlx::query(&q(
                "INSERT INTO link_metrics (network, a, b, a_uuid, b_uuid, ts, latency_ms, loss_pct) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            ))
            .bind(network)
            .bind(a)
            .bind(b)
            .bind(a_uuid)
            .bind(b_uuid)
            .bind(bucket)
            .bind(latency)
            .bind(loss)
            .execute(&mut *tx)
            .await;
            if ins.is_err() {
                ok = false;
                break;
            }
        }
        if ok {
            let _ = tx.commit().await;
        } else {
            let _ = tx.rollback().await;
        }
    }
}

fn spawn_history_maintenance(state: AppState) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(600)).await;
            compact_link_metrics(&state).await;
        }
    });
}

/// Load persisted geolocation into the runtime registry and resolve any node
/// that still lacks country/city. This keeps flags for nodes that connected in
/// the past (including offline ones) and prevents "UN" right after a restart.
/// Re-run IP geolocation for every node that has a known public address. Used
/// when the provider changes and once on startup when the resolver version is
/// newer than the data already stored.
fn spawn_geo_reresolve_all(state: AppState) {
    tokio::spawn(async move {
        let rows: Vec<(String, Option<String>, Option<String>)> = sqlx::query_as(
            "SELECT node_id, public_ipv4, public_ipv6 FROM nodes WHERE public_ipv4 IS NOT NULL OR public_ipv6 IS NOT NULL",
        )
        .fetch_all(&state.db)
        .await
        .unwrap_or_default();
        for (node_id, v4, v6) in rows {
            if let Some(ip) = v4.or(v6) {
                let state_clone = state.clone();
                tokio::spawn(async move { lookup_and_store_geo(state_clone, node_id, ip).await });
            }
        }
    });
}

fn spawn_geo_backfill(state: AppState) {
    tokio::spawn(async move {
        #[allow(clippy::type_complexity)]
        let rows: Vec<(String, Option<String>, Option<String>, Option<f64>, Option<f64>, Option<String>, Option<String>)> = sqlx::query_as(
            "SELECT node_id, country_code, city, latitude, longitude, public_ipv4, public_ipv6 FROM nodes",
        )
        .fetch_all(&state.db)
        .await
        .unwrap_or_default();
        let stored_version = get_setting(&state.db, "geo_resolver").await;
        let resolve_all = stored_version.as_deref() != Some(GEO_RESOLVER_VERSION);
        if resolve_all {
            let _ = set_setting(&state.db, "geo_resolver", GEO_RESOLVER_VERSION).await;
        }
        for (node_id, country, city, lat, lon, v4, v6) in rows {
            if country.is_some() || city.is_some() {
                let mut rt = state.node_rt.entry(node_id.clone()).or_default();
                if rt.country_code.is_none() {
                    rt.country_code = country.clone();
                }
                if rt.city.is_none() {
                    rt.city = city.clone();
                }
                if rt.lat.is_none() {
                    rt.lat = lat;
                }
                if rt.lon.is_none() {
                    rt.lon = lon;
                }
            }
            if resolve_all || country.is_none() || city.is_none() {
                if let Some(ip) = v4.or(v6) {
                    let state_clone = state.clone();
                    let id = node_id.clone();
                    tokio::spawn(async move { lookup_and_store_geo(state_clone, id, ip).await });
                }
            }
        }
    });
}

fn spawn_snapshot_flusher(state: AppState) {
    tokio::spawn(async move {
        let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(4));
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(10)).await;
            let now_ts = now();
            let due: Vec<(String, NodeRt)> = state
                .node_rt
                .iter()
                .filter(|e| now_ts - state.rt_flush.get(e.key()).map(|v| *v).unwrap_or(0) >= 60)
                .map(|e| (e.key().clone(), e.value().clone()))
                .collect();
            for (node_id, rt) in due {
                let Ok(permit) = semaphore.clone().try_acquire_owned() else {
                    // keep it for the next cycle instead of blocking the tick
                    continue;
                };
                // mark as in-flight so the next cycle does not enqueue it twice
                state.rt_flush.insert(node_id.clone(), now_ts);
                let state_clone = state.clone();
                let id = node_id.clone();
                let flushed_ts = rt.ts;
                tokio::spawn(async move {
                    let _permit = permit;
                    if flush_one_node(state_clone.clone(), id.clone(), rt).await {
                        if let Some(mut live) = state_clone.node_rt.get_mut(&id) {
                            live.pending_links.retain(|row| row.5 > flushed_ts);
                        }
                    } else {
                        // retry in the next cycle
                        state_clone.rt_flush.insert(id, 0);
                    }
                });
            }
        }
    });
}

/// One sampling tick: derive per-tunnel bandwidth from the byte counters in the
/// in-memory snapshots. Used by the background loop and by tests.
fn sample_edge_rates_once(state: &AppState) {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let fresh_before = now() - 30;
        let rows: Vec<(String,)> = state
            .node_rt
            .iter()
            .filter(|e| e.value().ts >= fresh_before)
            .map(|e| (e.value().snapshot.to_string(),))
            .collect();
        let mut seen_keys: std::collections::HashSet<String> = std::collections::HashSet::new();
        for (snapshot,) in rows {
            let Ok(value) = serde_json::from_str::<Value>(&snapshot) else { continue };
            let Some(instances) = value.get("instances").and_then(|v| v.as_array()) else { continue };
            for inst in instances {
                let network = inst.get("network_name").and_then(|v| v.as_str()).unwrap_or_default();
                let Some(edges) = inst.pointer("/topology/edges").and_then(|v| v.as_array()) else { continue };
                for e in edges {
                    let a = e.get("a").and_then(|v| v.as_u64()).unwrap_or(0);
                    let b = e.get("b").and_then(|v| v.as_u64()).unwrap_or(0);
                    let pair = if a < b { (a, b) } else { (b, a) };
                    let Some(tunnels) = e.get("tunnels").and_then(|v| v.as_array()) else { continue };
                    for (idx, tunnel) in tunnels.iter().enumerate() {
                        let ident = tunnel
                            .get("remote_addr")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string())
                            .unwrap_or_else(|| format!("t{}", idx));
                        let rx = tunnel.get("rx_bytes").and_then(|v| v.as_u64()).unwrap_or(0);
                        let tx = tunnel.get("tx_bytes").and_then(|v| v.as_u64()).unwrap_or(0);
                        let key = format!("{}|{}|{}|{}", network, pair.0, pair.1, ident);
                        if !seen_keys.insert(key.clone()) {
                            continue;
                        }
                        // Counters only move when a node reports (every few
                        // seconds), so a 1s sample would otherwise read 0 in
                        // between. Keep the last computed rate until the
                        // counters actually change, and only decay to zero
                        // after a few seconds without any movement.
                        let prev = state.edge_traffic.get(&key).map(|v| *v);
                        match prev {
                            Some((ts, prx, ptx)) if rx > prx || tx > ptx => {
                                let elapsed = ((now_ms - ts) as f64 / 1000.0).max(0.2);
                                let drx = rx.saturating_sub(prx) as f64 / elapsed;
                                let dtx = tx.saturating_sub(ptx) as f64 / elapsed;
                                state.edge_rates.insert(key.clone(), (drx, dtx));
                                state.edge_traffic.insert(key.clone(), (now_ms, rx, tx));
                            }
                            Some((ts, _prx, _ptx)) => {
                                if now_ms - ts > 6000 {
                                    state.edge_rates.insert(key.clone(), (0.0, 0.0));
                                }
                            }
                            None => {
                                state.edge_traffic.insert(key.clone(), (now_ms, rx, tx));
                            }
                        }
                    }
                }
            }
        }
}

fn spawn_edge_rate_sampler(state: AppState) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            sample_edge_rates_once(&state);
        }
    });
}

/// Aggregate the scoped topology into the geographic model the globe renders:
/// one point per city (real lat/lon from the geo API) and one arc per city pair.
fn build_geo_view(node_list: &[Value], edge_list: &[Value]) -> Value {
    let mut peer_city: std::collections::HashMap<u64, (String, f64, f64, String)> = std::collections::HashMap::new();
    let mut cities: std::collections::BTreeMap<String, Value> = std::collections::BTreeMap::new();
    for n in node_list {
        let Some(peer) = n.get("peer_id").and_then(|v| v.as_u64()) else { continue };
        let (Some(city), Some(lat), Some(lon)) = (
            n.get("city").and_then(|v| v.as_str()),
            n.get("lat").and_then(|v| v.as_f64()),
            n.get("lon").and_then(|v| v.as_f64()),
        ) else {
            continue;
        };
        let group = n.get("p2p_group").and_then(|v| v.as_str()).unwrap_or_default().to_string();
        let key = format!("{}|{:.2}|{:.2}", city, lat, lon);
        peer_city.insert(peer, (key.clone(), lat, lon, city.to_string()));
        let entry = cities.entry(key).or_insert_with(|| json!({
            "city": city, "lat": lat, "lon": lon, "nodes": 0, "group": group,
            "members": [],
        }));
        entry["nodes"] = json!(entry["nodes"].as_u64().unwrap_or(0) + 1);
        if let Some(members) = entry["members"].as_array_mut() {
            members.push(json!({
                "peer_id": peer as u64,
                "hostname": n.get("hostname").cloned().unwrap_or(Value::Null),
                "alias": n.get("alias").cloned().unwrap_or(Value::Null),
                "ipv4": n.get("ipv4").cloned().unwrap_or(Value::Null),
                "instance_id": n.get("instance_id").cloned().unwrap_or(Value::Null),
                "group": n.get("p2p_group").cloned().unwrap_or(Value::Null),
            }));
        }
    }
    let mut links: std::collections::BTreeMap<(String, String), Value> = std::collections::BTreeMap::new();
    for e in edge_list {
        let a = e.get("a").and_then(|v| v.as_u64()).unwrap_or(0);
        let b = e.get("b").and_then(|v| v.as_u64()).unwrap_or(0);
        let (Some(ca), Some(cb)) = (peer_city.get(&a), peer_city.get(&b)) else { continue };
        if ca.0 == cb.0 {
            continue;
        }
        let key = if ca.0 < cb.0 { (ca.0.clone(), cb.0.clone()) } else { (cb.0.clone(), ca.0.clone()) };
        let rx = e.get("rx_bps").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let tx = e.get("tx_bps").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let latency = e.get("latency_ms").and_then(|v| v.as_f64());
        let entry = links.entry(key).or_insert_with(|| json!({
            "a_city": ca.3, "a_lat": ca.1, "a_lon": ca.2,
            "b_city": cb.3, "b_lat": cb.1, "b_lon": cb.2,
            "traffic": 0.0, "latency_ms": latency,
        }));
        entry["traffic"] = json!(entry["traffic"].as_f64().unwrap_or(0.0) + rx + tx);
        if let Some(lat) = latency {
            let current = entry["latency_ms"].as_f64();
            entry["latency_ms"] = json!(Some(current.map_or(lat, |c| c.min(lat))));
        }
    }
    json!({
        "cities": cities.values().cloned().collect::<Vec<_>>(),
        "links": links.values().cloned().collect::<Vec<_>>(),
        "has_geo": !cities.is_empty(),
    })
}

async fn topology_payload(state: &AppState, net_filter: Option<&str>, group_filter: Option<&str>) -> Value {
    // realtime data comes exclusively from the in-memory runtime registry
    let fresh_before = now() - 30;
    let rows: Vec<(String, String)> = state
        .node_rt
        .iter()
        .filter(|e| e.value().ts >= fresh_before)
        .map(|e| (e.key().clone(), e.value().snapshot.to_string()))
        .collect();
    // nodes are keyed by (network, peer_id): the same peer exists independently
    // in every network instance and must not collapse into one row.
    let mut nodes: std::collections::BTreeMap<(String, u32), Value> = std::collections::BTreeMap::new();
    let mut edges: std::collections::BTreeMap<(u32, u32, String), Value> = std::collections::BTreeMap::new();
    let mut peer_owner: std::collections::HashMap<u32, String> = std::collections::HashMap::new();
    // p2p group per (node_id, network) from the config the console pushed; the
    // agent snapshot does not carry it, so the topology view resolves it here.
    let version_rows: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT node_id, network, toml FROM config_versions ORDER BY id DESC",
    )
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();
    let mut cv_groups: std::collections::HashMap<(String, String), String> =
        std::collections::HashMap::new();
    for (node_id, network, toml) in version_rows {
        let key = (node_id.clone(), network);
        if cv_groups.contains_key(&key) {
            continue; // rows are newest-first
        }
        if let Ok(value) = toml::from_str::<toml::Value>(&toml) {
            let group = value
                .get("network_identity")
                .and_then(|v| v.get("p2p_group"))
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            cv_groups.insert(key, group);
        }
    }
    // instance uuid -> p2p group: network_instances is the authoritative
    // node/network -> instance_id mapping the console assigns on apply.
    let mut instance_groups: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    let inst_rows: Vec<(String, String, String)> =
        sqlx::query_as("SELECT node_id, network, instance_id FROM network_instances")
            .fetch_all(&state.db)
            .await
            .unwrap_or_default();
    let mut instance_nodes: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for (node_id, network, instance_id) in inst_rows {
        if let Some(group) = cv_groups.get(&(node_id.clone(), network)) {
            if !group.is_empty() {
                instance_groups.insert(instance_id.clone(), group.clone());
            }
        }
        instance_nodes.insert(instance_id, node_id);
    }
    // real geolocation (city + lat/lon) resolved from the node public IP by the
    // geo API; nodes without it simply do not appear on the globe
    let geo_rows: Vec<(String, Option<String>, Option<String>, Option<f64>, Option<f64>)> = sqlx::query_as(
        "SELECT node_id, country_code, city, latitude, longitude FROM nodes",
    )
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();
    let mut node_geo: std::collections::HashMap<String, (String, String, f64, f64)> =
        std::collections::HashMap::new();
    for (node_id, country, city, lat, lon) in geo_rows {
        if let (Some(city), Some(lat), Some(lon)) = (city, lat, lon) {
            node_geo.insert(node_id, (country.unwrap_or_default(), city, lat, lon));
        }
    }
    for (node_id, snapshot) in &rows {
        let Ok(value) = serde_json::from_str::<Value>(snapshot) else { continue };
        for inst in value.get("instances").and_then(|v| v.as_array()).into_iter().flatten() {
            if let Some(pid) = inst.get("peer_id").and_then(|v| v.as_u64()) {
                peer_owner.insert(pid as u32, node_id.clone());
            }
            let network = inst.get("network_name").and_then(|v| v.as_str()).unwrap_or_default();
            let p2p_group = inst.get("p2p_group").and_then(|v| v.as_str()).unwrap_or_default();
            let Some(topo) = inst.get("topology") else { continue };
            for n in topo.get("nodes").and_then(|v| v.as_array()).into_iter().flatten() {
                if let Some(peer_id) = n.get("peer_id").and_then(|v| v.as_u64()) {
                    let local = n.get("local").and_then(|v| v.as_bool()).unwrap_or(false);
                    let mut node = n.clone();
                    node["network_name"] = json!(network);
                    let resolved_group = n
                        .get("instance_id")
                        .and_then(|v| v.as_str())
                        .and_then(|uuid| instance_groups.get(uuid).cloned())
                        .unwrap_or_else(|| if local { p2p_group.to_string() } else { String::new() });
                    node["p2p_group"] = json!(resolved_group);
                    node["agent_node_id"] = json!(node_id);
                    let geo_owner = n
                        .get("instance_id")
                        .and_then(|v| v.as_str())
                        .and_then(|uuid| instance_nodes.get(uuid))
                        .cloned()
                        .unwrap_or_else(|| node_id.clone());
                    if let Some((country, city, lat, lon)) = node_geo.get(&geo_owner) {
                        node["country_code"] = json!(country);
                        node["city"] = json!(city);
                        node["lat"] = json!(lat);
                        node["lon"] = json!(lon);
                    }
                    match nodes.entry((network.to_string(), peer_id as u32)) {
                        std::collections::btree_map::Entry::Occupied(mut e) => {
                            if local && !resolved_group.is_empty() {
                                e.get_mut()["p2p_group"] = json!(resolved_group);
                            }
                        }
                        std::collections::btree_map::Entry::Vacant(v) => {
                            v.insert(node);
                        }
                    }
                }
            }
            for e in topo.get("edges").and_then(|v| v.as_array()).into_iter().flatten() {
                let a = e.get("a").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                let b = e.get("b").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                let key = (a.min(b), a.max(b), network.to_string());
                if let Some(existing) = edges.get_mut(&key) {
                    let existing_probe = existing
                        .get("tunnels")
                        .and_then(|v| v.as_array())
                        .map(|tunnels| tunnels.iter().any(|t| t.get("capacity_source").and_then(|v| v.as_str()) == Some("probe")))
                        .unwrap_or(false);
                    let new_probe = e
                        .get("tunnels")
                        .and_then(|v| v.as_array())
                        .map(|tunnels| tunnels.iter().any(|t| t.get("capacity_source").and_then(|v| v.as_str()) == Some("probe")))
                        .unwrap_or(false);
                    if new_probe && !existing_probe {
                        let mut replacement = e.clone();
                        replacement["network_name"] = json!(network);
                        let tunnel_types: Vec<Value> = replacement
                            .get("tunnels")
                            .and_then(|v| v.as_array())
                            .into_iter()
                            .flatten()
                            .filter_map(|t| t.get("type").cloned())
                            .collect();
                        replacement["tunnel_types"] = json!(tunnel_types);
                        *existing = replacement;
                    }
                    continue;
                }
                let mut edge = e.clone();
                edge["network_name"] = json!(network);
                let tunnel_types: Vec<Value> = e.get("tunnels").and_then(|v| v.as_array()).into_iter().flatten()
                    .filter_map(|t| t.get("type").cloned()).collect();
                edge["tunnel_types"] = json!(tunnel_types);
                edges.insert(key, edge);
            }
        }
    }
    let meta_rows: Vec<(String, Option<String>, Option<String>)> =
        sqlx::query_as("SELECT node_id, alias, country_code FROM nodes")
            .fetch_all(&state.db)
            .await
            .unwrap_or_default();
    let mut aliases: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let mut countries: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for (id, alias, country) in meta_rows {
        if let Some(alias) = alias.filter(|a| !a.is_empty()) {
            aliases.insert(id.clone(), alias);
        }
        if let Some(country) = country.filter(|c| !c.is_empty()) {
            countries.insert(id, country);
        }
    }
    for (pid, owner) in &peer_owner {
        for ((_net, node_pid), node) in nodes.iter_mut() {
            if node_pid != pid { continue; }
            if let Some(alias) = aliases.get(owner) {
                node["alias"] = json!(alias);
            }
            if let Some(country) = countries.get(owner) {
                node["country_code"] = json!(country);
            }
        }
    }
    for ((a, b, network), edge) in edges.iter_mut() {
        let mut edge_rx = 0.0f64;
        let mut edge_tx = 0.0f64;
        if let Some(tunnels) = edge.get_mut("tunnels").and_then(|v| v.as_array_mut()) {
            for (idx, tunnel) in tunnels.iter_mut().enumerate() {
                let ident = tunnel
                    .get("remote_addr")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| format!("t{}", idx));
                let key = format!("{}|{}|{}|{}", network, a, b, ident);
                let (rx_bps, tx_bps) = state.edge_rates.get(&key).map(|v| *v).unwrap_or((0.0, 0.0));
                edge_rx += rx_bps;
                edge_tx += tx_bps;
                tunnel["rx_bps"] = json!(rx_bps);
                tunnel["tx_bps"] = json!(tx_bps);
            }
        }
        edge["rx_bps"] = json!(edge_rx);
        edge["tx_bps"] = json!(edge_tx);
        let tunnels = edge.get("tunnels").and_then(|v| v.as_array()).cloned().unwrap_or_default();
        let lat = tunnels
            .iter()
            .filter_map(|t| t.get("latency_ms").and_then(|v| v.as_f64()))
            .fold(f64::INFINITY, f64::min);
        let loss_vals: Vec<f64> = tunnels
            .iter()
            .filter_map(|t| t.get("loss_pct").and_then(|v| v.as_f64()))
            .collect();
        edge["latency_ms"] = if lat.is_finite() { json!(lat) } else { Value::Null };
        edge["loss_pct"] = if loss_vals.is_empty() {
            Value::Null
        } else {
            json!(loss_vals.iter().sum::<f64>() / loss_vals.len() as f64)
        };
        edge["bandwidth_bps"] = json!(edge_rx + edge_tx);
    }

    // Explicit group summary so the topology API is self-describing: the web
    // UI (and any other client) can render one region per p2p group without
    // inferring it from node fields.
    let mut groups: std::collections::BTreeMap<String, Vec<Value>> =
        std::collections::BTreeMap::new();
    for node in nodes.values() {
        let name = node
            .get("p2p_group")
            .and_then(|v| v.as_str())
            .filter(|g| !g.is_empty())
            .unwrap_or("ungrouped")
            .to_string();
        groups
            .entry(name)
            .or_default()
            .push(json!({
                "peer_id": node.get("peer_id").cloned().unwrap_or(Value::Null),
                "hostname": node.get("hostname").cloned().unwrap_or(Value::Null),
            }));
    }
    let groups: Vec<Value> = groups
        .into_iter()
        .map(|(name, members)| {
            json!({"name": name, "count": members.len(), "members": members})
        })
        .collect();
    let mut node_list: Vec<Value> = nodes.values().cloned().collect();
    let mut edge_list: Vec<Value> = edges.values().cloned().collect();
    let mut cross_links: Vec<Value> = Vec::new();
    let peer_group: std::collections::HashMap<u64, String> = node_list
        .iter()
        .filter_map(|n| {
            let id = n.get("peer_id").and_then(|v| v.as_u64())?;
            let g = n.get("p2p_group").and_then(|v| v.as_str()).unwrap_or("").to_string();
            Some((id, g))
        })
        .collect();
    if net_filter.is_some() || group_filter.is_some() {
        let allowed: std::collections::HashSet<(String, u64)> = node_list
            .iter()
            .filter(|n| {
                let net_ok = net_filter
                    .map(|f| n.get("network_name").and_then(|v| v.as_str()) == Some(f))
                    .unwrap_or(true);
                let grp_ok = group_filter
                    .map(|f| n.get("p2p_group").and_then(|v| v.as_str()) == Some(f))
                    .unwrap_or(true);
                net_ok && grp_ok
            })
            .filter_map(|n| {
                let id = n.get("peer_id").and_then(|v| v.as_u64())?;
                let net = n.get("network_name").and_then(|v| v.as_str()).unwrap_or("").to_string();
                Some((net, id))
            })
            .collect();
        if group_filter.is_some() {
            let mut seen: std::collections::HashSet<(u64, String)> = std::collections::HashSet::new();
            for e in &edge_list {
                let a = e.get("a").and_then(|v| v.as_u64()).unwrap_or(0);
                let b = e.get("b").and_then(|v| v.as_u64()).unwrap_or(0);
                let net = e.get("network_name").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let in_a = allowed.contains(&(net.clone(), a));
                let in_b = allowed.contains(&(net, b));
                if in_a == in_b {
                    continue;
                }
                let (inner, outer) = if in_a { (a, b) } else { (b, a) };
                let target = peer_group.get(&outer).cloned().unwrap_or_default();
                if !target.is_empty() && seen.insert((inner, target.clone())) {
                    cross_links.push(json!({"peer_id": inner, "group": target}));
                }
            }
        }
        node_list.retain(|n| {
            let Some(id) = n.get("peer_id").and_then(|v| v.as_u64()) else { return false };
            let net = n.get("network_name").and_then(|v| v.as_str()).unwrap_or("").to_string();
            allowed.contains(&(net, id))
        });
        edge_list.retain(|e| {
            let a = e.get("a").and_then(|v| v.as_u64()).unwrap_or(0);
            let b = e.get("b").and_then(|v| v.as_u64()).unwrap_or(0);
            let net = e.get("network_name").and_then(|v| v.as_str()).unwrap_or("").to_string();
            if !allowed.contains(&(net.clone(), a)) || !allowed.contains(&(net, b)) {
                return false;
            }
            net_filter
                .map(|f| e.get("network_name").and_then(|v| v.as_str()) == Some(f))
                .unwrap_or(true)
        });
    }
    if net_filter.is_none() {
        // aggregate view: one node per peer, one edge per pair (summed rates)
        let mut merged_nodes: std::collections::BTreeMap<u64, Value> = std::collections::BTreeMap::new();
        for n in node_list {
            let Some(pid) = n.get("peer_id").and_then(|v| v.as_u64()) else { continue };
            let net = n.get("network_name").and_then(|v| v.as_str()).unwrap_or("").to_string();
            match merged_nodes.entry(pid) {
                std::collections::btree_map::Entry::Occupied(mut e) => {
                    let node = e.get_mut();
                    let mut nets = node.get("networks").and_then(|v| v.as_array()).cloned().unwrap_or_default();
                    if !net.is_empty() && !nets.iter().any(|x| x.as_str() == Some(net.as_str())) {
                        nets.push(json!(net));
                    }
                    node["networks"] = json!(nets);
                }
                std::collections::btree_map::Entry::Vacant(v) => {
                    let mut node = n;
                    node["networks"] = json!([net]);
                    v.insert(node);
                }
            }
        }
        let mut merged_edges: std::collections::BTreeMap<(u64, u64), Value> = std::collections::BTreeMap::new();
        for e in edge_list {
            let a = e.get("a").and_then(|v| v.as_u64()).unwrap_or(0);
            let b = e.get("b").and_then(|v| v.as_u64()).unwrap_or(0);
            let key = if a < b { (a, b) } else { (b, a) };
            let net = e.get("network_name").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let rx = e.get("rx_bps").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let tx = e.get("tx_bps").and_then(|v| v.as_f64()).unwrap_or(0.0);
            match merged_edges.entry(key) {
                std::collections::btree_map::Entry::Occupied(mut o) => {
                    let edge = o.get_mut();
                    let erx = edge.get("rx_bps").and_then(|v| v.as_f64()).unwrap_or(0.0) + rx;
                    let etx = edge.get("tx_bps").and_then(|v| v.as_f64()).unwrap_or(0.0) + tx;
                    edge["rx_bps"] = json!(erx);
                    edge["tx_bps"] = json!(etx);
                    edge["bandwidth_bps"] = json!(erx + etx);
                    let mut nets = edge.get("networks").and_then(|v| v.as_array()).cloned().unwrap_or_default();
                    if !net.is_empty() && !nets.iter().any(|x| x.as_str() == Some(net.as_str())) {
                        nets.push(json!(net));
                    }
                    edge["networks"] = json!(nets);
                }
                std::collections::btree_map::Entry::Vacant(v) => {
                    let mut edge = e;
                    edge["networks"] = json!([net]);
                    v.insert(edge);
                }
            }
        }
        node_list = merged_nodes.into_values().collect();
        edge_list = merged_edges.into_values().collect();
    }
    let geo = build_geo_view(&node_list, &edge_list);
    json!({"graph": {
        "nodes": node_list,
        "edges": edge_list,
        "groups": groups,
        "cross_links": cross_links,
    },
    "geo": geo})
}

#[derive(serde::Deserialize, Default)]
struct TopologyQuery {
    #[serde(default)]
    network: Option<String>,
    #[serde(default)]
    group: Option<String>,
}

async fn board_topology(
    State(state): State<AppState>,
    axum::extract::Query(query): axum::extract::Query<TopologyQuery>,
) -> Json<Value> {
    let net = query.network.filter(|s| !s.is_empty());
    let grp = query.group.filter(|s| !s.is_empty());
    Json(topology_payload(&state, net.as_deref(), grp.as_deref()).await)
}

/// Realtime topology frame: the HTTP endpoint and the SSE stream must carry the
/// exact same model (graph + geo) or the globe loses its data on the next tick.
fn topology_event_json(payload: &Value) -> String {
    json!({
        "type": "topology",
        "graph": payload.get("graph").cloned().unwrap_or(Value::Null),
        "geo": payload.get("geo").cloned().unwrap_or(Value::Null),
    })
    .to_string()
}

async fn board_events(
    State(state): State<AppState>,
    axum::extract::Query(query): axum::extract::Query<TopologyQuery>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let net = query.network.filter(|s| !s.is_empty());
    let grp = query.group.filter(|s| !s.is_empty());
    let stream = futures::stream::unfold((state, net, grp), |(state, net, grp)| async move {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        let payload = topology_payload(&state, net.as_deref(), grp.as_deref()).await;
        let nodes = nodes_payload_lite(&state).await;
        // the realtime frame must carry the same model as the HTTP endpoint,
        // including the geo aggregation the globe renders
        let topology = Event::default().event("topology").data(topology_event_json(&payload));
        let nodes = Event::default().event("nodes").data(
            json!({"type": "nodes", "nodes": nodes.get("nodes").cloned().unwrap_or(Value::Null)}).to_string(),
        );
        Some((
            futures::stream::iter(vec![Ok::<_, Infallible>(topology), Ok(nodes)]),
            (state, net, grp),
        ))
    }).flatten();
    Sse::new(stream).keep_alive(KeepAlive::default())
}

#[derive(serde::Deserialize)]
struct HistoryQuery {
    network: String,
    a: i64,
    b: i64,
    #[serde(default = "default_range")]
    range: String,
}

fn default_range() -> String { "24h".to_string() }

fn resolve_peer_uuid(state: &AppState, network: &str, peer: i64) -> Option<String> {
    let now_ts = now();
    let rows: Vec<String> = state
        .node_rt
        .iter()
        .filter(|e| now_ts - e.value().ts <= 120)
        .map(|e| e.value().snapshot.to_string())
        .collect();
    for snapshot in rows {
        let Ok(value) = serde_json::from_str::<Value>(&snapshot) else { continue };
        let Some(instances) = value.get("instances").and_then(|v| v.as_array()) else { continue };
        for inst in instances {
            if inst.get("network_name").and_then(|v| v.as_str()) != Some(network) {
                continue;
            }
            let Some(nodes) = inst.pointer("/topology/nodes").and_then(|v| v.as_array()) else { continue };
            for node in nodes {
                if node.get("peer_id").and_then(|v| v.as_i64()) != Some(peer) {
                    continue;
                }
                if let Some(uuid) = node.get("instance_id").and_then(|v| v.as_str()) {
                    if !uuid.is_empty() {
                        return Some(uuid.to_string());
                    }
                }
            }
        }
    }
    None
}

async fn board_history(State(state): State<AppState>, axum::extract::Query(query): axum::extract::Query<HistoryQuery>) -> Json<Value> {
    let (step, since) = match query.range.as_str() {
        "60s" => (1, now() - 60),
        "1h" => (300, now() - 3600),
        "7d" => (3600, now() - 7 * 24 * 3600),
        "30d" => (7200, now() - 30 * 24 * 3600),
        _ => (600, now() - 24 * 3600),
    };
    let a_uuid = resolve_peer_uuid(&state, &query.network, query.a);
    let b_uuid = resolve_peer_uuid(&state, &query.network, query.b);
    let rows: Result<Vec<(i64, Option<f64>, Option<f64>)>, sqlx::Error> = match (a_uuid, b_uuid) {
        (Some(ua), Some(ub)) => {
            let (lo, hi) = if ua <= ub { (ua, ub) } else { (ub, ua) };
            sqlx::query_as(&q(
                "SELECT (ts / ?) * ? AS bucket, AVG(latency_ms), AVG(loss_pct) FROM link_metrics WHERE network = ? AND a_uuid = ? AND b_uuid = ? AND ts > ? GROUP BY bucket ORDER BY bucket LIMIT 4000",
            )).bind(step).bind(step).bind(&query.network).bind(&lo).bind(&hi).bind(since)
              .fetch_all(&state.db).await
        }
        _ => {
            sqlx::query_as(&q(
                "SELECT (ts / ?) * ? AS bucket, AVG(latency_ms), AVG(loss_pct) FROM link_metrics WHERE network = ? AND a = ? AND b = ? AND ts > ? GROUP BY bucket ORDER BY bucket LIMIT 4000",
            )).bind(step).bind(step).bind(&query.network).bind(query.a).bind(query.b).bind(since)
              .fetch_all(&state.db).await
        }
    };
    if let Err(err) = &rows { tracing::warn!(?err, "history query failed"); }
    let rows = rows.unwrap_or_default();
    Json(json!({"points": rows.into_iter().map(|(ts, latency, loss)| json!({
        "ts": ts, "latency_ms": latency, "loss_pct": loss,
    })).collect::<Vec<_>>()}))
}

async fn board_routes(State(state): State<AppState>) -> Json<Value> {
    let rows: Vec<String> = rt_snapshot_strings(&state);
    let mut routes = Vec::new();
    for snapshot in &rows {
        let Ok(value) = serde_json::from_str::<Value>(snapshot) else { continue };
        for inst in value.get("instances").and_then(|v| v.as_array()).into_iter().flatten() {
            let network = inst.get("network_name").and_then(|v| v.as_str()).unwrap_or_default();
            let Some(topo) = inst.get("topology") else { continue };
            for n in topo.get("nodes").and_then(|v| v.as_array()).into_iter().flatten() {
                if n.get("local").and_then(|v| v.as_bool()) == Some(true) { continue; }
                routes.push(json!({
                    "network": network,
                    "peer_id": n.get("peer_id"),
                    "hostname": n.get("hostname"),
                    "ipv4": n.get("ipv4"),
                    "next_hop_peer_id": n.get("next_hop_peer_id"),
                    "cost": n.get("cost"),
                }));
            }
        }
    }
    Json(json!({"routes": routes}))
}

async fn board_links(State(state): State<AppState>) -> Json<Value> {
    let rows: Vec<String> = rt_snapshot_strings(&state);
    let mut names: std::collections::BTreeMap<u32, String> = std::collections::BTreeMap::new();
    let mut links_map: std::collections::BTreeMap<(u32, u32, String), Value> = std::collections::BTreeMap::new();
    for snapshot in &rows {
        let Ok(value) = serde_json::from_str::<Value>(snapshot) else { continue };
        for inst in value.get("instances").and_then(|v| v.as_array()).into_iter().flatten() {
            let network = inst.get("network_name").and_then(|v| v.as_str()).unwrap_or_default();
            let Some(topo) = inst.get("topology") else { continue };
            for n in topo.get("nodes").and_then(|v| v.as_array()).into_iter().flatten() {
                if let (Some(id), Some(host)) = (n.get("peer_id").and_then(|v| v.as_u64()), n.get("hostname").and_then(|v| v.as_str())) {
                    names.entry(id as u32).or_insert_with(|| host.to_string());
                }
            }
            for e in topo.get("edges").and_then(|v| v.as_array()).into_iter().flatten() {
                let tunnels = e.get("tunnels").and_then(|v| v.as_array()).cloned().unwrap_or_default();
                let latency = tunnels.iter().filter_map(|t| t.get("latency_ms").and_then(|v| v.as_f64())).reduce(f64::min);
                let loss = tunnels.iter().filter_map(|t| t.get("loss_pct").and_then(|v| v.as_f64())).reduce(|a, b| a + b).map(|v| v / tunnels.len().max(1) as f64);
                let a = e.get("a").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                let b = e.get("b").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                let key = (a.min(b), a.max(b), network.to_string());
                links_map.entry(key).or_insert(json!({
                    "network": network,
                    "a": e.get("a"), "b": e.get("b"),
                    "tunnel_types": tunnels.iter().filter_map(|t| t.get("type").cloned()).collect::<Vec<_>>(),
                    "latency_ms": latency, "loss_pct": loss,
                    "rx_bps": 0, "tx_bps": 0, "bandwidth_mbps": null,
                    "metrics": {
                        "a_peer_id": a, "b_peer_id": b,
                        "latency_ms": latency, "loss_pct": loss,
                        "samples": [],
                    },
                }));
            }
        }
    }
    Json(json!({"links": links_map.values().cloned().collect::<Vec<_>>(), "names": names}))
}

async fn board_overview(State(state): State<AppState>) -> Json<Value> {
    let now_ts = now();
    let mut rows: Vec<(bool, Value)> = Vec::new();
    for e in state.node_rt.iter() {
        let rt = e.value();
        let fresh = now_ts - rt.ts <= RT_FRESH_SECS;
        rows.push((fresh && rt.online, rt.snapshot.clone()));
    }
    let total = rows.len();
    let online = rows.iter().filter(|(online, _)| *online).count();
    let mut networks: std::collections::BTreeMap<String, Value> = std::collections::BTreeMap::new();
    for (_, value) in &rows {
        let value = value.clone();
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

async fn ensure_admin(db: &AnyPool) -> anyhow::Result<()> {
    use argon2::{Argon2, PasswordHasher, password_hash::SaltString};
    use rand::Rng;
    let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM users").fetch_one(db).await?;
    if count.0 > 0 { return Ok(()); }
    let password: String = rand::thread_rng()
        .sample_iter(&rand::distributions::Alphanumeric)
        .take(18)
        .map(char::from)
        .collect();
    let salt = SaltString::generate(&mut rand::thread_rng());
    let hash = Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map_err(|e| anyhow::anyhow!("argon2: {e}"))?
        .to_string();
    sqlx::query(&q("INSERT INTO users (username, password_hash, role, created) VALUES (?, ?, ?, ?)"))
        .bind("admin").bind(hash).bind("admin").bind(now())
        .execute(db).await?;
    tracing::warn!("==============================================");
    tracing::warn!("dokitier console initial admin account");
    tracing::warn!("username: admin");
    tracing::warn!("password: {password}");
    tracing::warn!("==============================================");
    eprintln!("dokitier console initial admin: admin / {password}");
    Ok(())
}

#[derive(Deserialize)]
struct LoginBody {
    username: String,
    password: String,
    #[serde(default)]
    totp: Option<String>,
}

fn base32_decode(text: &str) -> Vec<u8> {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let mut bits = 0u32;
    let mut value = 0u32;
    let mut out = Vec::new();
    for ch in text.chars().map(|c| c.to_ascii_uppercase()).filter(|c| *c != '=' && !c.is_whitespace()) {
        let Some(index) = ALPHABET.iter().position(|b| *b as char == ch) else { continue };
        value = (value << 5) | index as u32;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push((value >> bits) as u8);
        }
    }
    out
}

fn base32_encode(data: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let mut out = String::new();
    let mut bits = 0u32;
    let mut value = 0u32;
    for byte in data {
        value = (value << 8) | *byte as u32;
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(ALPHABET[((value >> bits) & 31) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(ALPHABET[((value << (5 - bits)) & 31) as usize] as char);
    }
    out
}

fn totp_code(secret: &str, counter: u64) -> String {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    let key = base32_decode(secret);
    let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(&key) else { return String::new() };
    mac.update(&counter.to_be_bytes());
    let digest = mac.finalize().into_bytes();
    let offset = (digest[digest.len() - 1] & 0x0f) as usize;
    let code = ((digest[offset] as u32 & 0x7f) << 24)
        | ((digest[offset + 1] as u32) << 16)
        | ((digest[offset + 2] as u32) << 8)
        | digest[offset + 3] as u32;
    format!("{:06}", code % 1_000_000)
}

fn totp_verify(secret: &str, code: &str) -> bool {
    let code = code.trim();
    if code.len() != 6 || !code.chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    let counter = now() as u64 / 30;
    (-1i64..=1).any(|offset| {
        let value = (counter as i64 + offset).max(0) as u64;
        totp_code(secret, value) == code
    })
}

#[derive(Deserialize)]
struct AccountUpdateBody {
    current_password: String,
    #[serde(default)]
    new_username: Option<String>,
    #[serde(default)]
    new_password: Option<String>,
}

fn session_username(state: &AppState, headers: &axum::http::HeaderMap) -> Option<String> {
    let token = session_token(headers)?;
    state.sessions.get(&token).map(|entry| entry.value().clone())
}

async fn get_account(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> Json<Value> {
    let username = session_username(&state, &headers).unwrap_or_default();
    let role: Option<String> = sqlx::query_scalar(&q("SELECT role FROM users WHERE username = ?"))
        .bind(&username)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten();
    Json(json!({"username": username, "role": role}))
}

async fn update_account(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(body): Json<AccountUpdateBody>,
) -> Response {
    use argon2::{
        Argon2, PasswordHasher, PasswordVerifier,
        password_hash::{PasswordHash, SaltString},
    };
    let Some(username) = session_username(&state, &headers) else {
        return (StatusCode::UNAUTHORIZED, Json(json!({"error": "login required"}))).into_response();
    };
    let hash: Option<String> =
        sqlx::query_scalar(&q("SELECT password_hash FROM users WHERE username = ?"))
            .bind(&username)
            .fetch_optional(&state.db)
            .await
            .ok()
            .flatten();
    let Some(hash) = hash else {
        return (StatusCode::UNAUTHORIZED, Json(json!({"error": "login required"}))).into_response();
    };
    let verified = PasswordHash::new(&hash)
        .ok()
        .and_then(|parsed| {
            Argon2::default()
                .verify_password(body.current_password.as_bytes(), &parsed)
                .ok()
        })
        .is_some();
    if !verified {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "当前密码不正确"})),
        )
            .into_response();
    }
    let mut username_after = username.clone();
    if let Some(candidate) = body.new_username.as_deref().map(str::trim).filter(|v| !v.is_empty()) {
        if candidate != username {
            if candidate.len() > 64
                || !candidate
                    .chars()
                    .all(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.'))
            {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"error": "用户名只能包含字母、数字、-、_、.，且不超过 64 个字符"})),
                )
                    .into_response();
            }
            let taken: Option<String> =
                sqlx::query_scalar(&q("SELECT username FROM users WHERE username = ?"))
                    .bind(candidate)
                    .fetch_optional(&state.db)
                    .await
                    .ok()
                    .flatten();
            if taken.is_some() {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"error": "用户名已被占用"})),
                )
                    .into_response();
            }
            username_after = candidate.to_string();
        }
    }
    let mut new_hash: Option<String> = None;
    if let Some(password) = body.new_password.as_deref().filter(|v| !v.is_empty()) {
        if password.chars().count() < 6 {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "新密码长度至少 6 位"})),
            )
                .into_response();
        }
        let salt = SaltString::generate(&mut rand::thread_rng());
        match Argon2::default().hash_password(password.as_bytes(), &salt) {
            Ok(hash) => new_hash = Some(hash.to_string()),
            Err(err) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"error": format!("密码哈希失败: {err}")})),
                )
                    .into_response();
            }
        }
    }
    let update = sqlx::query(&q(
        "UPDATE users SET username = ?, password_hash = COALESCE(?, password_hash) WHERE username = ?",
    ))
    .bind(&username_after)
    .bind(new_hash.as_deref())
    .bind(&username)
    .execute(&state.db)
    .await;
    if let Err(err) = update {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("保存失败: {err}")})),
        )
            .into_response();
    }
    if username_after != username {
        // Keep existing sessions valid after a rename.
        for mut entry in state.sessions.iter_mut() {
            if entry.value() == &username {
                *entry.value_mut() = username_after.clone();
            }
        }
    }
    Json(json!({"ok": true, "username": username_after})).into_response()
}

async fn auth_status(State(state): State<AppState>) -> Json<Value> {
    let totp: Option<String> = sqlx::query_scalar("SELECT v FROM settings WHERE k = 'security_totp'")
        .fetch_optional(&state.db).await.ok().flatten();
    Json(json!({"totp_enabled": totp.as_deref() == Some("1")}))
}

async fn login(
    State(state): State<AppState>,
    Json(body): Json<LoginBody>,
) -> axum::response::Response {
    use argon2::{Argon2, PasswordVerifier, password_hash::PasswordHash};
    let row: Option<(String, String)> = sqlx::query_as(&q(
        "SELECT password_hash, role FROM users WHERE username = ?",
    )).bind(&body.username).fetch_optional(&state.db).await.ok().flatten();
    let Some((hash, role)) = row else {
        return (axum::http::StatusCode::UNAUTHORIZED, Json(json!({"error":"invalid credentials"}))).into_response();
    };
    let ok = PasswordHash::new(&hash)
        .ok()
        .and_then(|parsed| Argon2::default().verify_password(body.password.as_bytes(), &parsed).ok())
        .is_some();
    if !ok {
        return (axum::http::StatusCode::UNAUTHORIZED, Json(json!({"error":"invalid credentials"}))).into_response();
    }
    let totp_on: Option<String> = sqlx::query_scalar("SELECT v FROM settings WHERE k = 'security_totp'")
        .fetch_optional(&state.db).await.ok().flatten();
    if totp_on.as_deref() == Some("1") {
        let secret: Option<String> = sqlx::query_scalar(&q("SELECT totp_secret FROM users WHERE username = ?"))
            .bind(&body.username).fetch_optional(&state.db).await.ok().flatten();
        if let Some(secret) = secret.filter(|s| !s.is_empty()) {
            let code = body.totp.as_deref().unwrap_or_default();
            if !totp_verify(&secret, code) {
                return (axum::http::StatusCode::UNAUTHORIZED, Json(json!({"error":"totp_required","totp":true}))).into_response();
            }
        }
    }
    let token = uuid::Uuid::new_v4().to_string();
    state.sessions.insert(token.clone(), body.username.clone());
    let cookie = format!("dokitier_session={token}; HttpOnly; Path=/; SameSite=Lax; Max-Age=86400");
    let mut response = Json(json!({"ok": true, "username": body.username, "role": role})).into_response();
    response.headers_mut().insert(axum::http::header::SET_COOKIE, cookie.parse().unwrap());
    response
}

async fn logout(State(state): State<AppState>, headers: axum::http::HeaderMap) -> Json<Value> {
    if let Some(token) = session_token(&headers) {
        state.sessions.remove(&token);
    }
    Json(json!({"ok": true}))
}

fn session_token(headers: &axum::http::HeaderMap) -> Option<String> {
    let cookie = headers.get(axum::http::header::COOKIE)?.to_str().ok()?;
    for part in cookie.split(';') {
        let part = part.trim();
        if let Some(value) = part.strip_prefix("dokitier_session=") {
            return Some(value.to_string());
        }
    }
    None
}

async fn require_auth(
    State(state): State<AppState>,
    req: axum::http::Request<axum::body::Body>,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let Some(token) = session_token(req.headers()) else {
        return (axum::http::StatusCode::UNAUTHORIZED, Json(json!({"error":"login required"}))).into_response();
    };
    if !state.sessions.contains_key(&token) {
        return (axum::http::StatusCode::UNAUTHORIZED, Json(json!({"error":"login required"}))).into_response();
    }
    next.run(req).await
}

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

async fn list_audit(State(state): State<AppState>) -> Json<Value> {
    let rows: Vec<(i64, String, String, String, Option<String>)> = sqlx::query_as(&q(
        "SELECT ts, action, node_id, network, detail FROM audit_logs ORDER BY id DESC LIMIT 200",
    )).fetch_all(&state.db).await.unwrap_or_default();
    Json(json!({"logs": rows.into_iter().map(|(ts, action, node_id, network, detail)| json!({
        "ts": ts, "action": action, "node_id": node_id, "network": network, "detail": detail,
    })).collect::<Vec<_>>()}))
}

async fn health() -> Json<Value> { Json(json!({"ok": true, "service": "dokitier-console"})) }

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().init();
    let mut listen = "127.0.0.1:12800".to_string();
    let mut agent_quic = "127.0.0.1:12803".to_string();
    let mut db_path = "board.db".to_string();
    let mut webui = String::new(); // empty = embedded web UI
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
    let _ = DB_KIND.set(if db_path.starts_with("postgres://") || db_path.starts_with("postgresql://") {
        "postgres".to_string()
    } else if db_path.starts_with("mysql://") {
        "mysql".to_string()
    } else {
        "sqlite".to_string()
    });
    let _ = CONSOLE_START.set(std::time::Instant::now());
    let postgres = db_path.starts_with("postgres://") || db_path.starts_with("postgresql://");
    let db = AnyPoolOptions::new().max_connections(5)
        .connect(&database_url(&db_path)).await?;
    init_db(&db).await?;
    for (node_id, token) in &add_nodes {
        let upsert = if is_mysql() {
            "INSERT INTO nodes (node_id, token_hash, token) VALUES (?, ?, ?) ON DUPLICATE KEY UPDATE token_hash = VALUES(token_hash), token = VALUES(token)"
        } else {
            "INSERT INTO nodes (node_id, token_hash, token) VALUES (?, ?, ?) ON CONFLICT(node_id) DO UPDATE SET token_hash = excluded.token_hash, token = excluded.token"
        };
        sqlx::query(&q(upsert))
            .bind(node_id).bind(hash_token(token)).bind(token).execute(&db).await?;
        tracing::info!(%node_id, "registered node");
    }
    ensure_admin(&db).await?;
    let state = AppState { db, postgres,
        sessions: std::sync::Arc::new(dashmap::DashMap::new()),
        agents: std::sync::Arc::new(dashmap::DashMap::new()),
        pending: std::sync::Arc::new(dashmap::DashMap::new()),
        last_traffic: std::sync::Arc::new(dashmap::DashMap::new()),
        edge_traffic: std::sync::Arc::new(dashmap::DashMap::new()),
        edge_rates: std::sync::Arc::new(dashmap::DashMap::new()),
        node_rt: std::sync::Arc::new(dashmap::DashMap::new()),
        rt_flush: std::sync::Arc::new(dashmap::DashMap::new()),
        geo_attempts: std::sync::Arc::new(dashmap::DashMap::new()) };
    spawn_bandwidth_sampler(state.clone());
    spawn_edge_rate_sampler(state.clone());
    spawn_snapshot_flusher(state.clone());
    spawn_history_maintenance(state.clone());
    spawn_geo_backfill(state.clone());
    let _ = rustls::crypto::ring::default_provider().install_default();
    let certified = rcgen::generate_simple_self_signed(vec!["localhost".to_string()])?;
    let cert_der = rustls::pki_types::CertificateDer::from(certified.cert.der().to_vec());
    let key_der = rustls::pki_types::PrivateKeyDer::Pkcs8(
        rustls::pki_types::PrivatePkcs8KeyDer::from(certified.key_pair.serialize_der()),
    );
    // One self-signed certificate serves both the QUIC agent endpoint and the
    // web/wss port (https + wss on the same TCP listener).
    let web_tls = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert_der.clone()], key_der.clone_key())?;
    let tls_acceptor = tokio_rustls::TlsAcceptor::from(std::sync::Arc::new(web_tls));
    let mut tls = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert_der], key_der)?;
    tls.alpn_protocols = vec![b"dokitier".to_vec()];
    let mut quic_cfg = quinn::ServerConfig::with_crypto(std::sync::Arc::new(
        quinn::crypto::rustls::QuicServerConfig::try_from(tls)?,
    ));
    let mut transport = quinn::TransportConfig::default();
    transport.max_concurrent_bidi_streams(100u32.into());
    quic_cfg.transport_config(std::sync::Arc::new(transport));
    let quic_endpoint = quinn::Endpoint::server(quic_cfg, agent_quic.parse()?)?;
    tracing::info!(%listen, %agent_quic, "console listeners ready (websocket + quic agent endpoints)");
    let quic_state = state.clone();
    tokio::spawn(async move { quic_accept_loop(quic_state, quic_endpoint).await });
    let protected = Router::new()
        .route("/api/overview", get(overview))
        .route("/api/board/overview", get(board_overview))
        .route("/api/board/topology", get(board_topology))
        .route("/api/board/events", get(board_events))
        .route("/api/board/links", get(board_links))
        .route("/api/board/routes", get(board_routes))
        .route("/api/board/history", get(board_history))
        .route("/api/board/audit", get(list_audit))
        .route("/api/board/nodes", get(list_nodes).post(add_node))
        .route("/api/board/nodes/{node_id}", get(get_node).delete(delete_node))
        .route("/api/board/nodes/{node_id}/token", get(get_node_token).post(rotate_node_token))
        .route("/api/board/nodes/{node_id}/versions", get(list_versions))
        .route("/api/board/nodes/{node_id}/versions/{version_id}", get(get_version))
        .route("/api/board/nodes/{node_id}/command", axum::routing::post(post_command))
        .route("/api/board/nodes/{node_id}/alias", axum::routing::post(set_alias))
        .route("/api/board/settings/geo", get(get_geo_setting).post(set_geo_setting))
        .route("/api/board/nodes/{node_id}/settings", axum::routing::post(set_node_settings))
        .route("/api/board/settings/routing", get(get_routing_settings).post(set_routing_settings))
        .route("/api/board/system/perf", get(get_perf))
        .route("/api/board/bandwidth/history", get(board_bandwidth_history))
        .route("/api/board/networks", get(board_networks))
        .route("/api/board/networks/create", axum::routing::post(create_network))
        .route("/api/board/precheck", axum::routing::post(precheck_config))
        .route("/api/board/drift", get(board_drift))
        .route("/api/board/acl", get(get_acl).post(set_acl))
        .route("/api/board/security", get(get_security_settings).post(set_security_settings))
        .route("/api/board/security/totp/setup", axum::routing::post(totp_setup))
        .route("/api/board/security/totp/verify", axum::routing::post(totp_verify_setup))
        .route("/api/board/security/totp/disable", axum::routing::post(totp_disable))
        .route("/api/board/account", get(get_account).post(update_account))
        .route_layer(axum::middleware::from_fn_with_state(state.clone(), require_auth));
    let base = Router::new()
        .route("/healthz", get(health))
        .route("/install.sh", get(install_script))
        .route("/api/login", axum::routing::post(login))
        .route("/api/auth/status", get(auth_status))
        .route("/api/logout", axum::routing::post(logout))
        .route("/api/agent/ws", get(ws_agent))
        .merge(protected);
    let app = if webui.is_empty() {
        base.fallback(serve_webui)
    } else {
        base.fallback_service(ServeDir::new(&webui).append_index_html_on_directories(true))
    }
    .with_state(state);
    let listener = TcpListener::bind(&listen).await?;
    tracing::info!(
        %listen,
        %agent_quic,
        embedded_webui = webui.is_empty(),
        "console listening (http/https + ws/wss on one TCP port, quic on UDP)"
    );
    axum::serve(
        MixedListener {
            listener,
            tls: tls_acceptor,
        },
        app,
    )
    .with_graceful_shutdown(async {
        let _ = tokio::signal::ctrl_c().await;
    })
    .await?;
    Ok(())
}

#[cfg(test)]
mod runtime_tests {
    use super::*;
    use sqlx::any::AnyPoolOptions;

    async fn test_state() -> AppState {
        sqlx::any::install_default_drivers();
        POSTGRES.store(false, Ordering::Relaxed);
        let db = AnyPoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("sqlite memory");
        init_db(&db).await.expect("schema");
        sqlx::query("INSERT INTO nodes (node_id, token_hash, token, alias, country_code) VALUES (?, ?, ?, ?, ?)")
            .bind("n1")
            .bind(hash_token("tok"))
            .bind("tok")
            .bind("alias-n1")
            .bind("cn")
            .execute(&db)
            .await
            .expect("seed node");
        AppState {
            db,
            sessions: Default::default(),
            postgres: false,
            agents: Default::default(),
            pending: Default::default(),
            last_traffic: Default::default(),
            edge_traffic: Default::default(),
            edge_rates: Default::default(),
            node_rt: Default::default(),
            rt_flush: Default::default(),
            geo_attempts: Default::default(),
        }
    }

    fn frame(rx: u64) -> Value {
        json!({
            "public_ipv4": "203.0.113.5",
            "cpu_pct": 12.0,
            "mem_pct": 30.0,
            "mem_used": 1024,
            "mem_total": 4096,
            "instances": [{
                "network_name": "netA",
                "peer_id": 11,
                "p2p_group": "cn",
                "hostname": "node-one",
                "version": "test",
                "ipv4": "10.1.0.1",
                "topology": {
                    "nodes": [
                        {"peer_id": 11, "local": true, "hostname": "node-one", "ipv4": "10.1.0.1"},
                        {"peer_id": 22, "local": false, "hostname": "node-two", "ipv4": "10.1.0.2"}
                    ],
                    "edges": [{
                        "a": 11, "b": 22,
                        "tunnels": [{
                            "type": "tcp",
                            "local_addr": "tcp://10.1.0.1:11011",
                            "remote_addr": "tcp://10.1.0.2:11011",
                            "latency_ms": 12.5,
                            "loss_pct": 0.5,
                            "rx_bytes": rx,
                            "tx_bytes": rx / 2,
                            "capacity_mbps": 100.0
                        }]
                    }]
                }
            }]
        })
    }

    #[tokio::test]
    async fn realtime_is_memory_only_and_flush_persists() {
        let state = test_state().await;
        apply_snapshot_to_rt(&state, "n1", &frame(1_000_000));
        sample_edge_rates_once(&state);
        tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
        apply_snapshot_to_rt(&state, "n1", &frame(3_000_000));
        sample_edge_rates_once(&state);

        // 1) no SQL write happened just from receiving frames
        let snapshot_before: Option<String> = sqlx::query_scalar("SELECT snapshot FROM nodes WHERE node_id = 'n1'")
            .fetch_optional(&state.db)
            .await
            .unwrap()
            .flatten();
        // schema default is the literal string "null"; receiving frames must not
        // replace it (only the batch flusher writes real snapshots)
        assert!(
            snapshot_before.as_deref().map(|v| v == "null" || v.is_empty()).unwrap_or(true),
            "per-frame snapshot must not hit SQL, got {snapshot_before:?}"
        );

        // 2) topology API serves the in-memory state with rates + latency
        let topo = topology_payload(&state, None, None).await;
        let graph = &topo["graph"];
        assert_eq!(graph["nodes"].as_array().unwrap().len(), 2);
        let edges = graph["edges"].as_array().unwrap();
        assert_eq!(edges.len(), 1);
        let rx = edges[0]["rx_bps"].as_f64().unwrap();
        assert!(rx > 1_000_000.0, "rate should reflect ~2MB over ~1s, got {rx}");
        assert_eq!(edges[0]["latency_ms"].as_f64().unwrap(), 12.5);

        // 3) network scope still isolates correctly
        let scoped = topology_payload(&state, Some("netA"), None).await;
        assert_eq!(scoped["graph"]["edges"].as_array().unwrap().len(), 1);
        let other = topology_payload(&state, Some("netB"), None).await;
        assert!(other["graph"]["edges"].as_array().unwrap().is_empty());

        // 4) node list merges DB static metadata with memory telemetry
        let nodes = nodes_payload_lite(&state).await;
        let node = &nodes["nodes"].as_array().unwrap()[0];
        assert_eq!(node["status"].as_str(), Some("online"));
        assert_eq!(node["alias"].as_str(), Some("alias-n1"));
        assert!(node["rx_bps"].as_f64().unwrap() > 0.0);

        // 5) explicit flush writes snapshot, telemetry and link history
        let rt = state.node_rt.get("n1").unwrap().clone();
        assert!(flush_one_node(state.clone(), "n1".to_string(), rt).await);
        let (snapshot_after, rx_bps): (String, f64) =
            sqlx::query_as("SELECT snapshot, rx_bps FROM nodes WHERE node_id = 'n1'")
                .fetch_one(&state.db)
                .await
                .unwrap();
        assert!(snapshot_after.contains("netA"));
        assert!(rx_bps > 0.0);
        let links: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM link_metrics")
            .fetch_one(&state.db)
            .await
            .unwrap();
        assert!(links >= 1, "flush must persist link samples, got {links}");
    }

    #[tokio::test]
    async fn link_history_is_downsampled_and_pruned() {
        let state = test_state().await;
        let now_ts = now();
        let insert = |ts: i64, latency: f64| {
            let db = state.db.clone();
            async move {
                sqlx::query("INSERT INTO link_metrics (network, a, b, a_uuid, b_uuid, ts, latency_ms, loss_pct) VALUES ('netA', 11, 22, 'u1', 'u2', ?, ?, 0.0)")
                    .bind(ts)
                    .bind(latency)
                    .execute(&db)
                    .await
                    .unwrap();
            }
        };
        // two raw samples older than 1h, off bucket boundaries
        insert(now_ts - 7200 + 7, 10.0).await;
        insert(now_ts - 7200 + 19, 20.0).await;
        // one sample beyond the 30 day retention window
        insert(now_ts - 31 * 24 * 3600, 30.0).await;

        compact_link_metrics(&state).await;

        let raw_left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM link_metrics WHERE ts % 60 <> 0")
            .fetch_one(&state.db)
            .await
            .unwrap();
        assert_eq!(raw_left, 0, "raw off-bucket samples must be aggregated away");
        let aggregated: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM link_metrics WHERE ts % 60 = 0")
            .fetch_one(&state.db)
            .await
            .unwrap();
        assert!(aggregated >= 1, "aggregated buckets expected, got {aggregated}");
        let expired: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM link_metrics WHERE ts < ?")
            .bind(now_ts - 30 * 24 * 3600)
            .fetch_one(&state.db)
            .await
            .unwrap();
        assert_eq!(expired, 0, "rows older than 30 days must be deleted");
    }

    #[tokio::test]
    async fn topology_geo_uses_node_coordinates() {
        let state = test_state().await;
        sqlx::query("UPDATE nodes SET city = 'Chengdu', latitude = 30.66, longitude = 104.06 WHERE node_id = 'n1'")
            .execute(&state.db)
            .await
            .unwrap();
        sqlx::query("INSERT INTO network_instances (node_id, network, instance_id, created) VALUES ('n1', 'netA', 'uuid-1', 0)")
            .execute(&state.db)
            .await
            .unwrap();
        let mut frame = frame(1_000_000);
        frame["instances"][0]["instance_id"] = json!("uuid-1");
        apply_snapshot_to_rt(&state, "n1", &frame);

        let topo = topology_payload(&state, None, None).await;
        let geo = &topo["geo"];
        let cities = geo["cities"].as_array().unwrap();
        assert_eq!(cities.len(), 1, "one city expected: {geo}");
        assert_eq!(cities[0]["city"].as_str(), Some("Chengdu"));
        assert_eq!(cities[0]["nodes"].as_u64(), Some(2));
        assert!((cities[0]["lat"].as_f64().unwrap() - 30.66).abs() < 1e-6);
        // the node itself carries the coordinates too
        let node = topo["graph"]["nodes"].as_array().unwrap().iter()
            .find(|n| n["peer_id"].as_u64() == Some(11))
            .expect("local node");
        assert_eq!(node["city"].as_str(), Some("Chengdu"));
    }

    #[tokio::test]
    async fn country_and_geo_survive_restart_without_agent() {
        let state = test_state().await;
        sqlx::query("UPDATE nodes SET country_code = 'cn', city = 'Chengdu', latitude = 30.66, longitude = 104.06, system_info = '{\"cpu_cores\": 8, \"os_name\": \"Linux\"}' WHERE node_id = 'n1'")
            .execute(&state.db)
            .await
            .unwrap();
        // a console restart leaves the database intact but wipes the runtime
        // registry: flags/geo must still resolve from persistence
        state.node_rt.clear();
        let payload = nodes_payload(&state).await;
        let node = &payload["nodes"].as_array().unwrap()[0];
        assert_eq!(node["country_code"].as_str(), Some("cn"), "flag must survive restart: {payload}");
        assert_eq!(node["city"].as_str(), Some("Chengdu"));
        assert_eq!(node["node_id"].as_str(), Some("n1"));
        assert_eq!(node["system_info"]["cpu_cores"].as_i64(), Some(8), "system info must come back after restart: {payload}");
    }

    #[tokio::test]
    async fn board_topology_returns_geo_model() {
        let state = test_state().await;
        sqlx::query("UPDATE nodes SET country_code = 'cn', city = 'Chengdu', latitude = 30.66, longitude = 104.06 WHERE node_id = 'n1'")
            .execute(&state.db)
            .await
            .unwrap();
        sqlx::query("INSERT INTO network_instances (node_id, network, instance_id, created) VALUES ('n1', 'netA', 'uuid-1', 0)")
            .execute(&state.db)
            .await
            .unwrap();
        let mut snapshot = frame(1_000_000);
        snapshot["instances"][0]["instance_id"] = json!("uuid-1");
        apply_snapshot_to_rt(&state, "n1", &snapshot);
        let response = board_topology(
            State(state.clone()),
            axum::extract::Query(TopologyQuery { network: None, group: None }),
        )
        .await;
        let body = &response.0;
        let cities = body["geo"]["cities"].as_array().unwrap();
        assert_eq!(cities.len(), 1, "globe needs cities: {body}");
        assert_eq!(cities[0]["city"].as_str(), Some("Chengdu"));
        assert!(cities[0]["lat"].as_f64().is_some());
        // the SSE frame must include the same geo model
        let frame: Value = serde_json::from_str(&topology_event_json(body)).unwrap();
        assert_eq!(frame["geo"]["cities"].as_array().unwrap().len(), 1, "SSE frame lost geo: {frame}");
    }


    #[tokio::test]
    async fn node_settings_accept_numeric_and_string_geo_overrides() {
        let state = test_state().await;
        let body = NodeSettingsBody {
            alias: Some("renamed".into()),
            country_code: Some(NumOrStr::Str("us".into())),
            latitude: Some(NumOrStr::Num(30.5)),
            longitude: Some(NumOrStr::Str("104.25".into())),
            bandwidth_mbps: None,
            bandwidth_up_mbps: None,
            bandwidth_down_mbps: None,
            bandwidth_intranet_down_mbps: None,
            bandwidth_internet_down_mbps: None,
            shared_listeners: None,
        };
        let res = set_node_settings(State(state.clone()), Path("n1".to_string()), Json(body)).await;
        assert!(res.0.get("error").is_none(), "unexpected error: {}", res.0);
        let (alias, country, lat, lon): (Option<String>, Option<String>, Option<f64>, Option<f64>) =
            sqlx::query_as("SELECT alias, country_code, latitude, longitude FROM nodes WHERE node_id = 'n1'")
                .fetch_one(&state.db)
                .await
                .unwrap();
        assert_eq!(alias.as_deref(), Some("renamed"));
        assert_eq!(country.as_deref(), Some("us"));
        assert_eq!(lat, Some(30.5));
        assert_eq!(lon, Some(104.25));

        let body = NodeSettingsBody {
            alias: None,
            country_code: None,
            latitude: Some(NumOrStr::Str("31.75".into())),
            longitude: Some(NumOrStr::Str("".into())),
            bandwidth_mbps: None,
            bandwidth_up_mbps: None,
            bandwidth_down_mbps: None,
            bandwidth_intranet_down_mbps: None,
            bandwidth_internet_down_mbps: None,
            shared_listeners: None,
        };
        let res = set_node_settings(State(state.clone()), Path("n1".to_string()), Json(body)).await;
        assert!(res.0.get("error").is_none(), "unexpected error: {}", res.0);
        let (lat, lon): (Option<f64>, Option<f64>) =
            sqlx::query_as("SELECT latitude, longitude FROM nodes WHERE node_id = 'n1'")
                .fetch_one(&state.db)
                .await
                .unwrap();
        assert_eq!(lat, Some(31.75));
        assert_eq!(lon, Some(104.25));
    }

    #[tokio::test]
    async fn add_node_and_rotate_token() {
        let state = test_state().await;
        let created = add_node(
            State(state.clone()),
            Json(AddNodeBody { node_id: "new-node-1".into(), alias: Some("New".into()) }),
        )
        .await;
        assert_eq!(created.0["ok"].as_bool(), Some(true), "{:?}", created.0);
        let token = created.0["token"].as_str().unwrap().to_string();
        assert_eq!(token.len(), 32);

        let fetched = get_node_token(State(state.clone()), Path("new-node-1".to_string())).await;
        assert_eq!(fetched.0["token"].as_str(), Some(token.as_str()));

        let dup = add_node(
            State(state.clone()),
            Json(AddNodeBody { node_id: "new-node-1".into(), alias: None }),
        )
        .await;
        assert_eq!(dup.0["ok"].as_bool(), Some(false));

        let rotated = rotate_node_token(State(state.clone()), Path("new-node-1".to_string())).await;
        let new_token = rotated.0["token"].as_str().unwrap().to_string();
        assert_ne!(new_token, token);
        assert_eq!(new_token.len(), 32);
    }

    #[tokio::test]
    async fn frontend_settings_payload_deserializes_and_saves() {
        let state = test_state().await;
        // exactly the body the web UI sends (strings for geo, numbers for caps)
        let raw = json!({
            "alias": "nms-renamed",
            "country_code": "cn",
            "latitude": "30.5",
            "longitude": "104.25",
            "bandwidth_up_mbps": 500,
            "bandwidth_down_mbps": 100,
            "bandwidth_intranet_down_mbps": null,
            "bandwidth_internet_down_mbps": null,
            "shared_listeners": "tcp://0.0.0.0:12401"
        });
        let body: NodeSettingsBody = serde_json::from_value(raw).expect("frontend payload must deserialize");
        let res = set_node_settings(State(state.clone()), Path("n1".to_string()), Json(body)).await;
        assert!(res.0.get("error").is_none(), "save failed: {}", res.0);
        let (alias, lat, listeners): (Option<String>, Option<f64>, Option<String>) =
            sqlx::query_as("SELECT alias, latitude, shared_listeners FROM nodes WHERE node_id = 'n1'")
                .fetch_one(&state.db)
                .await
                .unwrap();
        assert_eq!(alias.as_deref(), Some("nms-renamed"));
        assert_eq!(lat, Some(30.5));
        assert_eq!(listeners.as_deref(), Some("tcp://0.0.0.0:12401"));
    }

    #[tokio::test]
    async fn settings_push_waits_for_core_ack_and_reports_result() {
        let state = test_state().await;
        sqlx::query("INSERT INTO config_versions (node_id, network, hash, toml, created) VALUES ('n1', 'netA', 'h1', 'instance_name = \"netA-n1\"', 0)")
            .execute(&state.db)
            .await
            .unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::channel::<Value>(4);
        state.agents.insert("n1:netA".into(), tx);

        let state_for_task = state.clone();
        let handle = tokio::spawn(async move {
            let body = NodeSettingsBody {
                alias: None,
                country_code: None,
                latitude: None,
                longitude: None,
                bandwidth_mbps: None,
                bandwidth_up_mbps: Some(500.0),
                bandwidth_down_mbps: None,
                bandwidth_intranet_down_mbps: None,
                bandwidth_internet_down_mbps: None,
                shared_listeners: None,
            };
            set_node_settings(State(state_for_task), Path("n1".to_string()), Json(body)).await
        });

        // the console must be waiting for the core callback, not returning yet
        let frame = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
            .await
            .expect("console did not push the config")
            .expect("agent channel closed");
        let request_id = frame["request_id"].as_str().unwrap().to_string();
        assert_eq!(frame["command"]["kind"].as_str(), Some("apply_network"));
        let (_, result_tx) = state.pending.remove(&request_id).expect("pending request");
        let _ = result_tx.send(json!({"ok": true}));

        let res = tokio::time::timeout(std::time::Duration::from_secs(5), handle)
            .await
            .expect("save did not finish after the core ack")
            .unwrap();
        assert_eq!(res.0["ok"].as_bool(), Some(true), "{:?}", res.0);
        let up: Option<f64> = sqlx::query_scalar("SELECT bandwidth_up_mbps FROM nodes WHERE node_id = 'n1'")
            .fetch_one(&state.db)
            .await
            .unwrap();
        assert_eq!(up, Some(500.0));
    }
}
