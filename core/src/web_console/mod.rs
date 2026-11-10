//! Optional embedded web console for the dokitier core process.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use dokitier_core::config::{RoutingWeights, WebConsoleSettings};
use crate::common::config::{ConfigFileControl, TomlConfigLoader};
use dokitier_web::metrics::{LinkKey, MetricPoint};
use dokitier_web::{DataSource, MetricsHub, WebConsoleConfig};
use serde_json::json;
use tokio::sync::RwLock;

use crate::instance::factory::NativeInstanceManager;

pub struct CoreWebDataSource {
    manager: Arc<NativeInstanceManager>,
    routing: RoutingWeights,
    store: Arc<dokitier_web::Store>,
}

impl CoreWebDataSource {
    pub fn new(
        manager: Arc<NativeInstanceManager>,
        routing: RoutingWeights,
        store: Arc<dokitier_web::Store>,
    ) -> Self {
        Self {
            manager,
            routing,
            store,
        }
    }
}

#[async_trait::async_trait]
impl DataSource for CoreWebDataSource {
    async fn apply_config(&self, toml: &str) -> anyhow::Result<()> {
        let cfg = TomlConfigLoader::new_from_str(toml)
            .map_err(|e| anyhow::anyhow!("TOML 配置无效: {e}"))?;
        let ids = self.manager.instance_ids();
        if ids.is_empty() {
            anyhow::bail!("no running network instance");
        }
        self.manager.delete_network_instances(ids).await?;
        self.manager
            .run_network_instance(cfg, ConfigFileControl::STATIC_CONFIG)?;
        Ok(())
    }

    async fn overview(&self) -> serde_json::Value {
        let instances = self.manager.instances();
        let mut node_ids = HashSet::new();
        let mut edge_count = 0usize;
        for instance in &instances {
            for route in instance.route_snapshots().await {
                node_ids.insert(route.peer_id);
            }
            edge_count += instance
                .peer_snapshots()
                .await
                .iter()
                .filter(|peer| !peer.conns.is_empty())
                .count();
        }
        json!({
            "instances": instances.len(),
            "nodes": node_ids.len(),
            "links": edge_count,
            "routing": self.routing,
        })
    }

    async fn topology(&self) -> serde_json::Value {
        let instances = self.manager.instances();
        let mut nodes: HashMap<u32, serde_json::Value> = HashMap::new();
        let mut edges: Vec<serde_json::Value> = Vec::new();
        let mut seen_edges = HashSet::new();

        for instance in &instances {
            let my_peer_id = instance.peer_id();
            let node = instance.node_snapshot().await;
            nodes.insert(
                my_peer_id,
                json!({
                    "peer_id": my_peer_id,
                    "hostname": node.hostname,
                    "ipv4": node.ipv4_addr.map(|x| x.to_string()),
                    "ipv6": node.ipv6_addr.map(|x| x.to_string()),
                    "instance_id": node.instance_id.to_string(),
                    "version": node.version,
                    "nat_type": format!("{:?}", node.stun_info.udp_nat_type),
                    "local": true,
                }),
            );

            for route in instance.route_snapshots().await {
                nodes.entry(route.peer_id).or_insert_with(|| {
                    json!({
                        "peer_id": route.peer_id,
                        "hostname": route.hostname,
                        "ipv4": format_ipv4(&route.ipv4_addr),
                        "ipv6": format_ipv6(&route.ipv6_addr),
                        "instance_id": route.inst_id,
                        "version": route.version,
                        "nat_type": format!("{:?}", route.stun_info.as_ref().map(|x| x.udp_nat_type)),
                        "local": false,
                    })
                });
            }

            // Local direct connections carry tunnel types and metrics.
            let mut local_tunnels: HashMap<u32, Vec<String>> = HashMap::new();
            for peer in instance.peer_snapshots().await {
                if peer.conns.is_empty() {
                    continue;
                }
                let mut tunnels: Vec<String> = Vec::new();
                for conn in &peer.conns {
                    if let Some(tunnel) = &conn.tunnel
                        && !tunnels.contains(&tunnel.tunnel_type)
                    {
                        tunnels.push(tunnel.tunnel_type.clone());
                    }
                }
                local_tunnels.insert(peer.peer_id, tunnels);
            }

            // Global adjacency includes edges between peers that this node is
            // not directly connected to.
            for (src, dsts) in instance.conn_snapshots().await {
                for dst in dsts {
                    let key = LinkKey::new(src, dst);
                    let id = format!("{}:{}", key.a_peer_id, key.b_peer_id);
                    if !seen_edges.insert(id) {
                        continue;
                    }
                    let tunnels = if src == my_peer_id {
                        local_tunnels.get(&dst).cloned().unwrap_or_default()
                    } else if dst == my_peer_id {
                        local_tunnels.get(&src).cloned().unwrap_or_default()
                    } else {
                        Vec::new()
                    };
                    for peer_id in [key.a_peer_id, key.b_peer_id] {
                        nodes.entry(peer_id).or_insert_with(|| {
                            json!({
                                "peer_id": peer_id,
                                "hostname": format!("#{peer_id}"),
                                "local": false,
                            })
                        });
                    }
                    edges.push(json!({
                        "a": key.a_peer_id,
                        "b": key.b_peer_id,
                        "tunnel_types": tunnels,
                    }));
                }
            }
        }

        json!({
            "nodes": nodes.into_values().collect::<Vec<_>>(),
            "edges": edges,
        })
    }

