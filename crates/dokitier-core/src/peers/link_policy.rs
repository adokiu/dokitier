//! Process-wide link policy used by routing and the embedded web console.
//!
//! The web console persists link settings in SQLite and pushes the effective
//! values here. The OSPF cost calculator and the CIDR map consult this module,
//! so routing reacts without restarting the instance.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{OnceLock, RwLock};

use cidr::IpCidr;
use dashmap::DashMap;

use crate::config::RoutingWeights;
use crate::config::PeerId;

#[derive(Debug, Clone, Default)]
pub struct LinkPolicy {
    pub bandwidth_mbps: Option<f64>,
    pub latency_weight: Option<f64>,
    pub loss_weight: Option<f64>,
    pub bandwidth_weight: Option<f64>,
    pub disabled: bool,
}

static GENERATION: AtomicU64 = AtomicU64::new(1);
static WEIGHTED: AtomicBool = AtomicBool::new(true);
static WEIGHTS: OnceLock<RwLock<RoutingWeights>> = OnceLock::new();
static LINKS: OnceLock<RwLock<HashMap<(PeerId, PeerId), LinkPolicy>>> = OnceLock::new();
static STATIC_ROUTES: OnceLock<RwLock<Vec<(IpCidr, PeerId)>>> = OnceLock::new();
#[derive(Debug, Clone, Copy, Default)]
pub struct NodeBandwidth {
    pub up_mbps: f64,
    pub down_mbps: f64,
}

static NODE_BANDWIDTH: OnceLock<RwLock<HashMap<PeerId, NodeBandwidth>>> = OnceLock::new();

#[derive(Debug, Clone, Default)]
pub struct PeerMetrics {
    pub proto: String,
    pub transport_loss_pct: f32,
    pub utilization: f64,
    pub capacity_mbps: f64,
    pub transport_rtt_us: Option<u32>,
    pub probe_capacity_mbps: Option<f64>,
}

static PEER_METRICS: OnceLock<RwLock<HashMap<PeerId, PeerMetrics>>> = OnceLock::new();

#[derive(Debug, Clone, Copy, Default)]
pub struct LocalBandwidthCaps {
    pub intranet_down_mbps: Option<f64>,
    pub internet_down_mbps: Option<f64>,
}

static LOCAL_CAPS: OnceLock<RwLock<LocalBandwidthCaps>> = OnceLock::new();

fn local_caps() -> &'static RwLock<LocalBandwidthCaps> {
    LOCAL_CAPS.get_or_init(|| RwLock::new(LocalBandwidthCaps::default()))
}

pub fn set_local_bandwidth_caps(caps: LocalBandwidthCaps) {
    *local_caps().write().unwrap() = caps;
}

pub fn intranet_down_cap() -> Option<f64> {
    local_caps().read().unwrap().intranet_down_mbps
}

pub fn internet_down_cap() -> Option<f64> {
    local_caps().read().unwrap().internet_down_mbps
}

fn weights() -> &'static RwLock<RoutingWeights> {
    WEIGHTS.get_or_init(|| RwLock::new(RoutingWeights::default()))
}

fn links() -> &'static RwLock<HashMap<(PeerId, PeerId), LinkPolicy>> {
    LINKS.get_or_init(|| RwLock::new(HashMap::new()))
}

fn static_routes() -> &'static RwLock<Vec<(IpCidr, PeerId)>> {
    STATIC_ROUTES.get_or_init(|| RwLock::new(Vec::new()))
}

fn node_bandwidths() -> &'static RwLock<HashMap<PeerId, NodeBandwidth>> {
    NODE_BANDWIDTH.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Advertised per-node uplink/downlink capacity (Mbps). Directional link
/// capacity is derived as `min(up(src), down(dst))`; explicit per-link
/// overrides still win.
pub fn set_node_bandwidth_silent(peer: PeerId, up_mbps: f64, down_mbps: f64) {
    let mut map = node_bandwidths().write().unwrap();
    let entry = map.entry(peer).or_insert(NodeBandwidth {
        up_mbps: 0.1,
        down_mbps: 0.1,
    });
    entry.up_mbps = up_mbps.max(0.1);
    entry.down_mbps = down_mbps.max(0.1);
}

