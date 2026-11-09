//! Reverse connection from core to the standalone dokitier console.

use std::time::Duration;

use dokitier_controller::{transport::AgentTransport, ConsoleCommand, ConsoleFrame, SessionCipher};
use serde_json::{json, Value};
use tokio::{io::AsyncReadExt, net::TcpStream, time::interval};

use dokitier_core::config::toml::ConfigLoader as _;
use dokitier_core::config::ControllerSettings;

use crate::instance::factory::NativeInstanceManager;

fn networks_dir(config_path: &std::path::Path) -> std::path::PathBuf {
    config_path
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .join("networks")
}

async fn write_atomic(path: &std::path::Path, content: &str) -> std::io::Result<()> {
    use tokio::io::AsyncWriteExt as _;
    let tmp = path.with_extension("toml.tmp");
    let mut handle = tokio::fs::File::create(&tmp).await?;
    handle.write_all(content.as_bytes()).await?;
    handle.sync_all().await?;
    tokio::fs::rename(&tmp, path).await
}

fn valid_network_name(network: &str) -> bool {
    !network.is_empty()
        && network.len() <= 64
        && network != "."
        && network != ".."
        && network
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

fn safe_network_name(network: &str) -> String {
    network
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') { c } else { '_' })
        .collect()
}

pub fn spawn(manager: std::sync::Arc<NativeInstanceManager>, settings: ControllerSettings, config_path: Option<std::path::PathBuf>) {
    if settings.url.is_empty() || settings.node_id.is_empty() {
        tracing::warn!("controller section present but url/node_id is empty, skip agent");
        return;
    }
    tokio::spawn(async move {
        let _ = crate::shared_listener::start(settings.shared_listeners.clone()).await;
        {
            let manager = manager.clone();
            let _ = crate::shared_listener_handoff::install_network_resolver(std::sync::Arc::new(
                move |name: &str| {
                    manager
                        .instances()
                        .into_iter()
                        .find(|instance| {
                            instance.peer_manager().network_identity().network_name == name
                        })
                        .map(|instance| instance.peer_manager().clone())
                },
            ));
        }
        tokio::spawn(async {
            let ips = detect_public_ips().await;
            crate::shared_listener::set_public_ip(ips.0);
            crate::shared_listener::set_public_ip6(ips.1);
        });
        spawn_bandwidth_prober(manager.clone());
        if let Some(path) = config_path.as_deref() {
            let dir = networks_dir(path);
            if let Ok(mut entries) = tokio::fs::read_dir(&dir).await {
                while let Ok(Some(entry)) = entries.next_entry().await {
                    let path = entry.path();
                    if path.extension().and_then(|e| e.to_str()) != Some("toml") {
                        continue;
                    }
                    let Ok(text) = tokio::fs::read_to_string(&path).await else { continue };
                    if let Ok(probe) = crate::common::config::TomlConfigLoader::new_from_str(&text) {
                        let network = probe.get_network_identity().network_name.clone();
                        let text = crate::shared_listener::strip_listeners(&text);
                        let runtime_text = crate::shared_listener::rewrite_listeners(&text, &network);
                        let Ok(cfg) = crate::common::config::TomlConfigLoader::new_from_str(&runtime_text) else { continue };
                        let bandwidth = cfg
                .get_node_bandwidth_up_mbps()
                .zip(cfg.get_node_bandwidth_down_mbps());
            let intranet_cap = cfg.get_node_bandwidth_intranet_down_mbps();
            let internet_cap = cfg.get_node_bandwidth_internet_down_mbps();
                        let started = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            manager.run_network_instance(cfg, crate::common::config::ConfigFileControl::STATIC_CONFIG)
                        }));
                        match started {
                            Ok(Ok(id)) => {
                                if let Some((up, down)) = bandwidth {
                                    register_node_bandwidth(
                                        manager.as_ref(),
                                        id,
                                        up,
                                        down,
                                        intranet_cap,
                                        internet_cap,
                                    );
                                }
                            }
                            Ok(Err(err)) => tracing::warn!(?err, file = %path.display(), "failed to load network config"),
                            Err(_) => tracing::warn!(file = %path.display(), "network instance panicked while loading"),
                        }
                    }
                }
            }
        }
        let mut backoff = 1u64;
        let mut failures: u32 = 0;
        // Spread the very first connection so a console restart does not get a
        // reconnect storm from every agent at the same instant.
        let startup_jitter = (std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0)
            % 3_000) as u64;
        tokio::time::sleep(Duration::from_millis(startup_jitter)).await;
        loop {
            match session(&manager, &settings, config_path.clone()).await {
                Ok(()) => {
                    backoff = 1;
                    failures = 0;
                }
                Err(err) => {
                    failures = failures.saturating_add(1);
                    let reason = classify_disconnect(&err);
                    tracing::warn!(?err, reason, failures, "controller session ended");
                    if reason == "auth" {
                        // Bad token/identity will not self-heal: do not hammer
                        // the console, wait for an operator to fix credentials.
                        backoff = backoff.max(60);
                    }
                }
            }
            // exponential backoff with 50-150% jitter
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos())
                .unwrap_or(0) as u64;
            let jittered = (backoff as f64 * (0.5 + (nanos % 1000) as f64 / 1000.0)).max(0.5);
            // storm suppression: after 5 consecutive failures hold a 60s
            // cooldown ceiling and never busy-loop faster than 2s. A console
            // restart must not leave agents offline for minutes.
            let capped = if failures >= 5 { jittered.min(60.0).max(2.0) } else { jittered };
            tokio::time::sleep(Duration::from_millis((capped * 1000.0) as u64)).await;
            backoff = (backoff * 2).min(60);
        }
    });
}

