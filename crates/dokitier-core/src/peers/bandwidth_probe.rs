//! Active link-capacity probing over the peer RPC channel.
//!
//! A probe asks the remote peer to echo a payload of a requested size; the
//! requester measures the transfer time (minus the ping RTT) and derives a
//! one-way capacity estimate. Probes are rate limited and only run while the
//! link is idle; see the sampler in the core agent.

use crate::proto::{
    peer_rpc::{BandwidthProbeRequest, BandwidthProbeResponse, BandwidthProbeRpc},
    rpc_types::{self, controller::BaseController},
};

pub const MAX_PROBE_BYTES: u32 = 128 * 1024;
pub const DEFAULT_PROBE_BYTES: u32 = 256 * 1024;

#[derive(Debug, Clone, Copy)]
pub struct ProbeConfig {
    pub enabled: bool,
    pub interval_secs: u64,
    pub payload_bytes: u32,
    pub repeats: u32,
    pub max_concurrent: u32,
    pub idle_utilization_pct: f64,
    pub timeout_ms: u64,
    pub max_range_mbps: f64,
}

impl Default for ProbeConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            interval_secs: 3600,
            payload_bytes: DEFAULT_PROBE_BYTES,
            repeats: 2,
            max_concurrent: 2,
            idle_utilization_pct: 20.0,
            timeout_ms: 5000,
            max_range_mbps: 5000.0,
        }
    }
}

static PROBE_CONFIG: std::sync::OnceLock<std::sync::RwLock<ProbeConfig>> =
    std::sync::OnceLock::new();

fn probe_config() -> &'static std::sync::RwLock<ProbeConfig> {
    PROBE_CONFIG.get_or_init(|| std::sync::RwLock::new(ProbeConfig::default()))
}

pub fn set_probe_config(config: ProbeConfig) {
    let mut current = probe_config().write().unwrap();
    *current = ProbeConfig {
        payload_bytes: config.payload_bytes.clamp(64 * 1024, MAX_PROBE_BYTES),
        interval_secs: config.interval_secs.max(60),
        repeats: config.repeats.clamp(1, 5),
        max_concurrent: config.max_concurrent.clamp(1, 4),
        idle_utilization_pct: config.idle_utilization_pct.clamp(1.0, 95.0),
        timeout_ms: config.timeout_ms.clamp(500, 30_000),
        max_range_mbps: config.max_range_mbps.clamp(1.0, 100_000.0),
        enabled: config.enabled,
    };
}

pub fn get_probe_config() -> ProbeConfig {
    *probe_config().read().unwrap()
}

#[derive(Clone, Default)]
pub struct BandwidthProbeServerImpl;

#[async_trait::async_trait]
impl BandwidthProbeRpc for BandwidthProbeServerImpl {
    type Controller = BaseController;

    async fn probe(
        &self,
        _: BaseController,
        request: BandwidthProbeRequest,
    ) -> rpc_types::error::Result<BandwidthProbeResponse> {
        let size = request.size_bytes.min(MAX_PROBE_BYTES) as usize;
        Ok(BandwidthProbeResponse {
            nonce: request.nonce,
            payload: vec![0u8; size],
        })
    }
}

/// Derive link capacity in Mbps from a burst transfer.
///
/// `elapsed` is the whole RPC duration, `rtt` the ping round trip (both
/// directions of which are not useful for the burst), and `payload_bytes` the
/// bytes actually returned.
pub fn capacity_from_probe(payload_bytes: u64, elapsed_secs: f64, rtt_secs: f64) -> Option<f64> {
    if payload_bytes == 0 || !elapsed_secs.is_finite() || elapsed_secs <= 0.0 {
        return None;
    }
    let transfer = (elapsed_secs - rtt_secs).max(elapsed_secs * 0.2);
    if transfer <= 0.0 {
        return None;
    }
    let mbps = payload_bytes as f64 * 8.0 / transfer / 1_000_000.0;
    if !mbps.is_finite() || mbps <= 0.0 {
        return None;
    }
    Some(mbps.clamp(1.0, 100_000.0))
}
