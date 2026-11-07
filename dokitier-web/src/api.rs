use std::collections::HashMap;
use tokio::sync::RwLock;
use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use chrono::Utc;
use include_dir::{include_dir, Dir};
use serde::Deserialize;
use serde_json::json;

use crate::auth::{hash_password, verify_password, Sessions};
use crate::config::WebConsoleConfig;
use crate::metrics::MetricsHub;
use crate::store::{LinkConfig, StaticRoute, Store};
use crate::DataSource;

pub const SESSION_COOKIE: &str = "dokitier_session";

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<WebConsoleConfig>,
    pub store: Arc<Store>,
    pub hub: Arc<MetricsHub>,
    pub data: Arc<dyn DataSource>,
    pub sessions: Arc<Sessions>,
    pub password_hash: Arc<String>,
    pub sync_token: Option<String>,
    pub remote_metrics: Arc<RwLock<HashMap<(u32, u32), crate::metrics::LinkMetrics>>>,
    pub sync_runtime: Arc<crate::sync::SyncRuntime>,
}

impl AppState {
    pub fn new(
        config: WebConsoleConfig,
        store: Arc<Store>,
        hub: Arc<MetricsHub>,
        data: Arc<dyn DataSource>,
        sync_token: Option<String>,
        sync_runtime: Arc<crate::sync::SyncRuntime>,
    ) -> Self {
        let password_hash = hash_password(&config.password).expect("password hashing failed");
        let sessions = Sessions::new(config.session_ttl_hours);
        Self {
            config: Arc::new(config),
            store,
            hub,
            data,
            sessions: Arc::new(sessions),
            password_hash: Arc::new(password_hash),
            sync_token,
            remote_metrics: Arc::new(RwLock::new(HashMap::new())),
            sync_runtime,
        }
    }
}

pub fn router(state: AppState) -> Router {
    let protected = Router::new()
        .route("/api/overview", get(overview))
        .route("/api/topology", get(topology))
        .route("/api/config", get(get_config).put(save_config))
        .route("/api/nodes", get(nodes))
        .route("/api/nodes/{peer_id}", get(node))
        .route("/api/links", get(links))
        .route("/api/links/{a}/{b}", put(update_link))
        .route("/api/routes", get(routes))
        .route("/api/static-routes", post(create_static_route))
        .route("/api/static-routes/{id}", delete(remove_static_route))
        .route("/api/settings", get(get_settings).put(put_settings))
        .route("/api/history", get(history))
        .route("/api/sync", get(sync_status))
        .route("/api/events", get(events))
        .route_layer(middleware::from_fn_with_state(state.clone(), require_auth));

    Router::new()
        .route("/api/login", post(login))
        .route("/api/logout", post(logout))
        .route("/api/sync/export", get(sync_export))
        .route("/api/sync/import", post(sync_import))
        .route("/api/links/export", get(links_export))
        .merge(protected)
        .fallback(static_assets)
        .with_state(state)
}

#[derive(Deserialize)]
struct LoginRequest {
    password: String,
}

async fn login(
    State(state): State<AppState>,
    Json(req): Json<LoginRequest>,
) -> Response {
    if !verify_password(&req.password, &state.password_hash) {
        return (StatusCode::UNAUTHORIZED, Json(json!({"error": "invalid password"}))).into_response();
    }
    let token = state.sessions.create().await;
    let cookie = format!(
        "{SESSION_COOKIE}={token}; HttpOnly; SameSite=Strict; Path=/; Max-Age={}",
        state.config.session_ttl_hours.max(1) * 3600
    );
    let mut response = Json(json!({"ok": true})).into_response();
    response
        .headers_mut()
        .insert(header::SET_COOKIE, cookie.parse().unwrap());
    response
}

async fn logout(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(token) = session_token(&headers) {
        state.sessions.remove(&token).await;
    }
    let mut response = Json(json!({"ok": true})).into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        format!("{SESSION_COOKIE}=; Path=/; Max-Age=0").parse().unwrap(),
    );
    response
}

fn session_token(headers: &HeaderMap) -> Option<String> {
    let cookies = headers.get(header::COOKIE)?.to_str().ok()?;
    cookies.split(';').find_map(|part| {
        let part = part.trim();
        part.strip_prefix(&format!("{SESSION_COOKIE}="))
            .map(|value| value.to_string())
    })
}