fn classify_disconnect(err: &anyhow::Error) -> &'static str {
    let text = format!("{err:#}").to_ascii_lowercase();
    if text.contains("proof") || text.contains("unauthorized") || text.contains("forbidden") || text.contains("auth") {
        "auth"
    } else if text.contains("connect") || text.contains("timed out") || text.contains("timeout") || text.contains("unreachable")
        || text.contains("connection reset") || text.contains("broken pipe") || text.contains("dns")
    {
        "network"
    } else if text.contains("decode") || text.contains("frame") || text.contains("protocol") || text.contains("tls") {
        "protocol"
    } else {
        "unknown"
    }
}

fn system_stats() -> (f64, u64, u64) {
    use std::sync::{Mutex, OnceLock};
    static SYS: OnceLock<Mutex<sysinfo::System>> = OnceLock::new();
    let sys = SYS.get_or_init(|| Mutex::new(sysinfo::System::new()));
    let Ok(mut sys) = sys.lock() else { return (0.0, 0, 0) };
    sys.refresh_cpu_usage();
    let cpu = sys.global_cpu_usage() as f64;
    let (used, total) = memory_usage(&sys);
    (cpu, used, total)
}


#[cfg(target_os = "linux")]
fn collect_network_info() -> Value {
    use nix::ifaddrs::getifaddrs;
    use std::collections::BTreeMap;
    let mut map: BTreeMap<String, (Vec<String>, Vec<String>)> = BTreeMap::new();
    if let Ok(addrs) = getifaddrs() {
        for ifa in addrs {
            if ifa.interface_name == "lo" {
                continue;
            }
            let entry = map.entry(ifa.interface_name.clone()).or_default();
            if let Some(address) = ifa.address {
                if let Some(v4) = address.as_sockaddr_in() {
                    let prefix = ifa
                        .netmask
                        .and_then(|mask| mask.as_sockaddr_in().map(|m| u32::from(m.ip()).leading_ones()))
                        .unwrap_or(32);
                    entry.0.push(format!("{}/{prefix}", v4.ip()));
                } else if let Some(v6) = address.as_sockaddr_in6() {
                    let prefix = ifa
                        .netmask
                        .and_then(|mask| mask.as_sockaddr_in6().map(|m| u128::from(m.ip()).leading_ones()))
                        .unwrap_or(128);
                    entry.1.push(format!("{}/{prefix}", v6.ip()));
                }
            }
        }
    }
    let mut interfaces = Vec::new();
    for (name, (ipv4, ipv6)) in map {
        let base = format!("/sys/class/net/{name}");
        let read = |file: &str| {
            std::fs::read_to_string(format!("{base}/{file}"))
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        };
        let speed = read("speed").and_then(|value| value.parse::<i64>().ok());
        let driver = std::fs::read_link(format!("{base}/device/driver"))
            .ok()
            .and_then(|path| path.file_name().map(|name| name.to_string_lossy().to_string()));
        interfaces.push(json!({
            "name": name,
            "state": read("operstate").unwrap_or_default(),
            "mac": read("address").unwrap_or_default(),
            "speed_mbps": speed,
            "driver": driver,
            "ipv4": ipv4,
            "ipv6": ipv6,
        }));
    }
    let mut ipv4_prefixes: Vec<String> = Vec::new();
    let mut ipv6_prefixes: Vec<String> = Vec::new();
    for interface in &interfaces {
        if let Some(list) = interface.get("ipv4").and_then(|v| v.as_array()) {
            for item in list {
                let Some(text) = item.as_str() else { continue };
                let Some((addr, prefix)) = text.split_once('/') else { continue };
                let Ok(ip) = addr.parse::<std::net::Ipv4Addr>() else { continue };
                let Ok(prefix) = prefix.parse::<u32>() else { continue };
                let mask = if prefix == 0 { 0 } else { u32::MAX << (32 - prefix.min(32)) };
                let network = std::net::Ipv4Addr::from(u32::from(ip) & mask);
                ipv4_prefixes.push(format!("{network}/{prefix}"));
            }
        }
        if let Some(list) = interface.get("ipv6").and_then(|v| v.as_array()) {
            for item in list {
                let Some(text) = item.as_str() else { continue };
                let Some((addr, _)) = text.split_once('/') else { continue };
                let Ok(ip) = addr.parse::<std::net::Ipv6Addr>() else { continue };
                let mask = u128::MAX << 64;
                let network = std::net::Ipv6Addr::from(u128::from(ip) & mask);
                ipv6_prefixes.push(format!("{network}/64"));
            }
        }
    }
    ipv4_prefixes.sort();
    ipv4_prefixes.dedup();
    ipv6_prefixes.sort();
    ipv6_prefixes.dedup();
    let gateway = std::fs::read_to_string("/proc/net/route").ok().and_then(|text| {
        text.lines().skip(1).find_map(|line| {
            let mut parts = line.split_whitespace();
            let interface = parts.next()?.to_string();
            let _destination = parts.next()?;
            let gateway = parts.next()?;
            if gateway == "00000000" {
                return None;
            }
            let raw = u32::from_str_radix(gateway, 16).ok()?;
            Some(json!({"gateway": std::net::Ipv4Addr::from(raw.to_le_bytes()).to_string(), "interface": interface}))
        })
    });
    json!({
        "interfaces": interfaces,
        "gateway": gateway,
        "ipv4_prefixes": ipv4_prefixes,
        "ipv6_prefixes": ipv6_prefixes,
    })
}