pub fn set_node_bandwidth(peer: PeerId, up_mbps: f64, down_mbps: f64) {
    node_bandwidths().write().unwrap().insert(
        peer,
        NodeBandwidth {
            up_mbps: up_mbps.max(0.1),
            down_mbps: down_mbps.max(0.1),
        },
    );
    GENERATION.fetch_add(1, Ordering::Relaxed);
}

pub fn node_bandwidth(peer: PeerId) -> Option<NodeBandwidth> {
    node_bandwidths().read().unwrap().get(&peer).copied()
}

fn peer_metrics_map() -> &'static RwLock<HashMap<PeerId, PeerMetrics>> {
    PEER_METRICS.get_or_init(|| RwLock::new(HashMap::new()))
}

// ---------------------------------------------------------------------------
// Direct-link metrics published by the conn layer and flooded via RoutePeerInfo
// (decentralized peer-center replacement for cost inputs).
// ---------------------------------------------------------------------------

const DIRECT_CONN_METRICS_TTL_MS: u64 = 60_000;

static DIRECT_CONN_METRICS: OnceLock<DashMap<PeerId, (i32, u32, u64)>> = OnceLock::new();

fn direct_conn_metrics_map() -> &'static DashMap<PeerId, (i32, u32, u64)> {
    DIRECT_CONN_METRICS.get_or_init(DashMap::new)
}

/// Conn layer: publish the measured (latency_ms, loss_perm) of a DIRECT link.
/// Cheap: one sharded-map write per ping interval (~seconds), never on the
/// data path.
pub fn set_conn_metric(peer: PeerId, latency_ms: i32, loss_perm: u32) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    direct_conn_metrics_map().insert(peer, (latency_ms, loss_perm, now));
}

/// Snapshot of fresh (<=60s) direct-link metrics keyed by the neighbor peer.
pub fn direct_conn_metrics() -> std::collections::BTreeMap<PeerId, (i32, u32)> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    direct_conn_metrics_map()
        .iter()
        .filter(|(_, (_, _, ts))| now.saturating_sub(*ts) <= DIRECT_CONN_METRICS_TTL_MS)
        .map(|(peer, (latency, loss, _))| (*peer, (*latency, *loss)))
        .collect()
}

/// Publish measured transport metrics for a peer. Bumps the policy generation
/// only when the change is significant, to avoid re-running OSPF every second.
pub fn set_peer_metrics(peer: PeerId, metrics: PeerMetrics) {
    let mut map = peer_metrics_map().write().unwrap();
    let changed = match map.get(&peer) {
        Some(previous) => {
            (previous.transport_loss_pct - metrics.transport_loss_pct).abs() > 1.0
                || (previous.utilization - metrics.utilization).abs() > 0.05
                || (previous.capacity_mbps - metrics.capacity_mbps).abs() > 1.0
        }
        None => true,
    };
    map.insert(peer, metrics);
    drop(map);
    if changed {
        GENERATION.fetch_add(1, Ordering::Relaxed);
    }
}

/// Update only the capacity estimate for a peer (advertised bandwidth from the
/// peer center), preserving loss/utilization samples.
pub fn set_peer_capacity(peer: PeerId, capacity_mbps: f64) {
    if capacity_mbps <= 0.0 {
        return;
    }
    let capacity_mbps = capacity_mbps.round().max(1.0);
    let mut map = peer_metrics_map().write().unwrap();
    let entry = map.entry(peer).or_default();
    if (entry.capacity_mbps - capacity_mbps).abs() > 1.0 {
        entry.capacity_mbps = capacity_mbps;
        drop(map);
        GENERATION.fetch_add(1, Ordering::Relaxed);
    } else {
        entry.capacity_mbps = capacity_mbps;
    }
}

pub fn set_peer_probe_capacity(peer: PeerId, capacity_mbps: f64) {
    if capacity_mbps <= 0.0 {
        return;
    }
    let mut map = peer_metrics_map().write().unwrap();
    let entry = map.entry(peer).or_default();
    let changed = entry
        .probe_capacity_mbps
        .map(|previous| (previous - capacity_mbps).abs() > 1.0)
        .unwrap_or(true);
    entry.probe_capacity_mbps = Some(capacity_mbps);
    entry.capacity_mbps = capacity_mbps;
    drop(map);
    if changed {
        GENERATION.fetch_add(1, Ordering::Relaxed);
    }
}