    async fn nodes(&self) -> serde_json::Value {
        let instances = self.manager.instances();
        let mut nodes: HashMap<u32, serde_json::Value> = HashMap::new();
        for instance in &instances {
            let my_peer_id = instance.peer_id();
            let me = instance.node_snapshot().await;
            nodes.entry(my_peer_id).or_insert_with(|| {
                json!({
                    "peer_id": my_peer_id,
                    "hostname": me.hostname,
                    "ipv4": me.ipv4_addr.map(|x| x.to_string()),
                    "ipv6": me.ipv6_addr.map(|x| x.to_string()),
                    "instance_id": me.instance_id.to_string(),
                    "version": me.version,
                    "cost": 0,
                    "path_latency": 0,
                    "next_hop_peer_id": my_peer_id,
                    "proxy_cidrs": [],
                    "local": true,
                })
            });
            for route in instance.route_snapshots().await {
                if route.peer_id == my_peer_id {
                    continue;
                }
                nodes.entry(route.peer_id).or_insert_with(|| {
                    json!({
                        "peer_id": route.peer_id,
                        "hostname": route.hostname,
                        "ipv4": format_ipv4(&route.ipv4_addr),
                        "ipv6": format_ipv6(&route.ipv6_addr),
                        "instance_id": route.inst_id,
                        "version": route.version,
                        "cost": route.cost,
                        "path_latency": route.path_latency,
                        "next_hop_peer_id": route.next_hop_peer_id,
                        "proxy_cidrs": route.proxy_cidrs,
                        "local": false,
                    })
                });
            }
        }
        json!({ "nodes": nodes.into_values().collect::<Vec<_>>() })
    }

    async fn node(&self, peer_id: u32) -> Option<serde_json::Value> {
        let instances = self.manager.instances();
        for instance in &instances {
            if instance.peer_id() == peer_id {
                let node = instance.node_snapshot().await;
                return Some(json!({
                    "peer_id": peer_id,
                    "hostname": node.hostname,
                    "ipv4": node.ipv4_addr.map(|x| x.to_string()),
                    "ipv6": node.ipv6_addr.map(|x| x.to_string()),
                    "instance_id": node.instance_id.to_string(),
                    "version": node.version,
                    "listeners": node.listeners.iter().map(|x| x.to_string()).collect::<Vec<_>>(),
                    "features": format!("{:?}", node.feature_flags),
                    "local": true,
                }));
            }
            for route in instance.route_snapshots().await {
                if route.peer_id == peer_id {
                    return Some(json!({
                        "peer_id": route.peer_id,
                        "hostname": route.hostname,
                        "ipv4": format_ipv4(&route.ipv4_addr),
                        "ipv6": format_ipv6(&route.ipv6_addr),
                        "instance_id": route.inst_id,
                        "version": route.version,
                        "cost": route.cost,
                        "path_latency": route.path_latency,
                        "next_hop_peer_id": route.next_hop_peer_id,
                        "proxy_cidrs": route.proxy_cidrs,
                        "local": false,
                    }));
                }
            }
        }
        None
    }

