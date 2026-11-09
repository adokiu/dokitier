//! Regression tests for the outbound UDP `NetworkAnnounce` prelude.
//!
//! A shared UDP listener (one public port serving several EasyTier networks,
//! see `core/src/shared_listener.rs`) can only demultiplex a client after it
//! announced its network name. The announce is a plain `UdpPacketType::
//! NetworkAnnounce` datagram that must be sent **before the first mux `Syn`**
//! and retransmitted together with every `Syn` retry (UDP can drop or reorder
//! the prelude).
//!
//! These tests drive the production connector entry point
//! [`connect_udp`](dokitier_core::connectivity::transport::connect_udp) with a
//! capture `VirtualUdpSocket`, so they cover the exact path used by:
//!
//! * a configured `[[peer]] uri = "udp://..."` (instance → `ManualConnectorManager`
//!   → `reconnect_with_ip_version` → `connect_resolved` → `connect_udp`),
//! * the direct connector's `connect_ordinary` UDP branch (same `connect_udp`).
//!
//! They also pin the ordering contract that the shared listener relies on.
//!
//! Run with:
//!
//! ```text
//! PROTOC=/Users/chuyao/Documents/Code/.protoc/bin/protoc \
//! CARGO_HOME=/Users/chuyao/Documents/Code/.cargo-home \
//! cargo test -p dokitier --test udp_announce_trace --offline
//! ```

use std::{
    io,
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::Duration,
};

use async_trait::async_trait;
use dokitier_core::{
    connectivity::transport::{UdpSessionMode, connect_udp},
    packet::{UDP_TUNNEL_HEADER_SIZE, UdpPacketType},
    socket::udp::{
        UdpBindOptions, UdpSocketRecvMeta, UdpSocketSendMeta, VirtualUdpSocket,
        VirtualUdpSocketFactory,
    },
};

const NETWORK: &str = "udp-announce-audit";
const REMOTE: &str = "203.0.113.7:11010";

/// `VirtualUdpSocket` that records every outbound datagram and never delivers
/// an inbound one (the connect attempt is expected to time out; the test only
/// inspects what the client put on the wire).
struct CaptureSocket {
    local: SocketAddr,
    sent: Mutex<Vec<Vec<u8>>>,
}

impl CaptureSocket {
    fn new() -> Self {
        Self {
            local: "127.0.0.1:45000".parse().expect("static capture addr"),
            sent: Mutex::new(Vec::new()),
        }
    }

    fn sent(&self) -> Vec<Vec<u8>> {
        self.sent.lock().expect("capture mutex").clone()
    }
}

#[async_trait]
impl VirtualUdpSocket for CaptureSocket {
    fn local_addr(&self) -> io::Result<SocketAddr> {
        Ok(self.local)
    }

    async fn send_to(&self, data: &[u8], _addr: SocketAddr) -> io::Result<usize> {
        self.sent.lock().expect("capture mutex").push(data.to_vec());
        Ok(data.len())
    }

    async fn recv_from(&self, _buf: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
        // A real connect would receive `Sack` here. Keep the recv half pending
        // so the layer's receive task stays quiet while the test observes the
        // handshake datagrams.
        std::future::pending().await
    }

    async fn send_to_with_meta(
        &self,
        data: &[u8],
        addr: SocketAddr,
        _meta: UdpSocketSendMeta,
    ) -> io::Result<usize> {
        self.send_to(data, addr).await
    }

    async fn recv_from_with_meta(
        &self,
        buf: &mut [u8],
    ) -> io::Result<(usize, SocketAddr, UdpSocketRecvMeta)> {
        let (len, addr) = self.recv_from(buf).await?;
        Ok((len, addr, UdpSocketRecvMeta::default()))
    }
}

struct CaptureFactory {
    socket: Arc<CaptureSocket>,
}

#[async_trait]
impl VirtualUdpSocketFactory for CaptureFactory {
    type Socket = CaptureSocket;

    async fn bind_udp(&self, _options: UdpBindOptions) -> anyhow::Result<Arc<Self::Socket>> {
        Ok(self.socket.clone())
    }
}

fn message_type(datagram: &[u8]) -> u8 {
    assert!(
        datagram.len() >= UDP_TUNNEL_HEADER_SIZE,
        "datagram shorter than the UDP tunnel header: {} bytes",
        datagram.len()
    );
    datagram[4]
}