pub fn peer_metrics(peer: PeerId) -> Option<PeerMetrics> {
    peer_metrics_map().read().unwrap().get(&peer).cloned()
}

fn protocol_loss_weight(global: &RoutingWeights, proto: Option<&str>) -> f64 {
    match proto.map(str::to_ascii_lowercase).as_deref() {
        Some("tcp") | Some("ws") | Some("wss") | Some("faketcp") => global.tcp_loss_weight,
        Some("udp") => global.udp_loss_weight,
        Some("kcp") => global.kcp_loss_weight,
        Some("quic") => global.quic_loss_weight,
        Some("wg") | Some("wireguard") => global.wg_loss_weight,
        _ => 1.0,
    }
}

pub fn generation() -> u64 {
    GENERATION.load(Ordering::Relaxed)
}

pub fn set_routing_weights(value: RoutingWeights) {
    WEIGHTED.store(value.weighted, Ordering::Relaxed);
    *weights().write().unwrap() = value;
    GENERATION.fetch_add(1, Ordering::Relaxed);
}

/// Whether the OSPF engine should use the weighted cost table by default.
pub fn weighted_mode() -> bool {
    WEIGHTED.load(Ordering::Relaxed)
}

pub fn routing_weights() -> RoutingWeights {
    weights().read().unwrap().clone()
}

pub fn set_links(value: HashMap<(PeerId, PeerId), LinkPolicy>) {
    *links().write().unwrap() = value;
    GENERATION.fetch_add(1, Ordering::Relaxed);
}

pub fn set_static_routes(value: Vec<(IpCidr, PeerId)>) {
    *static_routes().write().unwrap() = value;
    GENERATION.fetch_add(1, Ordering::Relaxed);
}

pub fn list_static_routes() -> Vec<(IpCidr, PeerId)> {
    static_routes().read().unwrap().clone()
}

fn link_for(src: PeerId, dst: PeerId) -> Option<LinkPolicy> {
    let map = links().read().unwrap();
    map.get(&(src, dst))
        .or_else(|| map.get(&(dst, src)))
        .cloned()
}

/// Cost formula (directional):
/// `latency*w_lat + loss_term*w_loss + (ref_bw/bandwidth)*w_bw
///  + utilization*w_util + w_hop`, where the loss term is `-ln(1-p)` so
/// independent losses combine multiplicatively across hops.
pub fn calculate_cost(src: PeerId, dst: PeerId, latency_ms: i32, loss_pct: f32) -> i32 {
    calculate_cost_ext(src, dst, latency_ms, loss_pct, None, None)
}

pub fn calculate_cost_ext(
    src: PeerId,
    dst: PeerId,
    latency_ms: i32,
    loss_pct: f32,
    proto: Option<&str>,
    utilization: Option<f64>,
) -> i32 {
    let src_bw = node_bandwidth(src);
    let dst_bw = node_bandwidth(dst);
    let node_bw = match (src_bw, dst_bw) {
        (Some(a), Some(b)) => Some(a.up_mbps.min(b.down_mbps)),
        (Some(a), None) => Some(a.up_mbps),
        (None, Some(b)) => Some(b.down_mbps),
        (None, None) => None,
    };
    let link = link_for(src, dst);
    if link.as_ref().is_some_and(|link| link.disabled) {
        return i32::MAX;
    }
    let metrics = peer_metrics(dst).or_else(|| peer_metrics(src));
    let proto = proto.or_else(|| metrics.as_ref().map(|m| m.proto.as_str()));
    let utilization = utilization.or_else(|| metrics.as_ref().map(|m| m.utilization));
    let effective_loss = metrics
        .as_ref()
        .map(|m| loss_pct.max(m.transport_loss_pct))
        .unwrap_or(loss_pct);
    let node_bw = node_bw.or_else(|| {
        metrics
            .as_ref()
            .map(|m| m.capacity_mbps)
            .filter(|v| *v > 0.0)
    });
    let global = routing_weights();
    let weight = protocol_loss_weight(&global, proto);
    cost_with(&global, link.as_ref(), node_bw, weight, utilization, latency_ms, effective_loss, true)
}