fn collect_disks() -> Value {
    let disks = sysinfo::Disks::new_with_refreshed_list();
    let list: Vec<Value> = disks
        .list()
        .iter()
        .map(|disk| {
            json!({
                "name": disk.name().to_string_lossy(),
                "mount": disk.mount_point().to_string_lossy(),
                "total": disk.total_space(),
                "available": disk.available_space(),
                "kind": format!("{:?}", disk.kind()),
            })
        })
        .collect();
    json!(list)
}

#[cfg(not(target_os = "linux"))]
fn collect_network_info() -> Value {
    json!({"interfaces": [], "gateway": null})
}

fn os_pretty_name() -> String {
    std::fs::read_to_string("/etc/os-release")
        .ok()
        .and_then(|text| {
            text.lines().find_map(|line| {
                line.strip_prefix("PRETTY_NAME=")
                    .map(|value| value.trim().trim_matches('"').to_string())
            })
        })
        .or_else(sysinfo::System::long_os_version)
        .or_else(sysinfo::System::name)
        .unwrap_or_default()
}

fn has_net_admin() -> bool {
    let Ok(status) = std::fs::read_to_string("/proc/self/status") else { return false };
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("CapEff:") {
            if let Ok(bits) = u64::from_str_radix(rest.trim(), 16) {
                return bits & (1 << 12) != 0;
            }
        }
    }
    false
}

fn can_create_tun() -> bool {
    if !has_net_admin() { return false; }
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/net/tun")
        .map(|_| true)
        .unwrap_or(false)
}

fn can_create_tap() -> bool {
    can_create_tun()
}

fn can_create_vxlan() -> bool {
    has_net_admin() && std::path::Path::new("/sys/module/vxlan").exists()
}

fn system_info() -> Value {
    use std::sync::OnceLock;
    static INFO: OnceLock<Value> = OnceLock::new();
    let base = INFO.get_or_init(|| {
        let mut sys = sysinfo::System::new();
        sys.refresh_cpu_all();
        let cpus = sys.cpus();
        let model = cpus
            .first()
            .map(|cpu| cpu.brand().trim().to_string())
            .unwrap_or_default();
        json!({
            "cpu_model": model,
            "cpu_cores": sys.physical_core_count().unwrap_or(cpus.len()),
            "cpu_threads": cpus.len(),
            "cpu_arch": std::env::consts::ARCH,
            "os_name": sysinfo::System::name().unwrap_or_default(),
            "os_version": os_pretty_name(),
            "kernel": sysinfo::System::kernel_version().unwrap_or_default(),
            "hostname": sysinfo::System::host_name().unwrap_or_default(),
            "boot_time_secs": sysinfo::System::boot_time(),
            "shared_listener": crate::shared_listener::addr(),
            "capabilities": {
                "tun": can_create_tun(),
                "tap": can_create_tap(),
                "vxlan": can_create_vxlan(),
            },
            "network": collect_network_info(),
            "disks": collect_disks(),
        })
    });
    base.clone()
}

fn register_node_bandwidth(
    manager: &NativeInstanceManager,
    id: uuid::Uuid,
    up_mbps: f64,
    down_mbps: f64,
    intranet_down_mbps: Option<f64>,
    internet_down_mbps: Option<f64>,
) {
    if let Some(instance) = manager
        .instances()
        .into_iter()
        .find(|instance| instance.instance_id() == id)
    {
        dokitier_core::peers::link_policy::set_node_bandwidth(instance.peer_id(), up_mbps, down_mbps);
        dokitier_core::peers::link_policy::set_local_bandwidth_caps(
            dokitier_core::peers::link_policy::LocalBandwidthCaps {
                intranet_down_mbps,
                internet_down_mbps,
            },
        );
    }
}

#[cfg(target_os = "linux")]
fn memory_usage(_sys: &sysinfo::System) -> (u64, u64) {
    // htop-like accounting, same as the Sakuio agent collector
    let Ok(content) = std::fs::read_to_string("/proc/meminfo") else { return (0, 0) };
    let (mut total, mut free, mut cached, mut reclaimable, mut buffers, mut shmem) = (0u64, 0u64, 0u64, 0u64, 0u64, 0u64);
    for line in content.lines() {
        let mut parts = line.split_whitespace();
        let Some(key) = parts.next() else { continue };
        let Some(val) = parts.next().and_then(|v| v.parse::<u64>().ok()) else { continue };
        let bytes = val.saturating_mul(1024);
        match key.trim_end_matches(':') {
            "MemTotal" => total = bytes,
            "MemFree" => free = bytes,
            "Cached" => cached = bytes,
            "SReclaimable" => reclaimable = bytes,
            "Buffers" => buffers = bytes,
            "Shmem" => shmem = bytes,
            _ => {}
        }
    }
    if total == 0 { return (0, 0) }
    let mut used = if total >= free + cached + reclaimable + buffers {
        total - (free + cached + reclaimable + buffers)
    } else {
        total.saturating_sub(free)
    };
    used = used.saturating_add(shmem);
    (used, total)
}

#[cfg(not(target_os = "linux"))]
fn memory_usage(sys: &sysinfo::System) -> (u64, u64) {
    (sys.used_memory(), sys.total_memory())
}

