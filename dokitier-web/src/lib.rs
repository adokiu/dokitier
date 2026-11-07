//! dokitier embedded web console.
//!
//! The console is disabled unless `[web_console] enabled = true` is present in
//! the node config. It embeds the built SPA and exposes a JSON API plus a
//! WebSocket stream with 1-second link metrics.

pub mod api;
pub mod auth;
pub mod config;
pub mod metrics;
pub mod store;
pub mod sync;

pub use config::WebConsoleConfig;
pub use metrics::{LinkMetrics, MetricsHub};
pub use store::{LinkConfig, StaticRoute, Store};

use std::sync::Arc;

use anyhow::Context;

/// Data source implemented by the core instance.
///
/// The web crate must not depend on the core crate (avoid a dependency cycle),
/// so the core injects a snapshot provider.
#[async_trait::async_trait]
pub trait DataSource: Send + Sync {
    async fn overview(&self) -> serde_json::Value;
    async fn topology(&self) -> serde_json::Value;
    async fn nodes(&self) -> serde_json::Value;
    async fn node(&self, peer_id: u32) -> Option<serde_json::Value>;
    async fn routes(&self) -> serde_json::Value;
    async fn sync_status(&self) -> serde_json::Value {
        serde_json::json!({})
    }
    /// Shared token used by peer-to-peer configuration sync.
    async fn sync_token(&self) -> Option<String> {
        None
    }
    /// Peer overlay addresses (IPv4 without prefix) for sync.
    async fn sync_peer_ips(&self) -> Vec<String> {
        Vec::new()
    }
    /// Called after the web console persisted a configuration change.
    async fn on_config_changed(&self, _namespace: &str, _key: &str) {}
    /// Replace the running network instance with `toml` without restarting the
    /// process (listeners and peer connectors are recreated atomically).
    async fn apply_config(&self, _toml: &str) -> anyhow::Result<()> {
        anyhow::bail!("config apply is not supported by this data source")
    }
}

/// Open the SQLite store for the console.
pub async fn open_store(config: &WebConsoleConfig) -> anyhow::Result<Arc<Store>> {
    Ok(Arc::new(Store::open(&config.db_path).await?))
}

/// Start the console. Returns `Ok(None)` when the console is disabled.
pub async fn start(
    config: WebConsoleConfig,
    store: Arc<Store>,
    hub: Arc<MetricsHub>,
    data: Arc<dyn DataSource>,
) -> anyhow::Result<Option<tokio::task::JoinHandle<()>>> {
    if !config.enabled {
        tracing::info!("web console disabled (web_console.enabled = false)");
        return Ok(None);
    }
    config.validate()?;

    // Persist a snapshot every 60 seconds.
    {
        let store = store.clone();
        let hub = hub.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(std::time::Duration::from_secs(60));
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                ticker.tick().await;
                let samples = hub.history_snapshot().await;
                if samples.is_empty() {
                    continue;
                }
                if let Err(err) = store.insert_metrics(&samples).await {
                    tracing::warn!(%err, "failed to persist link metrics");
                }
                let cutoff = chrono::Utc::now().timestamp() - 7 * 24 * 3600;
                let _ = store.prune_metrics(cutoff).await;
            }
        });
    }

    let sync_token = data.sync_token().await;
    let sync_runtime = Arc::new(sync::SyncRuntime::default());
    let state = api::AppState::new(
        config.clone(),
        store.clone(),
        hub,
        data.clone(),
        sync_token.clone(),
        sync_runtime.clone(),
    );
    let listener = tokio::net::TcpListener::bind(&config.listen)
        .await
        .with_context(|| format!("web console failed to bind {}", config.listen))?;
    tracing::info!(listen = %config.listen, "web console listening");

    if let Some(token) = sync_token {
        let port = config
            .listen
            .rsplit(':')
            .next()
            .and_then(|p| p.parse::<u16>().ok())
            .unwrap_or(11211);
        sync::spawn(
            data.clone(),
            store,
            state.remote_metrics.clone(),
            token,
            port,
            config.sync_peers.clone(),
            sync_runtime,
        );
    }

    let app = api::router(state);
    Ok(Some(tokio::spawn(async move {
        if let Err(err) = axum::serve(listener, app).await {
            tracing::error!(%err, "web console server stopped");
        }
    })))
}
