use std::{net::SocketAddr, sync::Arc};

use futures::stream::FuturesUnordered;

use crate::socket::{
    IpVersion,
    udp::{
        UdpBindOptions, UdpSession, UdpSessionLayer, UdpSessionProtocol, VirtualUdpSocketFactory,
    },
};

use super::first_success;

pub struct ConnectedUdpSession {
    session: UdpSession,
    keep_alive: Box<dyn Send + Sync>,
}

impl ConnectedUdpSession {
    pub fn new<T>(session: UdpSession, keep_alive: T) -> Self
    where
        T: Send + Sync + 'static,
    {
        Self {
            session,
            keep_alive: Box::new(keep_alive),
        }
    }

    pub fn session(&self) -> &UdpSession {
        &self.session
    }

    pub fn into_parts(self) -> (UdpSession, Box<dyn Send + Sync>) {
        (self.session, self.keep_alive)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UdpSessionMode {
    EasyTierMux,
    Classified(UdpSessionProtocol),
}

pub async fn connect_udp<H>(
    host: Arc<H>,
    remote_addr: SocketAddr,
    bind_addrs: Vec<SocketAddr>,
    default_bind: UdpBindOptions,
    mode: UdpSessionMode,
    network_name: Option<String>,
) -> anyhow::Result<ConnectedUdpSession>
where
    H: VirtualUdpSocketFactory,
{
    let ip_version = if remote_addr.is_ipv4() {
        IpVersion::V4
    } else {
        IpVersion::V6
    };
    let default_bind = default_bind.with_ip_version(ip_version);
    let futures = FuturesUnordered::new();
    if bind_addrs.is_empty() {
        let local_addr = if remote_addr.is_ipv4() {
            "0.0.0.0:0".parse().expect("static IPv4 bind address")
        } else {
            "[::]:0".parse().expect("static IPv6 bind address")
        };
        let bind = default_bind
            .with_local_addr(Some(local_addr))
            .with_only_v6(true);
        futures.push(bind_and_connect(host.clone(), bind, remote_addr, mode, network_name.clone()));
    } else {
        for bind_addr in bind_addrs {
            let bind = default_bind
                .clone()
                .with_local_addr(Some(bind_addr))
                .with_only_v6(true);
            futures.push(bind_and_connect(host.clone(), bind, remote_addr, mode, network_name.clone()));
        }
    }
    first_success(futures).await
}

async fn bind_and_connect<H>(
    host: Arc<H>,
    bind: UdpBindOptions,
    remote_addr: SocketAddr,
    mode: UdpSessionMode,
    network_name: Option<String>,
) -> anyhow::Result<ConnectedUdpSession>
where
    H: VirtualUdpSocketFactory,
{
    let socket = host.bind_udp(bind).await?;
    let layer = Arc::new(UdpSessionLayer::new_with_stun_responder(socket, host));
    // The layer is created per connection attempt, so the network name has to be
    // installed here before connecting. Without it the EasyTier mux handshake is
    // sent without the `NetworkAnnounce` prelude and a shared UDP listener (one
    // public port serving several networks) cannot demultiplex the flow.
    if let Some(network) = network_name.as_deref() {
        layer.set_network_name(network);
    }
    let session = match mode {
        UdpSessionMode::EasyTierMux => layer.connect(remote_addr).await?,
        UdpSessionMode::Classified(protocol) => {
            layer.open_classified_session(protocol, remote_addr)?
        }
    };
    Ok(ConnectedUdpSession::new(session, layer))
}