async fn snapshot(manager: &NativeInstanceManager, public_ipv4: Option<&str>, public_ipv6: Option<&str>) -> Value {
    let mut instances = Vec::new();
    for instance in manager.instances() {
        let node = instance.node_snapshot().await;
        let identity = instance.peer_manager().network_identity();
        let me = instance.peer_id();
        let local_bw = dokitier_core::peers::link_policy::node_bandwidth(me);
        let mut topo_nodes = vec![json!({
            "peer_id": me,
            "instance_id": instance.instance_id().to_string(),
            "bandwidth_up_mbps": local_bw.map(|bw| bw.up_mbps),
            "bandwidth_down_mbps": local_bw.map(|bw| bw.down_mbps),
            "hostname": node.hostname,
            "ipv4": node.ipv4_addr.map(|x| x.to_string()),
            "version": node.version,
            "local": true,
        })];
        for route in instance.route_snapshots().await {
            let route_bw = dokitier_core::peers::link_policy::node_bandwidth(route.peer_id);
            topo_nodes.push(json!({
                "peer_id": route.peer_id,
                "instance_id": route.inst_id,
                "bandwidth_up_mbps": route_bw.map(|bw| bw.up_mbps),
                "bandwidth_down_mbps": route_bw.map(|bw| bw.down_mbps),
                "hostname": route.hostname,
                "ipv4": route.ipv4_addr.map(|x| x.to_string()),
                "version": route.version,
                "next_hop_peer_id": route.next_hop_peer_id,
                "cost": route.cost,
                "local": false,
            }));
        }
        let mut topo_edges = Vec::new();
        for peer in instance.peer_snapshots().await {
            if peer.conns.is_empty() { continue; }
            let metrics = dokitier_core::peers::link_policy::peer_metrics(peer.peer_id);
            let tunnels: Vec<serde_json::Value> = peer.conns.iter().map(|conn| {
                let stats = conn.stats.clone().unwrap_or_default();
                json!({
                    "type": conn.tunnel.as_ref().map(|t| t.tunnel_type.clone()).unwrap_or_default(),
                    "remote_addr": conn.tunnel.as_ref().and_then(|t| t.remote_addr.as_ref()).map(|u| u.to_string()),
                    "local_addr": conn.tunnel.as_ref().and_then(|t| t.local_addr.as_ref()).map(|u| u.to_string()),
                    "latency_ms": (stats.latency_us > 0).then(|| stats.latency_us as f64 / 1000.0),
                    "loss_pct": conn.loss_rate as f64 * 100.0,
                    "rx_bytes": stats.rx_bytes,
                    "tx_bytes": stats.tx_bytes,
                    "transport_loss_pct": metrics.as_ref().map(|m| m.transport_loss_pct).unwrap_or(0.0),
                    "transport_rtt_us": metrics.as_ref().and_then(|m| m.transport_rtt_us),
                    "capacity_mbps": metrics.as_ref().map(|m| m.capacity_mbps.round()).unwrap_or(0.0),
                    "capacity_source": if metrics.as_ref().and_then(|m| m.probe_capacity_mbps).is_some() {
                        "probe"
                    } else {
                        "unmeasured"
                    },
                    "utilization": metrics.as_ref().map(|m| m.utilization).unwrap_or(0.0),
                })
            }).collect();
            let advertised = dokitier_core::peers::link_policy::node_bandwidth(peer.peer_id);
            topo_edges.push(json!({
                "a": me, "b": peer.peer_id, "tunnels": tunnels,
                "advertised_up_mbps": advertised.map(|bw| bw.up_mbps),
                "advertised_down_mbps": advertised.map(|bw| bw.down_mbps),
            }));
        }
        instances.push(json!({
            "instance_id": node.instance_id.to_string(),
            "peer_id": me,
            "hostname": node.hostname,
            "version": node.version,
            "network_name": identity.network_name,
            "p2p_group": identity.p2p_group,
            "ipv4": node.ipv4_addr.map(|x| x.to_string()),
            "ipv6": node.ipv6_addr.map(|x| x.to_string()),
            "topology": {"nodes": topo_nodes, "edges": topo_edges},
        }));
    }
    let (cpu_raw, mem_used, mem_total) = system_stats();
    let cpu_pct = if cpu_raw.is_finite() { cpu_raw.clamp(0.0, 100.0) } else { 0.0 };
    let mem_pct = if mem_total > 0 {
        (mem_used as f64 * 100.0 / mem_total as f64).clamp(0.0, 100.0)
    } else {
        0.0
    };
    json!({
        "ts": chrono::Utc::now().timestamp(),
        "metrics_version": 2,
        "uptime_secs": sysinfo::System::uptime(),
        "public_ipv4": public_ipv4,
        "public_ipv6": public_ipv6,
        "cpu_pct": cpu_pct,
        "mem_used": mem_used,
        "mem_total": mem_total,
        "mem_pct": mem_pct,
        "instances": instances,
    })
}