    async fn routes(&self) -> serde_json::Value {
        let instances = self.manager.instances();
        let mut out = Vec::new();
        for instance in &instances {
            let my_peer_id = instance.peer_id();
            let routes: Vec<serde_json::Value> = instance
                .route_snapshots()
                .await
                .into_iter()
                .map(|route| {
                    json!({
                        "peer_id": route.peer_id,
                        "hostname": route.hostname,
                        "next_hop_peer_id": route.next_hop_peer_id,
                        "cost": route.cost,
                        "path_latency": route.path_latency,
                        "next_hop_latency_first": route.next_hop_peer_id_latency_first,
                        "cost_latency_first": route.cost_latency_first,
                        "path_latency_latency_first": route.path_latency_latency_first,
                        "ipv4": format_ipv4(&route.ipv4_addr),
                        "ipv6": format_ipv6(&route.ipv6_addr),
                        "proxy_cidrs": route.proxy_cidrs,
                    })
                })
                .collect();
            out.push(json!({ "my_peer_id": my_peer_id, "routes": routes }));
        }
        json!({ "instances": out })
    }

    async fn sync_token(&self) -> Option<String> {
        use dokitier_core::config::toml::ConfigLoader as _;
        for instance in self.manager.instances() {
            if let Some(config) = instance.toml_config() {
                let identity: dokitier_core::config::NetworkIdentity =
                    (&config.get_network_identity()).into();
                if let Some(digest) = identity.secret_digest() {
                    let bytes: &[u8; 32] = &digest;
                    return Some(bytes.iter().map(|b| format!("{b:02x}")).collect());
                }
            }
        }
        None
    }

    async fn sync_peer_ips(&self) -> Vec<String> {
        let mut out = Vec::new();
        for instance in self.manager.instances() {
            let my_ip = instance
                .node_snapshot()
                .await
                .ipv4_addr
                .map(|addr| addr.address().to_string());
            for route in instance.route_snapshots().await {
                let Some(cidr) = format_ipv4(&route.ipv4_addr) else {
                    continue;
                };
                let Some(ip) = cidr.split('/').next().map(|s| s.to_string()) else {
                    continue;
                };
                if Some(&ip) == my_ip.as_ref() || ip.is_empty() {
                    continue;
                }
                if !out.contains(&ip) {
                    out.push(ip);
                }
            }
        }
        out
    }

    async fn on_config_changed(&self, namespace: &str, key: &str) {
        tracing::info!(namespace, key, "web console config changed");
        reload_routing_policy(&self.store).await;
    }
}

fn format_ipv4(value: &Option<dokitier_proto::common::Ipv4Inet>) -> Option<String> {
    let inet = value.as_ref()?;
    let addr = inet.address?;
    Some(format!(
        "{}/{}",
        std::net::Ipv4Addr::from(addr.addr),
        inet.network_length
    ))
}

fn format_ipv6(value: &Option<dokitier_proto::common::Ipv6Inet>) -> Option<String> {
    let inet = value.as_ref()?;
    let addr = inet.address?;
    Some(format!(
        "{}/{}",
        std::net::Ipv6Addr::new(
            (addr.part1 >> 16) as u16,
            (addr.part1 & 0xffff) as u16,
            (addr.part2 >> 16) as u16,
            (addr.part2 & 0xffff) as u16,
            (addr.part3 >> 16) as u16,
            (addr.part3 & 0xffff) as u16,
            (addr.part4 >> 16) as u16,
            (addr.part4 & 0xffff) as u16,
        ),
        inet.network_length
    ))
}