async fn require_auth(
    State(state): State<AppState>,
    headers: HeaderMap,
    request: axum::extract::Request,
    next: Next,
) -> Response {
    let token = session_token(&headers);
    match token {
        Some(token) if state.sessions.validate(&token).await => next.run(request).await,
        _ => (StatusCode::UNAUTHORIZED, Json(json!({"error": "unauthorized"}))).into_response(),
    }
}

async fn overview(State(state): State<AppState>) -> Json<serde_json::Value> {
    Json(state.data.overview().await)
}

async fn topology(State(state): State<AppState>) -> Json<serde_json::Value> {
    let graph = state.data.topology().await;
    let local = state.hub.realtime_snapshot().await;
    let mut merged: HashMap<(u32, u32), crate::metrics::LinkMetrics> = state
        .remote_metrics
        .read()
        .await
        .iter()
        .map(|(k, v)| (*k, v.clone()))
        .collect();
    for metric in local {
        merged.insert((metric.a_peer_id, metric.b_peer_id), metric);
    }
    let metrics: Vec<_> = merged.into_values().collect();
    Json(json!({ "graph": graph, "metrics": metrics }))
}

#[derive(Deserialize)]
struct SaveConfigBody {
    toml: String,
}

async fn get_config(State(state): State<AppState>) -> Response {
    let Some(path) = state.config.config_path.as_ref() else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "node was not started from a config file"})),
        )
            .into_response();
    };
    match tokio::fs::read_to_string(path).await {
        Ok(toml) => Json(json!({"path": path.display().to_string(), "toml": toml})).into_response(),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": err.to_string()})),
        )
            .into_response(),
    }
}

async fn save_config(
    State(state): State<AppState>,
    Json(body): Json<SaveConfigBody>,
) -> Response {
    let Some(path) = state.config.config_path.as_ref() else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "node was not started from a config file"})),
        )
            .into_response();
    };
    if let Err(err) = toml::from_str::<toml::Value>(&body.toml) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("TOML 语法错误: {err}")})),
        )
            .into_response();
    }
    let tmp = path.with_extension("toml.tmp");
    if let Err(err) = tokio::fs::write(&tmp, body.toml.as_bytes()).await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": err.to_string()})),
        )
            .into_response();
    }
    if let Err(err) = tokio::fs::rename(&tmp, path).await {
        let _ = tokio::fs::remove_file(&tmp).await;
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": err.to_string()})),
        )
            .into_response();
    }
    Json(json!({"ok": true, "restart_required": true})).into_response()
}

async fn nodes(State(state): State<AppState>) -> Json<serde_json::Value> {
    Json(state.data.nodes().await)
}

async fn node(State(state): State<AppState>, Path(peer_id): Path<u32>) -> Response {
    match state.data.node(peer_id).await {
        Some(value) => Json(value).into_response(),
        None => (StatusCode::NOT_FOUND, Json(json!({"error": "node not found"}))).into_response(),
    }
}

async fn links_export(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if !sync_authorized(&headers, &state) {
        return (StatusCode::UNAUTHORIZED, Json(json!({"error": "unauthorized"}))).into_response();
    }
    let metrics = state.hub.realtime_snapshot().await;
    Json(json!({ "links": metrics })).into_response()
}

async fn links(State(state): State<AppState>) -> Json<serde_json::Value> {
    let configs = state.store.list_links().await.unwrap_or_default();
    let mut metrics = state.hub.realtime_snapshot().await;
    for (key, value) in state.remote_metrics.read().await.iter() {
        if !metrics
            .iter()
            .any(|m| (m.a_peer_id, m.b_peer_id) == *key)
        {
            metrics.push(value.clone());
        }
    }
    let mut map: HashMap<(u32, u32), LinkConfig> = configs
        .into_iter()
        .map(|link| ((link.a_peer_id, link.b_peer_id), link))
        .collect();
    let mut merged: Vec<serde_json::Value> = metrics
        .into_iter()
        .map(|metric| {
            let key = (metric.a_peer_id, metric.b_peer_id);
            let config = map.remove(&key);
            json!({ "metrics": metric, "config": config })
        })
        .collect();
    // Config-only links (no metrics yet) should still be listed and editable.
    for (_, config) in map {
        merged.push(json!({ "metrics": null, "config": config }));
    }
    Json(json!({ "links": merged }))
}