async fn handle_command(
    manager: &std::sync::Arc<NativeInstanceManager>,
    config_path: Option<&std::path::Path>,
    command: ConsoleCommand,
) -> (bool, Option<String>, Option<String>) {
    use tokio::io::AsyncWriteExt;
    match command {
        ConsoleCommand::GetConfig => match config_path {
            Some(path) => match tokio::fs::read_to_string(path).await {
                Ok(toml) => {
                    let toml = toml
                        .lines()
                        .map(|line| {
                            let trimmed = line.trim_start();
                            if trimmed.starts_with("token") && trimmed.contains('=') {
                                let indent = &line[..line.len() - trimmed.len()];
                                format!("{indent}token = \"<redacted>\"")
                            } else {
                                line.to_string()
                            }
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    (true, None, Some(toml))
                }
                Err(err) => (false, Some(err.to_string()), None),
            },
            None => (false, Some("core was not started from a config file".into()), None),
        },
        ConsoleCommand::ApplySharedListeners { listeners } => {
            let listeners: Vec<String> = listeners.into_iter().filter(|value| !value.trim().is_empty()).collect();
            match crate::shared_listener::apply(listeners.clone()).await {
                Ok(_) => {
                    if let Some(path) = config_path {
                        if let Ok(current) = tokio::fs::read_to_string(path).await {
                            if let Ok(mut value) = toml::from_str::<toml::Value>(&current) {
                                if let Some(table) = value.as_table_mut() {
                                    // The master config keeps the node's public
                                    // listeners in the standard top-level
                                    // `listeners` field.
                                    if listeners.is_empty() {
                                        table.remove("listeners");
                                    } else {
                                        table.insert(
                                            "listeners".to_string(),
                                            toml::Value::Array(
                                                listeners
                                                    .iter()
                                                    .cloned()
                                                    .map(toml::Value::String)
                                                    .collect(),
                                            ),
                                        );
                                    }
                                    // Drop the legacy spelling if an older
                                    // config still carries it.
                                    if let Some(controller) = table
                                        .get_mut("controller")
                                        .and_then(|controller| controller.as_table_mut())
                                    {
                                        controller.remove("shared_listeners");
                                    }
                                    if let Ok(text) = toml::to_string(&value) {
                                        let _ = write_atomic(path, &text).await;
                                    }
                                }
                            }
                        }
                    }
                    (true, None, None)
                }
                Err(err) => (false, Some(err.to_string()), None),
            }
        }
        ConsoleCommand::ApplyConfig { toml } => {
            // board mode keeps controller credentials in the startup config; never
            // overwrite that file with a pushed network config
            if let Some(path) = config_path {
                if let Ok(current) = tokio::fs::read_to_string(path).await {
                    if current.contains("[controller]") {
                        return (
                            false,
                            Some("apply_config is disabled in board mode; use apply_network".into()),
                            None,
                        );
                    }
                }
            }
            if let Err(err) = toml::from_str::<toml::Value>(&toml) {
                return (false, Some(format!("invalid TOML: {err}")), None);
            }
            let Some(path) = config_path else {
                return (false, Some("core was not started from a config file".into()), None);
            };
            let cfg = match crate::common::config::TomlConfigLoader::new_from_str(&toml) {
                Ok(cfg) => cfg,
                Err(err) => return (false, Some(format!("invalid config: {err}")), None),
            };
            let tmp = path.with_extension("toml.tmp");
            let write = async {
                let mut file = tokio::fs::File::create(&tmp).await?;
                file.write_all(toml.as_bytes()).await?;
                file.sync_all().await?;
                tokio::fs::rename(&tmp, path).await
            };
            if let Err(err) = write.await {
                return (false, Some(err.to_string()), None);
            }
            let ids = manager.instance_ids();
            if let Err(err) = manager.delete_network_instances(ids).await {
                return (false, Some(err.to_string()), None);
            }
            match manager.run_network_instance(cfg, crate::common::config::ConfigFileControl::STATIC_CONFIG) {
                Ok(_) => (true, None, None),
                Err(err) => (false, Some(err.to_string()), None),
            }
        }
        ConsoleCommand::ApplyNetwork { network, toml } => {
            if !valid_network_name(&network) {
                return (false, Some("invalid network name".into()), None);
            }
            if let Err(err) = toml::from_str::<toml::Value>(&toml) {
                return (false, Some(format!("invalid TOML: {err}")), None);
            }
            let Some(path) = config_path else {
                return (false, Some("core was not started from a config file".into()), None);
            };
            let toml = crate::shared_listener::strip_listeners(&toml);
            let runtime_toml = crate::shared_listener::rewrite_listeners(&toml, &network);
            let cfg = match crate::common::config::TomlConfigLoader::new_from_str(&runtime_toml) {
                Ok(cfg) => cfg,
                Err(err) => return (false, Some(format!("invalid config: {err}")), None),
            };
            if let Ok(value) = toml::from_str::<toml::Value>(&runtime_toml) {
                if let Some(netns) = value.get("netns").and_then(|v| v.as_str()) {
                    let direct = std::path::Path::new(netns);
                    let named = std::path::Path::new("/var/run/netns").join(netns);
                    if !direct.exists() && !named.exists() {
                        return (false, Some(format!("netns not found: {netns}")), None);
                    }
                }
            }
            let configured = cfg.get_network_identity().network_name;
            if configured != network {
                return (
                    false,
                    Some(format!("network_name mismatch: request={network:?} config={configured:?}")),
                    None,
                );
            }
            let dir = networks_dir(path);
            if let Err(err) = tokio::fs::create_dir_all(&dir).await {
                return (false, Some(err.to_string()), None);
            }
            let file = dir.join(format!("{}.toml", safe_network_name(&network)));
            let previous = tokio::fs::read_to_string(&file).await.ok();
            if let Err(err) = write_atomic(&file, &toml).await {
                return (false, Some(err.to_string()), None);
            }
            let ids: Vec<_> = manager
                .instances()
                .iter()
                .filter(|instance| instance.peer_manager().network_identity().network_name == network)
                .map(|instance| instance.instance_id())
                .collect();
            if let Err(err) = manager.delete_network_instances(ids).await {
                return (false, Some(err.to_string()), None);
            }
            let bandwidth = cfg
                .get_node_bandwidth_up_mbps()
                .zip(cfg.get_node_bandwidth_down_mbps());
            let intranet_cap = cfg.get_node_bandwidth_intranet_down_mbps();
            let internet_cap = cfg.get_node_bandwidth_internet_down_mbps();
            let started = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                manager.run_network_instance(cfg, crate::common::config::ConfigFileControl::STATIC_CONFIG)
            }));
            let outcome = match started {
                Ok(result) => result.map_err(|err| err.to_string()),
                Err(_) => Err("network instance panicked while starting".to_string()),
            };
            match outcome {
                Ok(id) => {
                    if let Some((up, down)) = bandwidth {
                        register_node_bandwidth(
                            manager.as_ref(),
                            id,
                            up,
                            down,
                            intranet_cap,
                            internet_cap,
                        );
                    }
                    (true, None, None)
                }
                Err(err) => {
                    // roll back the on-disk config and restart the previous instance
                    match previous {
                        Some(prev) => {
                            let _ = write_atomic(&file, &prev).await;
                            if let Ok(prev_cfg) = crate::common::config::TomlConfigLoader::new_from_str(&prev) {
                                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                    manager.run_network_instance(prev_cfg, crate::common::config::ConfigFileControl::STATIC_CONFIG)
                                }));
                            }
                        }
                        None => {
                            let _ = tokio::fs::remove_file(&file).await;
                        }
                    }
                    (false, Some(err), None)
                }
            }
        }
        ConsoleCommand::DeleteNetwork { network } => {
            if !valid_network_name(&network) {
                return (false, Some("invalid network name".into()), None);
            }
            let Some(path) = config_path else {
                return (false, Some("core was not started from a config file".into()), None);
            };
            let dir = networks_dir(path);
            let file = dir.join(format!("{}.toml", network));
            if let Err(err) = tokio::fs::remove_file(&file).await {
                if err.kind() != std::io::ErrorKind::NotFound {
                    return (false, Some(err.to_string()), None);
                }
            }
            let ids: Vec<_> = manager
                .instances()
                .iter()
                .filter(|instance| instance.peer_manager().network_identity().network_name == network)
                .map(|instance| instance.instance_id())
                .collect();
            match manager.delete_network_instances(ids).await {
                Ok(_) => (true, None, None),
                Err(err) => (false, Some(err.to_string()), None),
            }
        }
        ConsoleCommand::ApplyRouting { params } => {
            let values = [
                params.latency_weight,
                params.loss_weight,
                params.bandwidth_weight,
                params.reference_bandwidth_mbps,
                params.utilization_weight,
                params.hop_weight,
                params.loss_gain,
                params.tcp_loss_weight,
                params.udp_loss_weight,
                params.kcp_loss_weight,
                params.quic_loss_weight,
                params.wg_loss_weight,
                params.min_bandwidth_mbps,
            ];
            if values.iter().any(|value| !value.is_finite() || *value < 0.0) {
                return (false, Some("routing params must be finite and non-negative".into()), None);
            }
            if params.reference_bandwidth_mbps <= 0.0 {
                return (false, Some("reference_bandwidth_mbps must be greater than 0".into()), None);
            }
            dokitier_core::peers::bandwidth_probe::set_probe_config(
                dokitier_core::peers::bandwidth_probe::ProbeConfig {
                    enabled: params.probe_enabled,
                    interval_secs: params.probe_interval_secs,
                    payload_bytes: params.probe_payload_bytes,
                    repeats: params.probe_repeats,
                    max_concurrent: params.probe_max_concurrent,
                    idle_utilization_pct: params.probe_idle_utilization_pct,
                    timeout_ms: params.probe_timeout_ms,
                    max_range_mbps: params.probe_max_range_mbps,
                },
            );
            dokitier_core::peers::link_policy::set_routing_weights(dokitier_core::config::RoutingWeights {
                latency_weight: params.latency_weight,
                loss_weight: params.loss_weight,
                bandwidth_weight: params.bandwidth_weight,
                reference_bandwidth_mbps: params.reference_bandwidth_mbps,
                utilization_weight: params.utilization_weight,
                hop_weight: params.hop_weight,
                loss_gain: params.loss_gain,
                tcp_loss_weight: params.tcp_loss_weight,
                udp_loss_weight: params.udp_loss_weight,
                kcp_loss_weight: params.kcp_loss_weight,
                quic_loss_weight: params.quic_loss_weight,
                wg_loss_weight: params.wg_loss_weight,
                min_bandwidth_mbps: params.min_bandwidth_mbps,
                weighted: params.weighted,
            });
            (true, None, None)
        }
        ConsoleCommand::RestartInstance => (false, Some("restart_instance is not implemented yet".into()), None),
    }
}

