//! Reverse connection from core to the standalone dokitier console.

use std::time::Duration;

use dokitier_controller::{transport::AgentTransport, ConsoleFrame};
use serde_json::{json, Value};
use tokio::{io::AsyncReadExt, net::TcpStream, time::interval};

use dokitier_core::config::ControllerSettings;

use crate::instance::factory::NativeInstanceManager;

pub fn spawn(manager: std::sync::Arc<NativeInstanceManager>, settings: ControllerSettings) {
    if settings.url.is_empty() || settings.node_id.is_empty() {
        tracing::warn!("controller section present but url/node_id is empty, skip agent");
        return;
    }
    tokio::spawn(async move {
        let mut backoff = 1u64;
        loop {
            match session(&manager, &settings).await {
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
        instances.push(json!({
            "instance_id": node.instance_id.to_string(),
            "peer_id": instance.peer_id(),
            "hostname": node.hostname,
            "version": node.version,
            "ipv4": node.ipv4_addr.map(|x| x.to_string()),
            "ipv6": node.ipv6_addr.map(|x| x.to_string()),
        }));
    }
    json!({ "ts": chrono::Utc::now().timestamp(), "instances": instances })
}

async fn hello(manager: &NativeInstanceManager, settings: &ControllerSettings) -> Value {
    let (hostname, version) = match manager.instances().first() {
        Some(instance) => {
            let n = instance.node_snapshot().await;
            (n.hostname, n.version)
        }
        None => (String::new(), String::new()),
    };
    json!({
        "type": "hello",
        "node_id": settings.node_id,
        "token": settings.token,
        "hostname": hostname,
        "version": version,
        "networks": [],
    })
}

async fn session(
    manager: &std::sync::Arc<NativeInstanceManager>,
    settings: &ControllerSettings,
) -> anyhow::Result<()> {
    let mut transport = AgentTransport::connect(&settings.url).await?;
    transport.send(&hello(manager, settings).await).await?;
    tracing::info!(endpoint = %settings.url, node_id = %settings.node_id, "controller agent connected");

    let interval = Duration::from_secs(settings.interval_secs.max(1));
    loop {
        match tokio::time::timeout(interval, transport.recv()).await {
            Ok(Ok(value)) => {
                if let Ok(frame) = serde_json::from_value::<ConsoleFrame>(value) {
                    match frame {
                        ConsoleFrame::Command { request_id, .. } => {
                            let reply = serde_json::json!({
                                "type": "result",
                                "node_id": settings.node_id,
                                "request_id": request_id,
                                "ok": false,
                                "error": "config commands are not implemented by this core yet",
                            });
                            transport.send(&reply).await?;
                        }
                        _ => {}
                    }
                }
            }
            Ok(Err(err)) => return Err(err),
            Err(_) => {}
        }
        let snap = serde_json::json!({
            "type": "snapshot",
            "node_id": settings.node_id,
            "data": snapshot(manager).await,
        });
        transport.send(&snap).await?;
    }
}