fn cost_with(
    global: &RoutingWeights,
    link: Option<&LinkPolicy>,
    node_bw: Option<f64>,
    protocol_loss_weight: f64,
    utilization: Option<f64>,
    latency_ms: i32,
    loss_pct: f32,
    include_bandwidth: bool,
) -> i32 {
    let w_lat = link
        .and_then(|l| l.latency_weight)
        .unwrap_or(global.latency_weight);
    let w_loss = link
        .and_then(|l| l.loss_weight)
        .unwrap_or(global.loss_weight)
        * protocol_loss_weight.max(0.05);
    let w_bw = link
        .and_then(|l| l.bandwidth_weight)
        .unwrap_or(global.bandwidth_weight);
    let bandwidth = link
        .and_then(|l| l.bandwidth_mbps)
        .or(node_bw)
        .unwrap_or(global.reference_bandwidth_mbps)
        .max(0.1);
    let ref_bw = global.reference_bandwidth_mbps.max(0.1);

    if global.min_bandwidth_mbps > 0.0 && bandwidth < global.min_bandwidth_mbps {
        // hard prune: keep below AVOID_RELAY_COST semantics but very expensive
        return i32::MAX - 3;
    }

    let loss_fraction = (loss_pct as f64 / 100.0).clamp(0.0, 0.999);
    let loss_term = if loss_fraction > 0.0 {
        -((1.0 - loss_fraction).ln()) * global.loss_gain.max(0.0)
    } else {
        0.0
    };
    let utilization_term = global.utilization_weight.max(0.0)
        * utilization.unwrap_or(0.0).clamp(0.0, 1.0).powi(2);

    let mut cost = latency_ms as f64 * w_lat
        + loss_term * w_loss
        + utilization_term
        + global.hop_weight.max(0.0);
    if include_bandwidth {
        cost += (ref_bw / bandwidth) * w_bw;
    }
    (cost.clamp(1.0, (i32::MAX - 4) as f64)) as i32
}

/// Effective capacity of the link src->dst in Mbps. Precedence: per-link
/// override > user-configured node bandwidth (min of up/down across the two
/// ends) > auto-measured capacity. Never below 0.1 to keep divisions sane.
pub fn link_capacity_mbps(src: PeerId, dst: PeerId) -> f64 {
    let src_bw = node_bandwidth(src);
    let dst_bw = node_bandwidth(dst);
    let node_bw = match (src_bw, dst_bw) {
        (Some(a), Some(b)) => Some(a.up_mbps.min(b.down_mbps)),
        (Some(a), None) => Some(a.up_mbps),
        (None, Some(b)) => Some(b.down_mbps),
        (None, None) => None,
    };
    let link = link_for(src, dst);
    let metrics = peer_metrics(dst).or_else(|| peer_metrics(src));
    link.and_then(|l| l.bandwidth_mbps)
        .or(node_bw)
        .or_else(|| metrics.as_ref().map(|m| m.capacity_mbps).filter(|v| *v > 0.0))
        .unwrap_or_else(|| routing_weights().reference_bandwidth_mbps)
        .max(0.1)
}

/// Additive (latency/loss/utilization/hop) component of the link cost with the
/// bandwidth term REMOVED. Used by the bottleneck-aware path selection, which
/// adds the bandwidth penalty once per path based on the bottleneck capacity.
/// `utilization` is the fraction of the link capacity consumed by OTHER
/// traffic (never the flow being routed — that would oscillate).
pub fn link_base_cost(
    src: PeerId,
    dst: PeerId,
    latency_ms: i32,
    loss_pct: f32,
    utilization: Option<f64>,
) -> i32 {
    let link = link_for(src, dst);
    if link.as_ref().is_some_and(|link| link.disabled) {
        return i32::MAX;
    }
    let metrics = peer_metrics(dst).or_else(|| peer_metrics(src));
    let proto = metrics.as_ref().map(|m| m.proto.as_str());
    let effective_loss = metrics
        .as_ref()
        .map(|m| loss_pct.max(m.transport_loss_pct))
        .unwrap_or(loss_pct);
    let global = routing_weights();
    let weight = protocol_loss_weight(&global, proto);
    cost_with(
        &global,
        link.as_ref(),
        None,
        weight,
        utilization,
        latency_ms,
        effective_loss,
        false,
    )
}