const PUBLIC_IP_USER_AGENT: &str = "curl/8.0.1";

fn parse_ip(text: &str, version: u8) -> Option<String> {
    let matches_version = |ip: std::net::IpAddr| matches!((version, ip), (4, std::net::IpAddr::V4(_)) | (6, std::net::IpAddr::V6(_)));
    if let Ok(value) = serde_json::from_str::<Value>(text) {
        for key in ["ip", "query", "origin"] {
            if let Some(ip) = value.get(key).and_then(|v| v.as_str()).and_then(|s| s.trim().parse::<std::net::IpAddr>().ok()) {
                if matches_version(ip) {
                    return Some(ip.to_string());
                }
            }
        }
    }
    for token in text.split(|c: char| !(c.is_ascii_hexdigit() || c == '.' || c == ':')) {
        if let Ok(ip) = token.parse::<std::net::IpAddr>() {
            if matches_version(ip) {
                return Some(ip.to_string());
            }
        }
    }
    None
}

fn fetch_ip(agent: &ureq::Agent, url: &str, version: u8) -> Option<String> {
    let response = match agent.get(url).call() {
        Ok(response) => response,
        Err(err) => {
            eprintln!("public ip probe failed: {url}: {err}");
            return None;
        }
    };
    let text = match response.into_string() {
        Ok(text) => text,
        Err(err) => {
            eprintln!("public ip probe body failed: {url}: {err}");
            return None;
        }
    };
    let parsed = parse_ip(&text, version);
    eprintln!("public ip probe: {url}: {parsed:?}");
    parsed
}