#[derive(Deserialize, Default)]
struct LinkUpdate {
    bandwidth_mbps: Option<f64>,
    latency_weight: Option<f64>,
    loss_weight: Option<f64>,
    bandwidth_weight: Option<f64>,
    #[serde(default)]
    disabled: bool,
    #[serde(default)]
    note: String,
}

async fn update_link(
    State(state): State<AppState>,
    Path((a, b)): Path<(u32, u32)>,
    Json(update): Json<LinkUpdate>,
) -> Response {
    let (a_peer_id, b_peer_id) = if a <= b { (a, b) } else { (b, a) };
    let now = Utc::now();
    let link = LinkConfig {
        a_peer_id,
        b_peer_id,
        bandwidth_mbps: update.bandwidth_mbps,
        latency_weight: update.latency_weight,
        loss_weight: update.loss_weight,
        bandwidth_weight: update.bandwidth_weight,
        disabled: update.disabled,
        note: update.note,
        hlc: format!("{}:0:{}", now.timestamp_millis(), "web"),
        updated_at: now.timestamp(),
    };
    if let Err(err) = state.store.upsert_link(&link).await {
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": err.to_string()}))).into_response();
    }
    state
        .data
        .on_config_changed("link", &format!("{a_peer_id}|{b_peer_id}"))
        .await;
    Json(json!({"ok": true})).into_response()
}

async fn routes(State(state): State<AppState>) -> Json<serde_json::Value> {
    let ospf = state.data.routes().await;
    let static_routes = state.store.list_static_routes().await.unwrap_or_default();
    Json(json!({ "ospf": ospf, "static": static_routes }))
}

#[derive(Deserialize)]
struct StaticRouteRequest {
    dest_cidr: String,
    next_hop_peer_id: u32,
    #[serde(default = "default_metric")]
    metric: i64,
    #[serde(default = "default_true")]
    enabled: bool,
    #[serde(default)]
    note: String,
}

fn default_metric() -> i64 {
    100
}
fn default_true() -> bool {
    true
}

async fn create_static_route(
    State(state): State<AppState>,
    Json(req): Json<StaticRouteRequest>,
) -> Response {
    if req.dest_cidr.parse::<cidr::Ipv4Cidr>().is_err() && req.dest_cidr.parse::<cidr::Ipv6Cidr>().is_err() {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "invalid cidr"}))).into_response();
    }
    let now = Utc::now();
    let route = StaticRoute {
        id: 0,
        dest_cidr: req.dest_cidr,
        next_hop_peer_id: req.next_hop_peer_id,
        metric: req.metric,
        enabled: req.enabled,
        note: req.note,
        hlc: format!("{}:0:{}", now.timestamp_millis(), "web"),
    };
    match state.store.insert_static_route(&route).await {
        Ok(id) => {
            state.data.on_config_changed("static_route", &id.to_string()).await;
            Json(json!({"ok": true, "id": id})).into_response()
        }
        Err(err) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": err.to_string()}))).into_response(),
    }
}

async fn remove_static_route(State(state): State<AppState>, Path(id): Path<i64>) -> Response {
    match state.store.delete_static_route(id).await {
        Ok(true) => {
            state.data.on_config_changed("static_route", &id.to_string()).await;
            Json(json!({"ok": true})).into_response()
        }
        Ok(false) => (StatusCode::NOT_FOUND, Json(json!({"error": "not found"}))).into_response(),
        Err(err) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": err.to_string()}))).into_response(),
    }
}

async fn get_settings(State(state): State<AppState>) -> Json<serde_json::Value> {
    let settings = state.store.get_settings().await.unwrap_or_default();
    Json(json!(settings))
}

async fn put_settings(
    State(state): State<AppState>,
    Json(settings): Json<HashMap<String, String>>,
) -> Response {
    let now = Utc::now();
    for (key, value) in &settings {
        let hlc = format!("{}:0:{}", now.timestamp_millis(), "web");
        if let Err(err) = state.store.set_setting(key, value, &hlc).await {
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": err.to_string()}))).into_response();
        }
    }
    state.data.on_config_changed("settings", "*").await;
    Json(json!({"ok": true})).into_response()
}