// ---------------------------------------------------------------------------
// Per-(link, destination) traffic accounting
//
// The relay data path counts every forwarded byte per (next-hop link, final
// destination). A periodic sampler turns the byte counters into Mbps rates.
// Smart-mode routing uses these to attribute link usage to the flow that owns
// it: when computing the route FOR destination D, a link's available capacity
// is capacity minus the usage of every OTHER destination — the flow's own
// usage on its own path is excluded, which prevents the classic
// route-oscillation loop (a-b-c <-> a-d-c flip-flopping as each path fills
// up and frees the other).
//
// Hot-path guarantee: the counter is a sharded DashMap of atomics — the data
// plane takes a per-shard brief lock plus one atomic add per forwarded
// packet and NEVER contends with the sampler or the route builder, which
// read the same map afterwards without holding any lock across iteration.
// ---------------------------------------------------------------------------

static LINK_DST_BYTES: OnceLock<DashMap<(PeerId, PeerId), AtomicU64>> = OnceLock::new();
static LINK_LAST_SAMPLE: OnceLock<RwLock<HashMap<(PeerId, PeerId), (u64, i64)>>> = OnceLock::new();
static LINK_DST_RATES: OnceLock<RwLock<HashMap<(PeerId, PeerId), f64>>> = OnceLock::new();
static LINK_TOTAL_RATES: OnceLock<RwLock<HashMap<PeerId, f64>>> = OnceLock::new();

fn link_dst_bytes() -> &'static DashMap<(PeerId, PeerId), AtomicU64> {
    LINK_DST_BYTES.get_or_init(DashMap::new)
}

fn link_last_sample() -> &'static RwLock<HashMap<(PeerId, PeerId), (u64, i64)>> {
    LINK_LAST_SAMPLE.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Data path: count forwarded bytes on the link towards `link_peer` that are
/// addressed to `dst_peer`. Cheap: one sharded-map entry plus one atomic add.
pub fn add_link_dst_bytes(link_peer: PeerId, dst_peer: PeerId, bytes: u64) {
    link_dst_bytes()
        .entry((link_peer, dst_peer))
        .or_insert_with(|| AtomicU64::new(0))
        .fetch_add(bytes, Ordering::Relaxed);
}

/// Sampler: convert byte counters into Mbps rates since the previous call.
/// Call this periodically (1s) so rates never average over long idle windows.
pub fn sample_link_dst_rates() {
    let now_ts = now_millis();
    // Snapshot the atomics first (brief per-shard reads), do the math without
    // touching the data-plane map again.
    let current: Vec<((PeerId, PeerId), u64)> = link_dst_bytes()
        .iter()
        .map(|entry| (*entry.key(), entry.value().load(Ordering::Relaxed)))
        .collect();
    let mut last = link_last_sample().write().unwrap();
    let mut rates: HashMap<(PeerId, PeerId), f64> = HashMap::new();
    let mut totals: HashMap<PeerId, f64> = HashMap::new();
    for (key, total) in current {
        let rate_mbps = match last.get(&key).copied() {
            Some((prev_total, prev_ts))
                if prev_total <= total && prev_ts < now_ts && now_ts > prev_ts =>
            {
                let elapsed_secs = (now_ts - prev_ts) as f64 / 1000.0;
                if elapsed_secs > 0.0 {
                    Some(((total - prev_total) * 8) as f64 / elapsed_secs / 1_000_000.0)
                } else {
                    None
                }
            }
            _ => None,
        };
        last.insert(key, (total, now_ts));
        if let Some(rate_mbps) = rate_mbps.filter(|rate| *rate > 0.0) {
            rates.insert(key, rate_mbps);
            *totals.entry(key.0).or_insert(0.0) += rate_mbps;
        }
    }
    *LINK_DST_RATES.get_or_init(|| RwLock::new(HashMap::new())).write().unwrap() = rates;
    *LINK_TOTAL_RATES.get_or_init(|| RwLock::new(HashMap::new())).write().unwrap() = totals;
}