fn payload(datagram: &[u8]) -> &[u8] {
    &datagram[UDP_TUNNEL_HEADER_SIZE..]
}

async fn wait_for_datagrams(socket: &CaptureSocket, minimum: usize) -> Vec<Vec<u8>> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        let datagrams = socket.sent();
        if datagrams.len() >= minimum {
            return datagrams;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "expected at least {minimum} datagrams, captured {datagrams:?}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn remote_addr() -> SocketAddr {
    REMOTE.parse().expect("static remote addr")
}

fn spawn_connect(
    socket: Arc<CaptureSocket>,
    network_name: Option<&'static str>,
) -> tokio::task::JoinHandle<()> {
    let host = Arc::new(CaptureFactory { socket });
    tokio::spawn(async move {
        // The mux handshake cannot complete without a `Sack`; the future is
        // aborted by the test after the datagrams are observed.
        let _ = connect_udp(
            host,
            remote_addr(),
            Vec::new(),
            UdpBindOptions::direct_connect(),
            UdpSessionMode::EasyTierMux,
            network_name.map(str::to_owned),
        )
        .await;
    })
}

/// A configured peer's `udp://` connection (and the direct connector's ordinary
/// UDP branch) must put `NetworkAnnounce` on the wire before the mux `Syn`.
#[tokio::test]
async fn configured_udp_connect_announces_network_before_syn() {
    let socket = Arc::new(CaptureSocket::new());
    let task = spawn_connect(socket.clone(), Some(NETWORK));

    let datagrams = wait_for_datagrams(&socket, 2).await;
    task.abort();

    assert_eq!(
        message_type(&datagrams[0]),
        UdpPacketType::NetworkAnnounce as u8,
        "first datagram must be NetworkAnnounce, got type {}",
        message_type(&datagrams[0])
    );
    assert_eq!(datagrams[0][0..4], [0, 0, 0, 0], "announce conn_id is zero");
    assert_eq!(
        payload(&datagrams[0]),
        NETWORK.as_bytes(),
        "announce payload must carry the instance network name"
    );
    assert_eq!(
        message_type(&datagrams[1]),
        UdpPacketType::Syn as u8,
        "the mux Syn must follow the announce"
    );
    assert_eq!(
        payload(&datagrams[1]).len(),
        8,
        "Syn carries the 8-byte magic"
    );
}

/// UDP may drop or reorder the announce prelude, so the layer must resend it
/// with every Syn retry (200 ms retry interval today).
#[tokio::test]
async fn announce_is_retransmitted_with_syn_retries() {
    let socket = Arc::new(CaptureSocket::new());
    let task = spawn_connect(socket.clone(), Some(NETWORK));

    // 700 ms covers the initial pair plus at least two 200 ms retransmissions.
    let datagrams = wait_for_datagrams(&socket, 6).await;
    task.abort();

    let announces = datagrams
        .iter()
        .filter(|datagram| message_type(datagram) == UdpPacketType::NetworkAnnounce as u8)
        .count();
    assert!(
        announces >= 2,
        "expected the announce to be retransmitted, captured types: {:?}",
        datagrams.iter().map(|d| message_type(d)).collect::<Vec<_>>()
    );
    assert_eq!(
        message_type(&datagrams[0]),
        UdpPacketType::NetworkAnnounce as u8
    );
    assert_eq!(message_type(&datagrams[1]), UdpPacketType::Syn as u8);
}

/// Connectors that cannot name a network (raw standalone UDP tunnel) must not
/// invent an announce: only the mux Syn is sent.
#[tokio::test]
async fn connect_without_network_name_sends_syn_only() {
    let socket = Arc::new(CaptureSocket::new());
    let task = spawn_connect(socket.clone(), None);

    let datagrams = wait_for_datagrams(&socket, 1).await;
    task.abort();

    let types: Vec<u8> = datagrams.iter().map(|d| message_type(d)).collect();
    assert!(
        types.iter()
            .all(|msg_type| *msg_type != UdpPacketType::NetworkAnnounce as u8),
        "no announce expected without a network name, captured types: {types:?}"
    );
    assert_eq!(types[0], UdpPacketType::Syn as u8);
}