fn detect_ip_blocking(version: u8) -> Option<String> {
    // provider list and user agent mirror the Sakuio agent collector; ureq's
    // bundled webpki roots avoid depending on the host CA store
    let urls: &[&str] = if version == 4 {
        &[
            "https://api.ipify.org?format=json",
            "http://ipv4.ip.sb",
            "http://ip-api.com/json",
            "https://edge-ip.html.zone/geo",
            "https://vercel-ip.html.zone/geo",
            "https://www.visa.cn/cdn-cgi/trace",
            "https://www.qualcomm.cn/cdn-cgi/trace",
        ]
    } else {
        &[
            "https://api6.ipify.org?format=json",
            "https://v6.ip.zxinc.org/info.php?type=json",
            "https://ipv6.icanhazip.com",
            "http://api-ipv6.ip.sb/geoip",
        ]
    };
    let agent: ureq::Agent = ureq::AgentBuilder::new()
        .user_agent(PUBLIC_IP_USER_AGENT)
        .timeout(Duration::from_secs(8))
        .build();
    for url in urls {
        if let Some(found) = fetch_ip(&agent, url, version) {
            return Some(found);
        }
    }
    None
}

async fn detect_ip(version: u8) -> Option<String> {
    tokio::task::spawn_blocking(move || detect_ip_blocking(version))
        .await
        .ok()
        .flatten()
}

async fn detect_public_ips() -> (Option<String>, Option<String>) {
    let (v4, v6) = tokio::join!(detect_ip(4), detect_ip(6));
    (v4, v6)
}

fn spawn_bandwidth_prober(manager: std::sync::Arc<NativeInstanceManager>) {
    use dokitier_proto::{
        peer_rpc::{BandwidthProbeRequest, BandwidthProbeRpcClientFactory},
        rpc_types::controller::{BaseController, Controller},
    };
    tokio::spawn(async move {
        let mut last_probe: std::collections::HashMap<(u32, u32), std::time::Instant> =
            std::collections::HashMap::new();
        loop {
            let probe_cfg = dokitier_core::peers::bandwidth_probe::get_probe_config();
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            if !probe_cfg.enabled {
                continue;
            }
            let mut probes_this_cycle: u32 = 0;
            for instance in manager.instances() {
                let identity = instance.peer_manager().network_identity();
                let my_peer_id = instance.peer_id();
                let rpc = instance.peer_manager().get_peer_rpc_mgr();
                for peer in instance.peer_snapshots().await {
                    if peer.conns.is_empty() {
                        continue;
                    }
                    let util = dokitier_core::peers::link_policy::peer_metrics(peer.peer_id)
                        .map(|m| m.utilization)
                        .unwrap_or(0.0);
                    if util * 100.0 > probe_cfg.idle_utilization_pct {
                        continue;
                    }
                    if probes_this_cycle >= probe_cfg.max_concurrent.max(1) {
                        continue;
                    }
                    let key = (my_peer_id, peer.peer_id);
                    if last_probe
                        .get(&key)
                        .map(|at| at.elapsed() < std::time::Duration::from_secs(probe_cfg.interval_secs))
                        .unwrap_or(false)
                    {
                        continue;
                    }
                    let rtt_secs = peer
                        .conns
                        .iter()
                        .filter_map(|conn| conn.stats.as_ref())
                        .map(|stats| stats.latency_us as f64 / 1_000_000.0)
                        .filter(|value| *value > 0.0)
                        .reduce(f64::min)
                        .unwrap_or(0.0);
                    let client = rpc.rpc_client().scoped_client::<
                        BandwidthProbeRpcClientFactory<BaseController>,
                    >(my_peer_id, peer.peer_id, identity.network_name.clone());
                    probes_this_cycle += 1;
                    // small stagger so probes do not burst simultaneously
                    tokio::time::sleep(std::time::Duration::from_millis(
                        (peer.peer_id % 400) as u64,
                    ))
                    .await;
                    // Larger chunks amortize RPC/compression overhead; 32 KB
                    // frames stay well under the 128 KB tunnel frame limit.
                    let chunk_bytes: u32 = 32 * 1024;
                    let total_chunks = (probe_cfg.payload_bytes / chunk_bytes).clamp(1, 16);
                    let mut measured: Vec<f64> = Vec::new();
                    for _ in 0..probe_cfg.repeats.max(1) {
                        let started = std::time::Instant::now();
                        let mut calls = Vec::new();
                        for index in 0..total_chunks {
                            let rpc = rpc.clone();
                            let network = identity.network_name.clone();
                            let timeout_ms = probe_cfg.timeout_ms as i32;
                            calls.push(async move {
                                let client = rpc.rpc_client().scoped_client::<
                                    BandwidthProbeRpcClientFactory<BaseController>,
                                >(my_peer_id, peer.peer_id, network);
                                let mut ctrl = BaseController::default();
                                ctrl.set_timeout_ms(timeout_ms);
                                client
                                    .probe(
                                        ctrl,
                                        BandwidthProbeRequest {
                                            size_bytes: chunk_bytes,
                                            nonce: index as u64,
                                        },
                                    )
                                    .await
                            });
                        }
                        let results = futures::future::join_all(calls).await;
                        let bytes: usize = results
                            .iter()
                            .filter_map(|result| result.as_ref().ok())
                            .map(|response| response.payload.len())
                            .sum();
                        let elapsed = started.elapsed().as_secs_f64();
                        if bytes > 0 {
                            if let Some(capacity) =
                                dokitier_core::peers::bandwidth_probe::capacity_from_probe(
                                    bytes as u64,
                                    elapsed,
                                    rtt_secs,
                                )
                            {
                                measured.push(capacity);
                            }
                        }
                    }
                    if !measured.is_empty() {
                        measured.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                        let capacity = measured[measured.len() / 2]
                            .min(probe_cfg.max_range_mbps);
                        dokitier_core::peers::link_policy::set_peer_capacity(peer.peer_id, capacity);
                        dokitier_core::peers::link_policy::set_peer_probe_capacity(
                            peer.peer_id,
                            capacity,
                        );
                        dokitier_core::peers::transport_metrics::publish(
                            format!("{my_peer_id}:{}", peer.peer_id),
                            dokitier_core::peers::transport_metrics::TransportSample {
                                proto: "probe".to_string(),
                                capacity_mbps: Some(capacity),
                                ..Default::default()
                            },
                        );
                        eprintln!(
                            "BPROBE peer={} samples={} cap={:.0}Mbps",
                            peer.peer_id,
                            measured.len(),
                            capacity
                        );
                        last_probe.insert(key, std::time::Instant::now());
                    }
                }
            }
        }
    });
}

