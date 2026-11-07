//! Reverse connection from core to the standalone dokitier console.

use std::time::Duration;

use dokitier_controller::{transport::AgentTransport, ConsoleCommand, ConsoleFrame};
use serde_json::{json, Value};
use tokio::{io::AsyncReadExt, net::TcpStream, time::interval};

use dokitier_core::config::ControllerSettings;

use crate::instance::factory::NativeInstanceManager;

pub fn spawn(manager: std::sync::Arc<NativeInstanceManager>, settings: ControllerSettings, config_path: Option<std::path::PathBuf>) {
    if settings.url.is_empty() || settings.node_id.is_empty() {
        tracing::warn!("controller section present but url/node_id is empty, skip agent");
        return;
    }
    tokio::spawn(async move {
        let mut backoff = 1u64;
        loop {
            match session(&manager, &settings, config_path.clone()).await {
                Ok(()) => backoff = 1,
                Err(err) => {
                    tracing::warn!(?err, "controller session ended");
                }
            }
            tokio::time::sleep(Duration::from_secs(backoff)).await;
            backoff = (backoff * 2).min(30);
        }
    });
}

async fn snapshot(manager: &NativeInstanceManager) -> Value {
    let mut instances = Vec::new();
    for instance in manager.instances() {
        let node = instance.node_snapshot().await;
        let identity = instance.peer_manager().network_identity();
        let me = instance.peer_id();
        let mut topo_nodes = vec![json!({
            "peer_id": me,
            "hostname": node.hostname,
            "ipv4": node.ipv4_addr.map(|x| x.to_string()),
            "version": node.version,
            "local": true,
        })];
        for route in instance.route_snapshots().await {
            topo_nodes.push(json!({
                "peer_id": route.peer_id,
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
            let tunnels: Vec<serde_json::Value> = peer.conns.iter().map(|conn| {
                let stats = conn.stats.clone().unwrap_or_default();
                json!({
                    "type": conn.tunnel.as_ref().map(|t| t.tunnel_type.clone()).unwrap_or_default(),
                    "remote_addr": conn.tunnel.as_ref().and_then(|t| t.remote_addr.as_ref()).map(|u| u.to_string()),
                    "latency_ms": (stats.latency_us > 0).then(|| stats.latency_us as f64 / 1000.0),
                    "loss_pct": conn.loss_rate as f64 * 100.0,
                    "rx_bytes": stats.rx_bytes,
                    "tx_bytes": stats.tx_bytes,
                })
            }).collect();
            topo_edges.push(json!({"a": me, "b": peer.peer_id, "tunnels": tunnels}));
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
    json!({ "ts": chrono::Utc::now().timestamp(), "instances": instances })
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
                Ok(toml) => (true, None, Some(toml)),
                Err(err) => (false, Some(err.to_string()), None),
            },
            None => (false, Some("core was not started from a config file".into()), None),
        },
        ConsoleCommand::ApplyConfig { toml } => {
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
        ConsoleCommand::RestartInstance => (false, Some("restart_instance is not implemented yet".into()), None),
    }
}

async fn hello(manager: &NativeInstanceManager, settings: &ControllerSettings) -> Value {
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
    json!({
        "type": "hello",
        "node_id": settings.node_id,
        "token": settings.token,
        "hostname": hostname,
        "version": version,
        "networks": networks,
    })
}

async fn session(
    manager: &std::sync::Arc<NativeInstanceManager>,
    settings: &ControllerSettings,
    config_path: Option<std::path::PathBuf>,
) -> anyhow::Result<()> {
    let mut transport = tokio::time::timeout(
        Duration::from_secs(10),
        AgentTransport::connect(&settings.url),
    )
    .await
    .map_err(|_| anyhow::anyhow!("controller connect timeout"))??;

    let challenge = transport.recv().await?;
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
    tracing::info!(endpoint = %settings.url, node_id = %settings.node_id, "controller agent connected");

    let interval = Duration::from_secs(settings.interval_secs.max(1));
    let heartbeat_every = Duration::from_secs(settings.heartbeat_secs.max(5));
    let mut last_heartbeat = tokio::time::Instant::now();
    loop {
        match tokio::time::timeout(interval, transport.recv()).await {
            Ok(Ok(value)) => {
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
                        transport.send(&reply).await?;
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
            "data": snapshot(manager).await,
        });
        transport.send(&snap).await?;
    }
}
