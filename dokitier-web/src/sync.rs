//! Peer-to-peer configuration sync.
//!
//! Every node pulls `/api/sync/export` from the other nodes' embedded consoles
//! over the overlay network and merges the records with last-write-wins by HLC.
//! Pull-only convergence is enough: each node pulls from every peer it knows.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use tokio::sync::RwLock;

use serde_json::Value;

use crate::store::SyncRecord;
use crate::{DataSource, Store};

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct PeerSyncStatus {
    pub peer: String,
    pub ok: bool,
    pub applied: usize,
    pub conflicts: u32,
    pub failures: u32,
    pub next_retry: i64,
    pub last_success: Option<i64>,
    pub last_error: Option<String>,
}

#[derive(Default)]
pub struct SyncRuntime {
    peers: RwLock<HashMap<String, PeerSyncStatus>>,
    last_run: AtomicI64,
}

impl SyncRuntime {
    pub fn touch(&self) {
        self.last_run
            .store(chrono::Utc::now().timestamp(), Ordering::Relaxed);
    }

    pub async fn record_ok(&self, peer: &str, applied: usize, conflicts: usize) {
        let mut peers = self.peers.write().await;
        let entry = peers.entry(peer.to_string()).or_default();
        entry.peer = peer.to_string();
        entry.ok = true;
        entry.applied = applied;
        entry.conflicts = conflicts as u32;
        entry.failures = 0;
        entry.next_retry = 0;
        entry.last_success = Some(chrono::Utc::now().timestamp());
        entry.last_error = None;
    }

    pub async fn record_error(&self, peer: &str, error: impl Into<String>) {
        let mut peers = self.peers.write().await;
        let entry = peers.entry(peer.to_string()).or_default();
        entry.peer = peer.to_string();
        entry.ok = false;
        entry.failures = entry.failures.saturating_add(1);
        // 5s, 10s, 20s, 40s, ... capped at 300s.
        let delay = 5i64
            .saturating_mul(1i64 << entry.failures.min(6))
            .min(300);
        entry.next_retry = chrono::Utc::now().timestamp() + delay;
        entry.last_error = Some(error.into());
    }

    /// False while the peer is in backoff after repeated failures.
    pub async fn should_try(&self, peer: &str) -> bool {
        let peers = self.peers.read().await;
        match peers.get(peer) {
            Some(status) if status.next_retry > chrono::Utc::now().timestamp() => false,
            _ => true,
        }
    }

    pub async fn snapshot(&self) -> serde_json::Value {
        let peers: Vec<PeerSyncStatus> = self.peers.read().await.values().cloned().collect();
        serde_json::json!({
            "last_run": self.last_run.load(Ordering::Relaxed),
            "peers": peers,
        })
    }
}

/// Pull every peer once and merge. Returns the number of applied records.
pub async fn pull_once(
    client: &reqwest::Client,
    store: &Store,
    runtime: &SyncRuntime,
    token: &str,
    peers: &[String],
) -> usize {
    let mut total = 0;
    for ip in peers {
        if !runtime.should_try(ip).await {
            continue;
        }
        let url = format!("http://{ip}/api/sync/export");
        let response = match client
            .get(&url)
            .header("x-dokitier-sync-token", token)
            .send()
            .await
        {
            Ok(response) => response,
            Err(err) => {
                runtime.record_error(ip, err.to_string()).await;
                continue;
            }
        };
        if !response.status().is_success() {
            runtime
                .record_error(ip, format!("HTTP {}", response.status()))
                .await;
            continue;
        }
        let body = match response.json::<Value>().await {
            Ok(body) => body,
            Err(err) => {
                runtime.record_error(ip, err.to_string()).await;
                continue;
            }
        };
        let records: Vec<SyncRecord> =
            serde_json::from_value(body.get("records").cloned().unwrap_or(Value::Null))
                .unwrap_or_default();
        if records.is_empty() {
            continue;
        }
        match store.apply_sync_records_with_stats(&records).await {
            Ok(stats) => {
                if stats.applied > 0 {
                    tracing::info!(peer = %ip, applied = stats.applied, "applied remote config records");
                    total += stats.applied;
                }
                runtime.record_ok(ip, stats.applied, stats.conflicts).await;
            }
            Err(err) => {
                tracing::warn!(%err, peer = %ip, "failed to apply remote config");
                runtime.record_error(ip, err.to_string()).await;
            }
        }
    }
    total
}

