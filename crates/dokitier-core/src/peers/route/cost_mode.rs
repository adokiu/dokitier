//! Routing cost modes.
//!
//! A mode decides how the next-hop table is derived from the live topology.
//! Modes are intentionally small and additive: adding one means adding a
//! variant here plus a `gen_next_hop_map_*` implementation in
//! `peer_ospf_route.rs` — nothing else in the data path changes.
//!
//! - `Smart`: composite cost — latency + loss + link utilization per hop, plus
//!   the bottleneck-bandwidth penalty counted ONCE per path (the path's
//!   available bandwidth is the minimum link capacity along it; user-configured
//!   node bandwidth always overrides auto measurements). The chosen first hop
//!   is sticky: a new best path must beat the current one by a margin before
//!   traffic migrates, so jittering metrics cannot flap paths.
//! - `MinLatency`: pure per-link latency sum.
//! - `LeastHop`: pure hop count.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum RouteCostMode {
    #[default]
    #[serde(alias = "smart")]
    Smart,
    #[serde(alias = "min_latency", alias = "min-latency")]
    MinLatency,
    #[serde(alias = "least_hop", alias = "least-hop")]
    LeastHop,
}

impl RouteCostMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Smart => "smart",
            Self::MinLatency => "min_latency",
            Self::LeastHop => "least_hop",
        }
    }

    pub fn parse(value: &str) -> Self {
        match value.trim().to_lowercase().as_str() {
            "min_latency" | "min-latency" | "latency" => Self::MinLatency,
            "least_hop" | "least-hop" | "hop" => Self::LeastHop,
            _ => Self::Smart,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_known_modes_and_defaults_to_smart() {
        assert_eq!(RouteCostMode::parse("smart"), RouteCostMode::Smart);
        assert_eq!(RouteCostMode::parse("min_latency"), RouteCostMode::MinLatency);
        assert_eq!(RouteCostMode::parse("least_hop"), RouteCostMode::LeastHop);
        assert_eq!(RouteCostMode::parse("nonsense"), RouteCostMode::Smart);
    }
}
