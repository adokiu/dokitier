use std::collections::{HashMap, VecDeque};

use serde::{Deserialize, Serialize};

/// Number of 1-second samples kept in memory (60 seconds of realtime data).
pub const REALTIME_WINDOW: usize = 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LinkKey {
    pub a_peer_id: u32,
    pub b_peer_id: u32,
}

impl LinkKey {
    pub fn new(a: u32, b: u32) -> Self {
        // Canonical ordering so both endpoints report the same key.
        if a <= b {
            Self {
                a_peer_id: a,
                b_peer_id: b,
            }
        } else {
            Self {
                a_peer_id: b,
                b_peer_id: a,
            }
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MetricPoint {
    pub ts: i64,
    pub latency_ms: Option<f32>,
    pub loss_pct: Option<f32>,
    pub rx_bps: Option<f64>,
    pub tx_bps: Option<f64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TunnelStat {
    #[serde(default)]
    pub id: String,
    pub tunnel_type: String,
    #[serde(default)]
    pub remote_addr: Option<String>,
    pub latency_ms: Option<f32>,
    pub loss_pct: Option<f32>,
    pub rx_bps: Option<f64>,
    pub tx_bps: Option<f64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LinkMetrics {
    pub a_peer_id: u32,
    pub b_peer_id: u32,
    pub tunnel_types: Vec<String>,
    pub latency_ms: Option<f32>,
    pub loss_pct: Option<f32>,
    pub rx_bps: Option<f64>,
    pub tx_bps: Option<f64>,
    pub bandwidth_mbps: Option<f32>,
    pub source_peer_id: Option<u32>,
    /// Per-protocol breakdown for parallel links between the same peer pair.
    #[serde(default)]
    pub tunnels: Vec<TunnelStat>,
    pub updated_at: i64,
    /// Newest last; capped at REALTIME_WINDOW.
    #[serde(default, skip_serializing_if = "VecDeque::is_empty")]
    pub samples: VecDeque<MetricPoint>,
}

/// One row persisted to SQLite every 60 seconds.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricSample {
    pub ts: i64,
    pub a_peer_id: u32,
    pub b_peer_id: u32,
    pub latency_ms: Option<f32>,
    pub loss_pct: Option<f32>,
    pub rx_bps: Option<f64>,
    pub tx_bps: Option<f64>,
    pub bandwidth_mbps: Option<f32>,
}

#[derive(Default)]
pub struct MetricsHub {
    links: tokio::sync::RwLock<HashMap<LinkKey, LinkMetrics>>,
}

impl MetricsHub {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one 1-second sample for a link.
    pub async fn record(&self, key: LinkKey, point: MetricPoint) {
        let mut links = self.links.write().await;
        let entry = links.entry(key).or_insert_with(|| LinkMetrics {
            a_peer_id: key.a_peer_id,
            b_peer_id: key.b_peer_id,
            ..Default::default()
        });
        entry.latency_ms = point.latency_ms.or(entry.latency_ms);
        entry.loss_pct = point.loss_pct.or(entry.loss_pct);
        entry.rx_bps = point.rx_bps.or(entry.rx_bps);
        entry.tx_bps = point.tx_bps.or(entry.tx_bps);
        entry.updated_at = point.ts;
        entry.samples.push_back(point);
        while entry.samples.len() > REALTIME_WINDOW {
            entry.samples.pop_front();
        }
    }

    pub async fn set_static_info(
        &self,
        key: LinkKey,
        tunnel_types: Vec<String>,
        bandwidth_mbps: Option<f32>,
        source_peer_id: Option<u32>,
    ) {
        let mut links = self.links.write().await;
        let entry = links.entry(key).or_insert_with(|| LinkMetrics {
            a_peer_id: key.a_peer_id,
            b_peer_id: key.b_peer_id,
            ..Default::default()
        });
        entry.tunnel_types = tunnel_types;
        entry.bandwidth_mbps = bandwidth_mbps;
        entry.source_peer_id = source_peer_id;
    }

    pub async fn set_tunnels(&self, key: LinkKey, tunnels: Vec<TunnelStat>) {
        let mut links = self.links.write().await;
        let entry = links.entry(key).or_insert_with(|| LinkMetrics {
            a_peer_id: key.a_peer_id,
            b_peer_id: key.b_peer_id,
            ..Default::default()
        });
        entry.tunnels = tunnels;
    }

    pub async fn realtime_snapshot(&self) -> Vec<LinkMetrics> {
        self.links.read().await.values().cloned().collect()
    }

    /// Latest point of every link, used by the 60-second persistence worker.
    pub async fn history_snapshot(&self) -> Vec<MetricSample> {
        self.links
            .read()
            .await
            .values()
            .filter_map(|link| {
                let point = link.samples.back()?;
                Some(MetricSample {
                    ts: point.ts,
                    a_peer_id: link.a_peer_id,
                    b_peer_id: link.b_peer_id,
                    latency_ms: point.latency_ms.or(link.latency_ms),
                    loss_pct: point.loss_pct.or(link.loss_pct),
                    rx_bps: point.rx_bps.or(link.rx_bps),
                    tx_bps: point.tx_bps.or(link.tx_bps),
                    bandwidth_mbps: link.bandwidth_mbps,
                })
            })
            .collect()
    }

    pub async fn remove(&self, key: LinkKey) {
        self.links.write().await.remove(&key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn ring_is_capped_and_latest_wins() {
        let hub = MetricsHub::new();
        let key = LinkKey::new(2, 1);
        for i in 0..100 {
            hub.record(
                key,
                MetricPoint {
                    ts: i,
                    latency_ms: Some(10.0),
                    ..Default::default()
                },
            )
            .await;
        }
        let snap = hub.realtime_snapshot().await;
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].samples.len(), REALTIME_WINDOW);
        assert_eq!(snap[0].a_peer_id, 1);
        assert_eq!(snap[0].updated_at, 99);

        let history = hub.history_snapshot().await;
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].ts, 99);
    }
}