/// Pull per-link metrics from peers so the topology can show edges that are
/// not directly attached to this node.
pub async fn pull_link_metrics(
    client: &reqwest::Client,
    cache: &tokio::sync::RwLock<std::collections::HashMap<(u32, u32), crate::metrics::LinkMetrics>>,
    token: &str,
    peers: &[String],
) {
    for ip in peers {
        let url = format!("http://{ip}/api/links/export");
        let Ok(response) = client
            .get(&url)
            .header("x-dokitier-sync-token", token)
            .send()
            .await
        else {
            continue;
        };
        if !response.status().is_success() {
            continue;
        }
        let Ok(body) = response.json::<Value>().await else {
            continue;
        };
        let links: Vec<crate::metrics::LinkMetrics> =
            serde_json::from_value(body.get("links").cloned().unwrap_or(Value::Null))
                .unwrap_or_default();
        if links.is_empty() {
            continue;
        }
        let mut guard = cache.write().await;
        for link in links {
            guard.insert((link.a_peer_id, link.b_peer_id), link);
        }
    }
}

pub fn spawn(
    data: Arc<dyn DataSource>,
    store: Arc<Store>,
    remote_metrics: Arc<tokio::sync::RwLock<std::collections::HashMap<(u32, u32), crate::metrics::LinkMetrics>>>,
    token: String,
    port: u16,
    override_peers: Vec<String>,
    runtime: Arc<SyncRuntime>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let client = match reqwest::Client::builder()
            .timeout(Duration::from_secs(3))
            .build()
        {
            Ok(client) => client,
            Err(err) => {
                tracing::warn!(%err, "config sync disabled: failed to build http client");
                return;
            }
        };

        loop {
            tokio::time::sleep(Duration::from_secs(5)).await;
            let peers = if override_peers.is_empty() {
                data.sync_peer_ips()
                    .await
                    .into_iter()
                    .map(|ip| format!("{ip}:{port}"))
                    .collect()
            } else {
                override_peers.clone()
            };
            runtime.touch();
            let applied = pull_once(&client, &store, &runtime, &token, &peers).await;
            if applied > 0 {
                data.on_config_changed("sync", "*").await;
            }
            pull_link_metrics(&client, &remote_metrics, &token, &peers).await;
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{LinkConfig, StaticRoute};
    use axum::{routing::get, Json, Router};
    use serde_json::json;

    fn temp_db() -> String {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir()
            .join(format!("dokitier-sync-{nanos}.db"))
            .to_string_lossy()
            .to_string()
    }

    #[tokio::test]
    async fn pulls_and_materializes_peer_records() {
        let link = LinkConfig {
            a_peer_id: 11,
            b_peer_id: 22,
            bandwidth_mbps: Some(80.0),
            latency_weight: Some(1.5),
            loss_weight: Some(2.5),
            bandwidth_weight: Some(3.5),
            disabled: false,
            note: "from-peer".into(),
            hlc: "123:0:peer".into(),
            updated_at: 1,
        };
        let records = json!({
            "records": [{
                "key": "link:11|22",
                "namespace": "link",
                "value": serde_json::to_string(&link).unwrap(),
                "hlc": link.hlc,
                "origin": "peer",
                "deleted": false,
                "updated_at": 1,
            }]
        });
        let payload = records.clone();
        let app = Router::new().route(
            "/api/sync/export",
            get(move || {
                let payload = payload.clone();
                async move { Json(payload) }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        let store = Store::open(&temp_db()).await.unwrap();
        let client = reqwest::Client::new();
        let runtime = SyncRuntime::default();
        let applied = pull_once(
            &client,
            &store,
            &runtime,
            "token",
            &[format!("127.0.0.1:{port}")],
        )
        .await;
        assert_eq!(applied, 1);

        let links = store.list_links().await.unwrap();
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].bandwidth_mbps, Some(80.0));

        // Static route records also materialize.
        let route = StaticRoute {
            id: 0,
            dest_cidr: "10.8.0.0/16".into(),
            next_hop_peer_id: 22,
            metric: 5,
            enabled: true,
            note: String::new(),
            hlc: "124:0:peer".into(),
        };
        let rec = SyncRecord {
            key: "static_route:10.8.0.0/16|22".into(),
            namespace: "static_route".into(),
            value: serde_json::to_string(&route).unwrap(),
            hlc: route.hlc.clone(),
            origin: "peer".into(),
            deleted: false,
            updated_at: 1,
        };
        store.apply_sync_records(&[rec]).await.unwrap();
        assert_eq!(store.list_static_routes().await.unwrap().len(), 1);
    }
}

#[cfg(test)]
mod backoff_tests {
    use super::*;

    #[tokio::test]
    async fn failing_peer_enters_backoff_then_recovers() {
        let runtime = SyncRuntime::default();
        assert!(runtime.should_try("peer").await);
        runtime.record_error("peer", "timeout").await;
        assert!(!runtime.should_try("peer").await);
        runtime.record_ok("peer", 1, 0).await;
        assert!(runtime.should_try("peer").await);
        let snapshot = runtime.snapshot().await;
        assert_eq!(snapshot["peers"][0]["failures"], 0);
    }
}