/// Sample every instance's direct connections once per second into the hub.
pub fn spawn_metrics_sampler(
    manager: Arc<NativeInstanceManager>,
    hub: Arc<MetricsHub>,
    store: Arc<dokitier_web::Store>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut previous: HashMap<String, (i64, u64, u64)> = HashMap::new();
        let link_cache: Arc<RwLock<HashMap<(u32, u32), f64>>> =
            Arc::new(RwLock::new(HashMap::new()));
        let cache_handle = link_cache.clone();
        let store_for_cache = store.clone();
        tokio::spawn(async move {
            loop {
                if let Ok(links) = store_for_cache.list_links().await {
                    let map = links
                        .into_iter()
                        .filter_map(|link| {
                            link.bandwidth_mbps
                                .map(|bw| ((link.a_peer_id, link.b_peer_id), bw))
                        })
                        .collect();
                    *cache_handle.write().await = map;
                }
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            }
        });

        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(1));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            ticker.tick().await;
            let now_ms = chrono::Utc::now().timestamp_millis();
            let bandwidths = link_cache.read().await.clone();
            for instance in manager.instances() {
                let my_peer_id = instance.peer_id();
                for peer in instance.peer_snapshots().await {
                    if peer.conns.is_empty() {
                        continue;
                    }
                    let key = LinkKey::new(my_peer_id, peer.peer_id);
                    let mut tunnels: Vec<dokitier_web::metrics::TunnelStat> = Vec::new();
                    let mut overall_latency: Option<f32> = None;
                    let mut overall_loss = 0.0f32;
                    let mut overall_conns = 0usize;
                    let mut overall_rx = 0.0f64;
                    let mut overall_tx = 0.0f64;

                    for conn in &peer.conns {
                        let stats = conn.stats.clone().unwrap_or_default();
                        let (mut rx_bps, mut tx_bps) = (0.0, 0.0);
                        if let Some((prev_ts, prev_rx, prev_tx)) = previous.get(&conn.conn_id) {
                            let elapsed = ((now_ms - prev_ts) as f64 / 1000.0).max(0.001);
                            if now_ms > *prev_ts {
                                rx_bps = (stats.rx_bytes.saturating_sub(*prev_rx)) as f64 / elapsed;
                                tx_bps = (stats.tx_bytes.saturating_sub(*prev_tx)) as f64 / elapsed;
                            }
                        }
                        previous
                            .insert(conn.conn_id.clone(), (now_ms, stats.rx_bytes, stats.tx_bytes));

                        let latency = (stats.latency_us > 0)
                            .then(|| stats.latency_us as f32 / 1000.0);
                        if let Some(lat) = latency {
                            overall_latency = Some(overall_latency.map_or(lat, |x| x.min(lat)));
                        }
                        overall_loss += conn.loss_rate * 100.0;
                        overall_conns += 1;
                        overall_rx += rx_bps;
                        overall_tx += tx_bps;

                        // One entry per physical connection: two TCP links between
                        // the same pair must stay separate in the topology.
                        tunnels.push(dokitier_web::metrics::TunnelStat {
                            id: conn.conn_id.clone(),
                            tunnel_type: conn
                                .tunnel
                                .as_ref()
                                .map(|t| t.tunnel_type.clone())
                                .unwrap_or_else(|| "unknown".to_string()),
                            remote_addr: conn
                                .tunnel
                                .as_ref()
                                .and_then(|t| t.remote_addr.as_ref())
                                .map(|u| u.to_string()),
                            latency_ms: latency,
                            loss_pct: Some(conn.loss_rate * 100.0),
                            rx_bps: Some(rx_bps),
                            tx_bps: Some(tx_bps),
                        });
                    }

                    let tunnel_types: Vec<String> =
                        tunnels.iter().map(|t| t.tunnel_type.clone()).collect();

                    hub.set_static_info(
                        key,
                        tunnel_types,
                        bandwidths.get(&(key.a_peer_id, key.b_peer_id)).map(|v| *v as f32),
                        Some(my_peer_id),
                    )
                    .await;
                    hub.set_tunnels(key, tunnels).await;
                    hub.record(
                        key,
                        MetricPoint {
                            ts: now_ms / 1000,
                            latency_ms: overall_latency,
                            loss_pct: (overall_conns > 0)
                                .then(|| overall_loss / overall_conns as f32),
                            rx_bps: Some(overall_rx),
                            tx_bps: Some(overall_tx),
                        },
                    )
                    .await;
                }
            }
        }
    })
}

/// If the database has no routing settings yet, seed it with the TOML values so
/// the console and the sync layer have a single source of truth.
async fn seed_routing_settings(store: &dokitier_web::Store, routing: &RoutingWeights) {
    let settings = store.get_settings().await.unwrap_or_default();
    if settings.contains_key("routing.latency_weight") {
        return;
    }
    let now = chrono::Utc::now().timestamp_millis();
    let hlc = format!("{now}:0:config");
    let values = [
        ("routing.latency_weight", routing.latency_weight),
        ("routing.loss_weight", routing.loss_weight),
        ("routing.bandwidth_weight", routing.bandwidth_weight),
        (
            "routing.reference_bandwidth_mbps",
            routing.reference_bandwidth_mbps,
        ),
    ];
    for (key, value) in values {
        let _ = store.set_setting(key, &value.to_string(), &hlc).await;
    }
}