async fn hello(manager: &NativeInstanceManager, settings: &ControllerSettings) -> Value {
    let public_ips = detect_public_ips().await;
    crate::shared_listener::set_public_ip(public_ips.0.clone());
    let (hostname, version) = match manager.instances().first() {
        Some(instance) => {
            let n = instance.node_snapshot().await;
            (n.hostname, n.version)
        }
        None => (String::new(), String::new()),
    };
    let networks: Vec<serde_json::Value> = manager
        .instances()
        .iter()
        .map(|instance| {
            let identity = instance.peer_manager().network_identity();
            json!({
                "name": identity.network_name,
                "p2p_group": identity.p2p_group,
            })
        })
        .collect();
    let mut system = system_info();
    if let Some(object) = system.as_object_mut() {
        object.insert(
            "shared_listener".to_string(),
            serde_json::json!(crate::shared_listener::addr()),
        );
        object.insert(
            "shared_listener_stats".to_string(),
            crate::shared_listener::stats(),
        );
    }
    json!({
        "type": "hello",
        "node_id": settings.node_id,
        "token": settings.token,
        "hostname": hostname,
        "version": version,
        "networks": networks,
        "public_ipv4": public_ips.0,
        "public_ipv6": public_ips.1,
        "instance_id": settings.instance_id,
        "system": system,
    })
}

async fn session(
    manager: &std::sync::Arc<NativeInstanceManager>,
    settings: &ControllerSettings,
    config_path: Option<std::path::PathBuf>,
) -> anyhow::Result<()> {
    let (public_ipv4, public_ipv6) = detect_public_ips().await;
    let mut transport = tokio::time::timeout(
        Duration::from_secs(10),
        AgentTransport::connect(&settings.url),
    )
    .await
    .map_err(|_| anyhow::anyhow!("controller connect timeout"))??;

    if settings.url.starts_with("quic://") {
        transport.send(&serde_json::json!({"type":"start"})).await?;
    }
    tracing::info!("waiting for controller challenge");
    let challenge = transport.recv().await?;
    tracing::info!(?challenge, "controller challenge received");
    let (nonce, ts) = match serde_json::from_value::<ConsoleFrame>(challenge)? {
        ConsoleFrame::Challenge { nonce, ts } => (nonce, ts),
        other => anyhow::bail!("expected challenge, got {other:?}"),
    };
    let client_proof = dokitier_controller::proof(&settings.token, "client", &settings.node_id, &nonce, ts);
    let mut hello = hello(manager, settings).await;
    hello["nonce"] = json!(nonce);
    hello["ts"] = json!(ts);
    hello["proof"] = json!(client_proof);
    transport.send(&hello).await?;
    let response = transport.recv().await?;
    match serde_json::from_value::<ConsoleFrame>(response)? {
        ConsoleFrame::ChallengeOk { proof: server_proof } => {
            let expected = dokitier_controller::proof(&settings.token, "server", &settings.node_id, &nonce, ts);
            if expected != server_proof {
                anyhow::bail!("controller server proof mismatch");
            }
        }
        other => anyhow::bail!("expected challenge_ok, got {other:?}"),
    }
    let cipher = SessionCipher::new(&settings.token, &nonce, ts);
    tracing::info!(endpoint = %settings.url, node_id = %settings.node_id, "controller agent connected");

    let interval = Duration::from_secs(settings.interval_secs.max(1));
    let heartbeat_every = Duration::from_secs(settings.heartbeat_secs.max(5));
    let mut last_heartbeat = tokio::time::Instant::now();
    loop {
        match tokio::time::timeout(interval, transport.recv()).await {
            Ok(Ok(value)) => {
                let value = if value.get("type").and_then(|v| v.as_str()) == Some("enc") {
                    cipher.decrypt(&value).unwrap_or(serde_json::Value::Null)
                } else {
                    value
                };
                if let Ok(frame) = serde_json::from_value::<ConsoleFrame>(value) {
                    if let ConsoleFrame::Command { request_id, command } = frame {
                        let (ok, error, config) = handle_command(manager, config_path.as_deref(), command).await;
                        let reply = serde_json::json!({
                            "type": "result",
                            "node_id": settings.node_id,
                            "request_id": request_id,
                            "ok": ok,
                            "error": error,
                            "config": config,
                        });
                        transport.send(&cipher.encrypt(&reply)).await?;
                    }
                }
            }
            Ok(Err(err)) => return Err(err),
            Err(_) => {}
        }
        if last_heartbeat.elapsed() >= heartbeat_every {
            let heartbeat = serde_json::json!({
                "type": "heartbeat",
                "node_id": settings.node_id,
                "ts": chrono::Utc::now().timestamp(),
            });
            transport.send(&heartbeat).await?;
            last_heartbeat = tokio::time::Instant::now();
        }
        let snap = serde_json::json!({
            "type": "snapshot",
            "node_id": settings.node_id,
            "data": snapshot(manager, public_ipv4.as_deref(), public_ipv6.as_deref()).await,
        });
        transport.send(&cipher.encrypt(&snap)).await?;
    }
}