/// Mbps currently forwarded over the link towards `link_peer` for the given
/// destination (0 when unmeasured).
pub fn link_dst_rate(link_peer: PeerId, dst_peer: PeerId) -> f64 {
    LINK_DST_RATES
        .get_or_init(|| RwLock::new(HashMap::new()))
        .read()
        .unwrap()
        .get(&(link_peer, dst_peer))
        .copied()
        .unwrap_or(0.0)
}

/// Total Mbps currently forwarded over the link towards `link_peer`.
pub fn link_total_rate(link_peer: PeerId) -> f64 {
    LINK_TOTAL_RATES
        .get_or_init(|| RwLock::new(HashMap::new()))
        .read()
        .unwrap()
        .get(&link_peer)
        .copied()
        .unwrap_or(0.0)
}

/// Per-destination rates leaving this node through the link towards
/// `link_peer` (for the peer-center report).
pub fn link_dst_rates_for(link_peer: PeerId) -> HashMap<PeerId, f64> {
    LINK_DST_RATES
        .get_or_init(|| RwLock::new(HashMap::new()))
        .read()
        .unwrap()
        .iter()
        .filter(|(key, _)| key.0 == link_peer)
        .map(|(key, rate)| (key.1, *rate))
        .collect()
}

fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cost_uses_weights_and_link_overrides() {
        set_routing_weights(RoutingWeights {
            latency_weight: 1.0,
            loss_weight: 10.0,
            bandwidth_weight: 0.0,
            reference_bandwidth_mbps: 100.0,
            ..RoutingWeights::default()
        });
        set_links(HashMap::new());
        // 20ms * 1 + 5% * 10 = 70
        assert_eq!(calculate_cost(1, 2, 20, 5.0), 70);

        let mut map = HashMap::new();
        map.insert(
            (1, 2),
            LinkPolicy {
                bandwidth_mbps: Some(10.0),
                latency_weight: Some(2.0),
                loss_weight: Some(0.0),
                bandwidth_weight: Some(1.0),
                disabled: false,
            },
        );
        set_links(map);
        // 20ms * 2 + (100/10) * 1 = 50
        assert_eq!(calculate_cost(1, 2, 20, 5.0), 50);

        let mut map = HashMap::new();
        map.insert(
            (1, 2),
            LinkPolicy {
                disabled: true,
                ..Default::default()
            },
        );
        set_links(map);
        assert_eq!(calculate_cost(1, 2, 20, 5.0), i32::MAX);
    }

    #[test]
    fn static_routes_roundtrip() {
        let cidr: IpCidr = "10.50.0.0/16".parse().unwrap();
        set_static_routes(vec![(cidr, 42)]);
        let got = list_static_routes();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].1, 42);
        set_static_routes(Vec::new());
    }

    #[test]
    fn link_dst_rates_track_forwarded_bytes() {
        // Retry loop: unit tests elsewhere may rebuild routes concurrently and
        // consume this test's rate window; each attempt re-primes and retries.
        for _ in 0..4 {
            add_link_dst_bytes(10, 100, 1_000_000);
            sample_link_dst_rates(); // prime the window
            std::thread::sleep(std::time::Duration::from_millis(1100));
            add_link_dst_bytes(10, 100, 2_000_000); // +1 MB in the window
            add_link_dst_bytes(10, 200, 250_000); // new key: primed only
            sample_link_dst_rates();

            let rate_d100 = link_dst_rate(10, 100);
            // 1 MB * 8 over ~1.1s ≈ 7.3 Mbps (loose bounds for CI jitter).
            if rate_d100 > 4.0 && rate_d100 < 12.0 {
                assert_eq!(link_dst_rate(10, 200), 0.0);
                let for_link = link_dst_rates_for(10);
                assert_eq!(for_link[&100], rate_d100);
                assert!(link_dst_rates_for(99).is_empty());
                return;
            }
        }
        panic!("link dst rates never converged to the expected window");
    }
}