/// Push SQLite config into the process-wide routing policy.
pub async fn reload_routing_policy(store: &dokitier_web::Store) {
    use std::collections::HashMap;

    let links = store.list_links().await.unwrap_or_default();
    let mut map = HashMap::new();
    for link in links {
        map.insert(
            (link.a_peer_id, link.b_peer_id),
            dokitier_core::peers::link_policy::LinkPolicy {
                bandwidth_mbps: link.bandwidth_mbps,
                latency_weight: link.latency_weight,
                loss_weight: link.loss_weight,
                bandwidth_weight: link.bandwidth_weight,
                disabled: link.disabled,
            },
        );
    }
    dokitier_core::peers::link_policy::set_links(map);

    let settings = store.get_settings().await.unwrap_or_default();
    let defaults = dokitier_core::peers::link_policy::routing_weights();
    let read = |key: &str, default: f64| {
        settings
            .get(key)
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(default)
    };
    let mut weights = defaults;
    weights.latency_weight = read("routing.latency_weight", weights.latency_weight);
    weights.loss_weight = read("routing.loss_weight", weights.loss_weight);
    weights.bandwidth_weight = read("routing.bandwidth_weight", weights.bandwidth_weight);
    weights.reference_bandwidth_mbps = read(
        "routing.reference_bandwidth_mbps",
        weights.reference_bandwidth_mbps,
    );
    weights.utilization_weight = read("routing.utilization_weight", weights.utilization_weight);
    weights.hop_weight = read("routing.hop_weight", weights.hop_weight);
    weights.loss_gain = read("routing.loss_gain", weights.loss_gain);
    weights.min_bandwidth_mbps = read("routing.min_bandwidth_mbps", weights.min_bandwidth_mbps);
    weights.tcp_loss_weight = read("routing.tcp_loss_weight", weights.tcp_loss_weight);
    weights.udp_loss_weight = read("routing.udp_loss_weight", weights.udp_loss_weight);
    weights.kcp_loss_weight = read("routing.kcp_loss_weight", weights.kcp_loss_weight);
    weights.quic_loss_weight = read("routing.quic_loss_weight", weights.quic_loss_weight);
    weights.wg_loss_weight = read("routing.wg_loss_weight", weights.wg_loss_weight);
    weights.weighted = settings
        .get("routing.mode")
        .map(|mode| mode != "least_hop")
        .unwrap_or(true);
    weights.cost_mode = settings
        .get("routing.cost_mode")
        .map(|mode| dokitier_core::peers::RouteCostMode::parse(mode))
        .unwrap_or(defaults.cost_mode);
    dokitier_core::peers::link_policy::set_routing_weights(weights);

    let static_routes = store.list_static_routes().await.unwrap_or_default();
    let parsed = static_routes
        .into_iter()
        .filter(|r| r.enabled)
        .filter_map(|r| {
            let cidr = r.dest_cidr.parse::<cidr::IpCidr>().ok()?;
            Some((cidr, r.next_hop_peer_id))
        })
        .collect();
    dokitier_core::peers::link_policy::set_static_routes(parsed);
}

/// Start the console when the node config enables it.
pub async fn maybe_start(
    settings: WebConsoleSettings,
    routing: RoutingWeights,
    manager: Arc<NativeInstanceManager>,
    config_path: Option<std::path::PathBuf>,
) -> anyhow::Result<()> {
    if !settings.enabled {
        tracing::info!("web console disabled (web_console.enabled = false)");
        return Ok(());
    }
    let config = WebConsoleConfig {
        enabled: true,
        listen: settings.listen,
        password: settings.password,
        session_ttl_hours: settings.session_ttl_hours,
        db_path: settings.db_path,
        sync_peers: settings.sync_peers,
        config_path,
    };
    let store = dokitier_web::open_store(&config).await?;
    seed_routing_settings(&store, &routing).await;
    reload_routing_policy(&store).await;
    let hub = Arc::new(MetricsHub::new());
    let data = Arc::new(CoreWebDataSource::new(
        manager.clone(),
        routing,
        store.clone(),
    ));
    dokitier_web::start(config.clone(), store.clone(), hub.clone(), data).await?;
    spawn_metrics_sampler(manager, hub, store);
    Ok(())
}
