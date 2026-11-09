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
    geo_attempts: std::sync::Arc<dashmap::DashMap<String, i64>>,
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

fn now() -> i64 { SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() as i64 }

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
        "public_ipv4 TEXT", "public_ipv6 TEXT", "country_code TEXT", "rx_bps REAL", "tx_bps REAL", "node_uuid TEXT", "system_info TEXT", "traffic TEXT", "bandwidth_mbps REAL", "bandwidth_up_mbps REAL", "bandwidth_down_mbps REAL", "bandwidth_intranet_down_mbps REAL", "bandwidth_internet_down_mbps REAL", "shared_listeners TEXT",
        "alias TEXT", "cpu_pct REAL", "mem_pct REAL", "mem_used BIGINT", "mem_total BIGINT",
    ] {
        let _ = sqlx::query(&format!("ALTER TABLE nodes ADD COLUMN {column}")).execute(db).await;
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

const GEO_PROVIDERS: [&str; 4] = ["ipwho.is", "ip.sb", "ipinfo.io", "ipapi.co"];

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

async fn fetch_country(client: &reqwest::Client, provider: &str, ip: &str) -> Option<String> {
    let (url, field) = match provider {
        "ip.sb" => (format!("https://api.ip.sb/geoip/{ip}"), "country_code"),
        "ipinfo.io" => (format!("https://ipinfo.io/{ip}/json"), "country"),
        "ipapi.co" => (format!("https://ipapi.co/{ip}/json/"), "country_code"),
        _ => (format!("https://ipwho.is/{ip}"), "country_code"),
    };
    let response = client.get(url).send().await.ok()?;
    let value: Value = response.json().await.ok()?;
    value.get(field).and_then(|v| v.as_str()).map(|s| s.to_lowercase())
}

async fn lookup_and_store_country(db: AnyPool, node_id: String, ip: String) {
    let client = match reqwest::Client::builder().timeout(std::time::Duration::from_secs(6)).build() {
        Ok(client) => client,
        Err(_) => return,
    };
    let configured = get_setting(&db, "geo_provider").await.unwrap_or_else(|| "ipwho.is".to_string());
    let mut order = vec![configured.clone()];
    for provider in GEO_PROVIDERS {
        if provider != configured {
            order.push(provider.to_string());
        }
    }
    for provider in order {
        if let Some(code) = fetch_country(&client, &provider, &ip).await {
            if code.len() == 2 {
                let _ = sqlx::query(&q("UPDATE nodes SET country_code = ? WHERE node_id = ?"))
                    .bind(code).bind(&node_id).execute(&db).await;
                return;
            }
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
            let rows: Vec<(f64, f64)> = sqlx::query_as(
                "SELECT rx_bps, tx_bps FROM nodes WHERE status = 'online'",
            )
            .fetch_all(&state.db)
            .await
            .unwrap_or_default();
            let rx: f64 = rows.iter().map(|(rx, _)| *rx).sum();
            let tx: f64 = rows.iter().map(|(_, tx)| *tx).sum();
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
            if value.get("instance_name").and_then(|v| v.as_str()).unwrap_or("").is_empty() {
                errors.push("instance_name is required".to_string());
            }
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
    let acl_chains = load_acl_chains(&state.db).await;
    if let Ok(mut value) = toml::from_str::<toml::Value>(&toml) {
        if let Some(table) = value.as_table_mut() {
            inject_acl(table, &acl_chains);
            if let Ok(text) = toml::to_string(&value) { toml = text; }
        }
    }
    let key = format!("{node_id}:{network}");
    let key = if state.agents.contains_key(&key) {
        key
    } else {
        let prefix = format!("{node_id}:");
        match state.agents.iter().find(|entry| entry.key().starts_with(&prefix)) {
            Some(entry) => entry.key().clone(),
            None => return json!({"ok": false, "error": "node is offline"}),
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
    // Collect used /24 subnets from all node snapshots.
    let rows: Vec<(String, String, String, String)> = sqlx::query_as(
        "SELECT node_id, status, COALESCE(snapshot, ''), COALESCE(system_info, '{}') FROM nodes",
    ).fetch_all(&state.db).await.unwrap_or_default();
    let mut used_subnets = std::collections::HashSet::new();
    let mut node_info: std::collections::HashMap<String, (String, String, String)> = std::collections::HashMap::new();
    for (node_id, status, snapshot, system_info) in &rows {
        node_info.insert(node_id.clone(), (status.clone(), snapshot.clone(), system_info.clone()));
        if let Ok(value) = serde_json::from_str::<Value>(snapshot) {
            for inst in value.get("instances").and_then(|v| v.as_array()).into_iter().flatten() {
                if let Some(ip) = inst.get("ipv4").and_then(|v| v.as_str()) {
                    if let Some((a, b, c, _)) = parse_ipv4(ip) {
                        used_subnets.insert(format!("{a}.{b}.{c}.0/24"));
                    }
                }
            }
        }
    }
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
                let snapshot: Option<String> = sqlx::query_scalar(&q("SELECT snapshot FROM nodes WHERE node_id = ?"))
                    .bind(node).fetch_optional(&state.db).await.ok().flatten();
                let connected = snapshot
                    .and_then(|text| serde_json::from_str::<Value>(&text).ok())
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
    let rows: Vec<(String, String, String, String)> =
        sqlx::query_as("SELECT node_id, COALESCE(status, 'offline'), COALESCE(snapshot, ''), COALESCE(traffic, '{}') FROM nodes")
            .fetch_all(&state.db)
            .await
            .unwrap_or_default();
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
    let rows: Vec<(String, String, String)> = sqlx::query_as("SELECT node_id, status, snapshot FROM nodes")
        .fetch_all(&state.db)
        .await
        .unwrap_or_default();
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
    let nodes: Vec<(String,)> = sqlx::query_as(&q("SELECT node_id FROM nodes WHERE status = 'online'"))
        .fetch_all(&state.db)
        .await
        .unwrap_or_default();
    for (node_id,) in nodes {
        push_routing_to_node(&state, &node_id).await;
    }
    Json(json!({"ok": true, "params": body}))
}

async fn get_geo_setting(State(state): State<AppState>) -> Json<Value> {
    let provider = get_setting(&state.db, "geo_provider").await.unwrap_or_else(|| "ipwho.is".to_string());
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
        let snapshot: String = sqlx::query_scalar(&q("SELECT snapshot FROM nodes WHERE node_id = ?"))
            .bind(&node_id)
            .fetch_optional(&state.db)
            .await
            .ok()
            .flatten()
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
                                table.entry("node_bandwidth_up_mbps".to_string()).or_insert(toml::Value::Float(f64::from(up)));
                            }
                            if let Some(down) = down {
                                table.entry("node_bandwidth_down_mbps".to_string()).or_insert(toml::Value::Float(f64::from(down)));
                            }
                            if let Some(cap) = intranet {
                                table.entry("node_bandwidth_intranet_down_mbps".to_string()).or_insert(toml::Value::Float(f64::from(cap)));
                            }
                            if let Some(cap) = internet {
                                table.entry("node_bandwidth_internet_down_mbps".to_string()).or_insert(toml::Value::Float(f64::from(cap)));
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
            let hostname_opt = (!hostname.is_empty()).then_some(hostname.as_str());
            let version_opt = (!version.is_empty()).then_some(version.as_str());
            let networks_value = Value::Array(networks.clone());
            let networks_opt = (!networks.is_empty()).then_some(&networks_value);
            touch_node(&state.db, &node_id, hostname_opt, version_opt, networks_opt, None, "online").await;
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
                tokio::spawn(async move { lookup_and_store_country(db, id, ip).await });
            }
            Ok(())
        }
        AgentFrame::Heartbeat { node_id, .. } => {
            touch_node(&state.db, &node_id, None, None, None, None, "online").await; Ok(())
        }
        AgentFrame::Snapshot { node_id, data } => {
            let v4 = data.get("public_ipv4").and_then(|v| v.as_str()).map(|s| s.to_string());
            let v6 = data.get("public_ipv6").and_then(|v| v.as_str()).map(|s| s.to_string());
            update_public_ip(&state.db, &node_id, v4.as_deref(), v6.as_deref()).await;
            // resolve geo for nodes that still have no country (throttled per node)
            if let Some(ip) = v4.clone().or(v6.clone()) {
                let has_country: Option<(Option<String>,)> = sqlx::query_as(&q("SELECT country_code FROM nodes WHERE node_id = ?"))
                    .bind(&node_id).fetch_optional(&state.db).await.ok().flatten();
                let missing = has_country.and_then(|(c,)| c).map(|c| c.is_empty()).unwrap_or(true);
                let now_ts = now();
                let due = state.geo_attempts.get(&node_id).map(|v| now_ts - *v > 300).unwrap_or(true);
                if missing && due {
                    state.geo_attempts.insert(node_id.clone(), now_ts);
                    let db = state.db.clone();
                    let id = node_id.clone();
                    tokio::spawn(async move { lookup_and_store_country(db, id, ip).await });
                }
            }
            update_traffic(state, &node_id, &data).await;
            let cpu_pct = data.get("cpu_pct").and_then(|v| v.as_f64()).map(|v| v as f32);
            let mem_pct = data.get("mem_pct").and_then(|v| v.as_f64()).map(|v| v as f32);
            let mem_used = data.get("mem_used").and_then(|v| v.as_u64()).map(|v| v as i64);
            let mem_total = data.get("mem_total").and_then(|v| v.as_u64()).map(|v| v as i64);
            let _ = sqlx::query(&q("UPDATE nodes SET cpu_pct = COALESCE(?, cpu_pct), mem_pct = COALESCE(?, mem_pct), mem_used = COALESCE(?, mem_used), mem_total = COALESCE(?, mem_total) WHERE node_id = ?"))
                .bind(cpu_pct).bind(mem_pct).bind(mem_used).bind(mem_total).bind(&node_id)
                .execute(&state.db).await;
            let merged = merge_snapshot(state, &node_id, &data).await;
            if let Some(instances) = data.get("instances").and_then(|v| v.as_array()) {
                for inst in instances {
                    let network = inst.get("network_name").and_then(|v| v.as_str()).unwrap_or_default();
                    let Some(topo) = inst.get("topology") else { continue };
                    let mut peer_uuid: std::collections::HashMap<i64, String> = std::collections::HashMap::new();
                    for node in topo.get("nodes").and_then(|v| v.as_array()).into_iter().flatten() {
                        let id = node.get("peer_id").and_then(|v| v.as_u64()).unwrap_or_default() as i64;
                        if let Some(uuid) = node.get("instance_id").and_then(|v| v.as_str()) {
                            if !uuid.is_empty() {
                                peer_uuid.insert(id, uuid.to_string());
                            }
                        }
                    }
                    for e in topo.get("edges").and_then(|v| v.as_array()).into_iter().flatten() {
                        let a = e.get("a").and_then(|v| v.as_u64()).unwrap_or_default() as i64;
                        let b = e.get("b").and_then(|v| v.as_u64()).unwrap_or_default() as i64;
                        let tunnels = e.get("tunnels").and_then(|v| v.as_array()).cloned().unwrap_or_default();
                        let latency = tunnels.iter().filter_map(|t| t.get("latency_ms").and_then(|v| v.as_f64())).reduce(f64::min);
                        let loss = tunnels.iter().filter_map(|t| t.get("loss_pct").and_then(|v| v.as_f64())).reduce(|x, y| x + y).map(|v| v / tunnels.len().max(1) as f64);
                        let (a_uuid, b_uuid) = match (peer_uuid.get(&a), peer_uuid.get(&b)) {
                            (Some(ua), Some(ub)) if ua <= ub => (Some(ua.clone()), Some(ub.clone())),
                            (Some(ua), Some(ub)) => (Some(ub.clone()), Some(ua.clone())),
                            _ => (peer_uuid.get(&a).cloned(), peer_uuid.get(&b).cloned()),
                        };
                        let _ = sqlx::query(&q("INSERT INTO link_metrics (network, a, b, a_uuid, b_uuid, ts, latency_ms, loss_pct) VALUES (?, ?, ?, ?, ?, ?, ?, ?)"))
                            .bind(network).bind(a).bind(b).bind(a_uuid).bind(b_uuid).bind(now()).bind(latency).bind(loss)
                            .execute(&state.db).await;
                    }
                }
            }
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
    let rows = sqlx::query(&q(
        "SELECT node_id, hostname, version, networks, snapshot, last_seen, status, public_ipv4, public_ipv6, country_code, rx_bps, tx_bps, alias, cpu_pct, mem_pct, mem_used, mem_total, node_uuid, system_info, traffic, bandwidth_mbps, bandwidth_up_mbps, bandwidth_down_mbps, bandwidth_intranet_down_mbps, bandwidth_internet_down_mbps, shared_listeners FROM nodes ORDER BY node_id",
    )).fetch_all(&state.db).await.unwrap_or_default();
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
    let mut nodes = Vec::new();
    for row in rows {
        let networks: String = row.try_get("networks").unwrap_or_default();
        let snapshot: String = row.try_get("snapshot").unwrap_or_default();
        nodes.push(NodeView {
            node_id: row.try_get("node_id").unwrap_or_default(),
            hostname: row.try_get("hostname").unwrap_or_default(),
            version: row.try_get("version").unwrap_or_default(),
            networks: serde_json::from_str(&networks).unwrap_or(Value::Null),
            snapshot: serde_json::from_str(&snapshot).unwrap_or(Value::Null),
            last_seen: row.try_get("last_seen").unwrap_or_default(),
            status: row.try_get("status").unwrap_or_default(),
            public_ipv4: row.try_get("public_ipv4").ok(),
            public_ipv6: row.try_get("public_ipv6").ok(),
            country_code: row.try_get("country_code").ok(),
            rx_bps: row.try_get("rx_bps").ok(),
            tx_bps: row.try_get("tx_bps").ok(),
            alias: row.try_get("alias").ok(),
            cpu_pct: row.try_get("cpu_pct").ok(),
            mem_pct: row.try_get("mem_pct").ok(),
            mem_used: row.try_get("mem_used").ok(),
            mem_total: row.try_get("mem_total").ok(),
            node_uuid: row.try_get("node_uuid").ok(),
            system_info: serde_json::from_str(&row.try_get::<String, _>("system_info").unwrap_or_default()).unwrap_or(Value::Null),
            traffic: serde_json::from_str(&row.try_get::<String, _>("traffic").unwrap_or_default()).unwrap_or(Value::Null),
            bandwidth_mbps: row.try_get("bandwidth_mbps").ok(),
            bandwidth_up_mbps: row.try_get("bandwidth_up_mbps").ok(),
            bandwidth_down_mbps: row.try_get("bandwidth_down_mbps").ok(),
            bandwidth_intranet_down_mbps: row.try_get("bandwidth_intranet_down_mbps").ok(),
            bandwidth_internet_down_mbps: row.try_get("bandwidth_internet_down_mbps").ok(),
            shared_listeners: row.try_get("shared_listeners").ok(),
            instance_ids: json!(instance_map.get(&row.try_get::<String, _>("node_id").unwrap_or_default()).cloned().unwrap_or_default()),
        });
    }
    json!({"nodes": nodes})
}

async fn list_nodes(State(state): State<AppState>) -> Json<Value> {
    Json(nodes_payload(&state).await)
}

#[derive(serde::Deserialize)]
struct NodeSettingsBody {
    alias: Option<String>,
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

async fn set_node_settings(
    State(state): State<AppState>,
    Path(node_id): Path<String>,
    Json(body): Json<NodeSettingsBody>,
) -> Json<Value> {
    if let Some(alias) = body.alias.as_deref() {
        let _ = sqlx::query(&q("UPDATE nodes SET alias = ? WHERE node_id = ?"))
            .bind(alias.trim()).bind(&node_id).execute(&state.db).await;
    }
    if let Some(bw) = body.bandwidth_mbps {
        let _ = sqlx::query(&q("UPDATE nodes SET bandwidth_mbps = ?, bandwidth_up_mbps = ?, bandwidth_down_mbps = ? WHERE node_id = ?"))
            .bind(bw.max(0.0) as f32).bind(bw.max(0.0) as f32).bind(bw.max(0.0) as f32).bind(&node_id).execute(&state.db).await;
        spawn_reconcile_inner(state.clone(), node_id.clone(), true);
    }
    if let Some(shared) = body.shared_listeners.as_deref() {
        let _ = sqlx::query(&q("UPDATE nodes SET shared_listeners = ? WHERE node_id = ?"))
            .bind(shared)
            .bind(&node_id).execute(&state.db).await;
        let listeners: Vec<String> = shared.split(',').map(str::trim).filter(|v| !v.is_empty()).map(|v| v.to_string()).collect();
        let result = send_shared_listeners(&state, &node_id, listeners).await;
        if result.get("ok").and_then(|v| v.as_bool()) != Some(true) {
            return Json(result);
        }
        spawn_reconcile_inner(state.clone(), node_id.clone(), true);
    }
    if body.bandwidth_intranet_down_mbps.is_some() || body.bandwidth_internet_down_mbps.is_some() {
        let _ = sqlx::query(&q("UPDATE nodes SET bandwidth_intranet_down_mbps = COALESCE(?, bandwidth_intranet_down_mbps), bandwidth_internet_down_mbps = COALESCE(?, bandwidth_internet_down_mbps) WHERE node_id = ?"))
            .bind(body.bandwidth_intranet_down_mbps.map(|v| v.max(0.0) as f32))
            .bind(body.bandwidth_internet_down_mbps.map(|v| v.max(0.0) as f32))
            .bind(&node_id).execute(&state.db).await;
        spawn_reconcile_inner(state.clone(), node_id.clone(), true);
    }
    if body.bandwidth_up_mbps.is_some() || body.bandwidth_down_mbps.is_some() {
        let _ = sqlx::query(&q("UPDATE nodes SET bandwidth_up_mbps = COALESCE(?, bandwidth_up_mbps), bandwidth_down_mbps = COALESCE(?, bandwidth_down_mbps) WHERE node_id = ?"))
            .bind(body.bandwidth_up_mbps.map(|v| v.max(0.0) as f32))
            .bind(body.bandwidth_down_mbps.map(|v| v.max(0.0) as f32))
            .bind(&node_id).execute(&state.db).await;
        spawn_reconcile_inner(state.clone(), node_id.clone(), true);
    }
    Json(json!({"ok": true}))
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

async fn get_node(State(state): State<AppState>, Path(node_id): Path<String>) -> Json<Value> {
    let row: Option<(String, String, String, String, String, i64, String, Option<String>, Option<String>, Option<String>, Option<f32>, Option<f32>)> = sqlx::query_as(&q(
        "SELECT node_id, hostname, version, networks, snapshot, last_seen, status, public_ipv4, public_ipv6, country_code, rx_bps, tx_bps FROM nodes WHERE node_id = ?",
    )).bind(&node_id).fetch_optional(&state.db).await.ok().flatten();
    match row {
        Some((node_id, hostname, version, networks, snapshot, last_seen, status, public_ipv4, public_ipv6, country_code, rx_bps, tx_bps)) => Json(json!({
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
                if let Some(up) = up { table.entry("node_bandwidth_up_mbps".to_string()).or_insert(toml::Value::Float(f64::from(up))); }
                if let Some(down) = down { table.entry("node_bandwidth_down_mbps".to_string()).or_insert(toml::Value::Float(f64::from(down))); }
                if let Some(cap) = intranet { table.entry("node_bandwidth_intranet_down_mbps".to_string()).or_insert(toml::Value::Float(f64::from(cap))); }
                if let Some(cap) = internet { table.entry("node_bandwidth_internet_down_mbps".to_string()).or_insert(toml::Value::Float(f64::from(cap))); }
            }
            inject_acl(table, &acl_chains);
            if let Ok(text) = toml::to_string(&value) { toml = text; }
        }
    }
    toml
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
            touch_node(&state.db, node_id, Some(hostname), Some(version), Some(&Value::Array(networks.clone())), None, "online").await;
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
            touch_node(&state.db, node_id, None, None, None, None, "offline").await;
        });
    }
}

fn spawn_edge_rate_sampler(state: AppState) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
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

    // ---- server-side data scoping: network and/or p2p group ------------------
    let mut node_list: Vec<Value> = nodes.values().cloned().collect();
    let mut edge_list: Vec<Value> = edges.values().cloned().collect();
    let mut cross_links: Vec<Value> = Vec::new();
    let peer_group: std::collections::HashMap<u64, String> = node_list
        .iter()
        .filter_map(|n| {
            let id = n.get("peer_id").and_then(|v| v.as_u64())?;
            let g = n
                .get("p2p_group")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Some((id, g))
        })
        .collect();
    let scoped = net_filter.is_some() || group_filter.is_some();
    if scoped {
        let allowed: std::collections::HashSet<u64> = node_list
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
            .filter_map(|n| n.get("peer_id").and_then(|v| v.as_u64()))
            .collect();
        if group_filter.is_some() {
            let mut seen: std::collections::HashSet<(u64, String)> = std::collections::HashSet::new();
            for e in &edge_list {
                let a = e.get("a").and_then(|v| v.as_u64()).unwrap_or(0);
                let b = e.get("b").and_then(|v| v.as_u64()).unwrap_or(0);
                let in_a = allowed.contains(&a);
                let in_b = allowed.contains(&b);
                if in_a == in_b {
                    continue;
                }
                let (inner, outer) = if in_a { (a, b) } else { (b, a) };
                let target = peer_group.get(&outer).cloned().unwrap_or_default();
                if target.is_empty() {
                    continue;
                }
                if seen.insert((inner, target.clone())) {
                    cross_links.push(json!({"peer_id": inner, "group": target}));
                }
            }
        }
        node_list.retain(|n| {
            n.get("peer_id")
                .and_then(|v| v.as_u64())
                .map(|id| allowed.contains(&id))
                .unwrap_or(false)
        });
        edge_list.retain(|e| {
            let a = e.get("a").and_then(|v| v.as_u64()).unwrap_or(0);
            let b = e.get("b").and_then(|v| v.as_u64()).unwrap_or(0);
            if !allowed.contains(&a) || !allowed.contains(&b) {
                return false;
            }
            net_filter
                .map(|f| e.get("network_name").and_then(|v| v.as_str()) == Some(f))
                .unwrap_or(true)
        });
    }
    json!({"graph": {
        "nodes": node_list,
        "edges": edge_list,
        "groups": groups,
        "cross_links": cross_links,
    }})
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

async fn board_events(
    State(state): State<AppState>,
    axum::extract::Query(query): axum::extract::Query<TopologyQuery>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let net = query.network.filter(|s| !s.is_empty());
    let grp = query.group.filter(|s| !s.is_empty());
    let stream = futures::stream::unfold((state, net, grp), |(state, net, grp)| async move {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        let payload = topology_payload(&state, net.as_deref(), grp.as_deref()).await;
        let nodes = nodes_payload(&state).await;
        let topology = Event::default().event("topology").data(
            json!({"type": "topology", "graph": payload.get("graph").cloned().unwrap_or(Value::Null)}).to_string(),
        );
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

async fn resolve_peer_uuid(db: &AnyPool, network: &str, peer: i64) -> Option<String> {
    let rows: Vec<(String,)> = sqlx::query_as(&q("SELECT snapshot FROM nodes"))
        .fetch_all(db)
        .await
        .ok()?;
    for (snapshot,) in rows {
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
    let a_uuid = resolve_peer_uuid(&state.db, &query.network, query.a).await;
    let b_uuid = resolve_peer_uuid(&state.db, &query.network, query.b).await;
    let rows: Result<Vec<(i64, Option<f64>, Option<f64>)>, sqlx::Error> = match (a_uuid, b_uuid) {
        (Some(ua), Some(ub)) => {
            let (lo, hi) = if ua <= ub { (ua, ub) } else { (ub, ua) };
            sqlx::query_as(&q(
                "SELECT (ts / ?) * ? AS bucket, AVG(latency_ms), AVG(loss_pct) FROM link_metrics WHERE network = ? AND a_uuid = ? AND b_uuid = ? AND ts > ? GROUP BY bucket ORDER BY bucket",
            )).bind(step).bind(step).bind(&query.network).bind(&lo).bind(&hi).bind(since)
              .fetch_all(&state.db).await
        }
        _ => {
            sqlx::query_as(&q(
                "SELECT (ts / ?) * ? AS bucket, AVG(latency_ms), AVG(loss_pct) FROM link_metrics WHERE network = ? AND a = ? AND b = ? AND ts > ? GROUP BY bucket ORDER BY bucket",
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
    let rows: Vec<(String, String)> = sqlx::query_as("SELECT node_id, snapshot FROM nodes")
        .fetch_all(&state.db).await.unwrap_or_default();
    let mut routes = Vec::new();
    for (_, snapshot) in &rows {
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
    let rows: Vec<(String, String)> = sqlx::query_as("SELECT node_id, snapshot FROM nodes")
        .fetch_all(&state.db).await.unwrap_or_default();
    let mut names: std::collections::BTreeMap<u32, String> = std::collections::BTreeMap::new();
    let mut links_map: std::collections::BTreeMap<(u32, u32, String), Value> = std::collections::BTreeMap::new();
    for (_, snapshot) in &rows {
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
        geo_attempts: std::sync::Arc::new(dashmap::DashMap::new()) };
    spawn_bandwidth_sampler(state.clone());
    spawn_edge_rate_sampler(state.clone());
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
        .route("/api/board/nodes", get(list_nodes))
        .route("/api/board/nodes/{node_id}", get(get_node))
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