#[derive(Deserialize)]
struct HistoryQuery {
    a: u32,
    b: u32,
    #[serde(default = "default_range")]
    range: String,
}

fn default_range() -> String {
    "24h".to_string()
}

async fn history(
    State(state): State<AppState>,
    Query(query): Query<HistoryQuery>,
) -> Response {
    let (step, since) = match query.range.as_str() {
        "1h" => (60, Utc::now().timestamp() - 3600),
        "7d" => (600, Utc::now().timestamp() - 7 * 24 * 3600),
        "30d" => (3600, Utc::now().timestamp() - 30 * 24 * 3600),
        _ => (60, Utc::now().timestamp() - 24 * 3600),
    };
    let (a, b) = if query.a <= query.b {
        (query.a, query.b)
    } else {
        (query.b, query.a)
    };
    match state.store.query_metrics(a, b, since, step).await {
        Ok(points) => Json(json!({"points": points, "range": query.range})).into_response(),
        Err(err) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": err.to_string()}))).into_response(),
    }
}

async fn sync_status(State(state): State<AppState>) -> Json<serde_json::Value> {
    let local = state.store.sync_state().await.unwrap_or_else(|_| json!({}));
    let summary = state
        .store
        .sync_summary()
        .await
        .unwrap_or_else(|_| json!({}));
    let remote = state.data.sync_status().await;
    let runtime = state.sync_runtime.snapshot().await;
    Json(json!({"summary": summary, "local": local, "remote": remote, "runtime": runtime}))
}

fn sync_authorized(headers: &HeaderMap, state: &AppState) -> bool {
    let Some(expected) = state.sync_token.as_deref() else {
        return false;
    };
    headers
        .get("x-dokitier-sync-token")
        .and_then(|v| v.to_str().ok())
        .map(|v| v == expected)
        .unwrap_or(false)
}

async fn sync_export(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if !sync_authorized(&headers, &state) {
        return (StatusCode::UNAUTHORIZED, Json(json!({"error": "unauthorized"}))).into_response();
    }
    match state.store.list_sync_records().await {
        Ok(records) => Json(json!({ "records": records })).into_response(),
        Err(_) => Json(json!({ "records": [] })).into_response(),
    }
}

async fn sync_import(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> Response {
    if !sync_authorized(&headers, &state) {
        return (StatusCode::UNAUTHORIZED, Json(json!({"error": "unauthorized"}))).into_response();
    }
    let records: Vec<crate::store::SyncRecord> =
        serde_json::from_value(body.get("records").cloned().unwrap_or(json!([])))
            .unwrap_or_default();
    match state.store.apply_sync_records(&records).await {
        Ok(applied) => Json(json!({"ok": true, "applied": applied})).into_response(),
        Err(err) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": err.to_string()}))).into_response(),
    }
}

async fn events(State(state): State<AppState>, ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(move |socket| metrics_stream(socket, state))
}

async fn metrics_stream(mut socket: WebSocket, state: AppState) {
    let mut ticker = tokio::time::interval(std::time::Duration::from_secs(1));
    loop {
        ticker.tick().await;
        let payload = json!({
            "type": "metrics",
            "ts": Utc::now().timestamp(),
            "links": state.hub.realtime_snapshot().await,
        });
        if socket
            .send(Message::Text(payload.to_string().into()))
            .await
            .is_err()
        {
            break;
        }
    }
}

static ASSETS: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/webui/dist");

async fn static_assets(uri: axum::http::Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };
    let asset = ASSETS.get_file(path).or_else(|| ASSETS.get_file("index.html"));
    match asset {
        Some(file) => {
            let mime = mime_guess(file.path().to_str().unwrap_or(""));
            ([(header::CONTENT_TYPE, mime)], file.contents()).into_response()
        }
        None => (StatusCode::NOT_FOUND, "web ui not built").into_response(),
    }
}

fn mime_guess(path: &str) -> &'static str {
    match path.rsplit('.').next() {
        Some("js") => "text/javascript",
        Some("css") => "text/css",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("woff2") => "font/woff2",
        Some("json") => "application/json",
        _ => "text/html; charset=utf-8",
    }
}
