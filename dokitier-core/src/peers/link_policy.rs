//! Process-wide link policy used by routing and the embedded web console.
//!
//! The web console persists link settings in SQLite and pushes the effective
//! values here. The OSPF cost calculator and the CIDR map consult this module,
//! so routing reacts without restarting the instance.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{OnceLock, RwLock};

use cidr::IpCidr;

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

fn weights() -> &'static RwLock<RoutingWeights> {
    WEIGHTS.get_or_init(|| RwLock::new(RoutingWeights::default()))
}

fn links() -> &'static RwLock<HashMap<(PeerId, PeerId), LinkPolicy>> {
    LINKS.get_or_init(|| RwLock::new(HashMap::new()))
}

fn static_routes() -> &'static RwLock<Vec<(IpCidr, PeerId)>> {
    STATIC_ROUTES.get_or_init(|| RwLock::new(Vec::new()))
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

/// Cost formula:
/// `latency*w_lat + loss_pct*w_loss + (ref_bw/bandwidth)*w_bw`.
pub fn calculate_cost(src: PeerId, dst: PeerId, latency_ms: i32, loss_pct: f32) -> i32 {
    let Some(link) = link_for(src, dst) else {
        return cost_with(&routing_weights(), None, latency_ms, loss_pct);
    };
    if link.disabled {
        // Must be >= AVOID_RELAY_COST in the OSPF graph so the least-hop
        // computation stops treating this edge as a normal one-hop link.
        return i32::MAX;
    }
    cost_with(&routing_weights(), Some(&link), latency_ms, loss_pct)
}

fn cost_with(
    global: &RoutingWeights,
    link: Option<&LinkPolicy>,
    latency_ms: i32,
    loss_pct: f32,
) -> i32 {
    let w_lat = link
        .and_then(|l| l.latency_weight)
        .unwrap_or(global.latency_weight);
    let w_loss = link
        .and_then(|l| l.loss_weight)
        .unwrap_or(global.loss_weight);
    let w_bw = link
        .and_then(|l| l.bandwidth_weight)
        .unwrap_or(global.bandwidth_weight);
    let bandwidth = link
        .and_then(|l| l.bandwidth_mbps)
        .unwrap_or(global.reference_bandwidth_mbps)
        .max(0.1);
    let ref_bw = global.reference_bandwidth_mbps.max(0.1);

    let cost = latency_ms as f64 * w_lat
        + loss_pct as f64 * w_loss
        + (ref_bw / bandwidth) * w_bw;
    (cost.clamp(1.0, (i32::MAX - 2) as f64)) as i32
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
            weighted: true,
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
}
