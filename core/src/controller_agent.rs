//! Reverse connection from core to the standalone dokitier console.

use std::time::Duration;

use dokitier_controller::write_frame;
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
    let Some(rest) = settings.url.strip_prefix("tcp://") else {
        anyhow::bail!("only tcp:// controller endpoints are implemented so far: {}", settings.url);
    };
    let mut stream = TcpStream::connect(rest).await?;
    write_frame(&mut stream, &hello(manager, settings).await).await?;
    tracing::info!(endpoint = %settings.url, node_id = %settings.node_id, "controller agent connected");

    let mut ticker = interval(Duration::from_secs(settings.interval_secs.max(1)));
    let mut buf = [0u8; 4096];
    loop {
        tokio::select! {
            _ = ticker.tick() => {
                let snap = json!({
                    "type": "snapshot",
                    "node_id": settings.node_id,
                    "data": snapshot(manager).await,
                });
                write_frame(&mut stream, &snap).await?;
            }
            n = stream.read(&mut buf) => {
                let n = n?;
                if n == 0 {
                    anyhow::bail!("controller closed the connection");
                }
            }
        }
    }
}
