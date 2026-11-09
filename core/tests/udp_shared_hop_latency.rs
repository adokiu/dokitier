//! UDP shared-listener single-hop hop-cost probe.
//!
//! The shared UDP listener owns one public `UdpSocket`; a dispatcher task reads
//! datagrams, records the client's `NetworkAnnounce`, and forwards every
//! datagram of an announced client into a channel-backed
//! [`VirtualUdpSocket`], behind which the instance's normal
//! [`UdpSessionLayer`] accepts the EasyTier mux session and hands it to the
//! peer manager. That is the single-hop design: no per-session loopback
//! `UdpSocket`, no relay task duplicating the mux.
//!
//! This probe measures that extra dispatcher hop against the plain EasyTier UDP
//! listener baseline (one `UdpSessionLayer` directly on its own socket — what an
//! upstream EasyTier peer would use) and against a raw UDP echo ceiling, using
//! the same echo-tunnel idea as `core/tests/handoff_latency.rs`.
//!
//! ```text
//! raw UDP      : client UdpSocket  <-> echo UdpSocket
//! baseline     : client layer.connect(server)  <-> layer.accept -> UdpTunnel -> echo
//! shared port  : client layer(announce).connect(shared) -> dispatcher -> channel
//!                                                   -> SharedUdpSocket -> layer.accept
//!                                                   -> UdpTunnel -> echo
//! ```
//!
//! Both tunnel paths use `UdpTunnelUpgrader`, i.e. exactly the data plane the
//! real peer manager uses; the only difference is the dispatcher hop.
//!
//! # Running
//!
//! ```text
//! export PROTOC=/Users/chuyao/Documents/Code/.protoc/bin/protoc
//! export CARGO_HOME=/Users/chuyao/Documents/Code/.cargo-home
//! cargo test -p dokitier --test udp_shared_hop_latency --offline
//! cargo test -p dokitier --test udp_shared_hop_latency --offline -- --ignored --nocapture
//! ```
//!
//! Benchmarks are `#[ignore]`d because wall-clock assertions are flaky in CI;
//! run them manually and read the printed report.

use std::collections::HashMap;
use std::io;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::BytesMut;
use dokitier_core::packet::{PacketType, UdpPacketType, ZCPacket, ZCPacketType};
use dokitier_core::socket::udp::{
    UdpSessionLayer, UdpSocketRecvMeta, UdpSocketSendMeta, VirtualUdpSocket,
};
use dokitier_core::tunnel::Tunnel;
use dokitier_core::tunnel::udp::UdpTunnelUpgrader;
use futures::{SinkExt, StreamExt};
use tokio::net::UdpSocket;
use url::Url;

const NETWORK: &str = "udp-probe";

// ---------------------------------------------------------------------------
// Minimal public adapters over tokio sockets
// ---------------------------------------------------------------------------

/// `VirtualUdpSocket` over a plain tokio socket, for the baseline path.
struct TokioUdpSocket {
    socket: UdpSocket,
}

#[async_trait::async_trait]
impl VirtualUdpSocket for TokioUdpSocket {
    fn local_addr(&self) -> io::Result<SocketAddr> {
        self.socket.local_addr()
    }

    async fn send_to(&self, data: &[u8], addr: SocketAddr) -> io::Result<usize> {
        self.socket.send_to(data, addr).await
    }

    async fn recv_from(&self, buf: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
        // Leave room for the peer-manager header, like the real session layer.
        self.socket.recv_from(buf).await
    }
}

/// One network's channel-fed view of the shared public socket; mirrors
/// `core/src/shared_listener.rs::SharedUdpSocket` without touching that file.
struct SharedUdpSocket {
    local_addr: SocketAddr,
    sender: Arc<UdpSocket>,
    inbound: tokio::sync::Mutex<tokio::sync::mpsc::Receiver<(Vec<u8>, SocketAddr)>>,
}

#[async_trait::async_trait]
impl VirtualUdpSocket for SharedUdpSocket {
    fn local_addr(&self) -> io::Result<SocketAddr> {
        Ok(self.local_addr)
    }

    async fn send_to(&self, data: &[u8], addr: SocketAddr) -> io::Result<usize> {
        self.sender.send_to(data, addr).await
    }

    async fn recv_from(&self, buf: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
        let mut inbound = self.inbound.lock().await;
        let (data, addr) = inbound.recv().await.ok_or_else(|| {
            io::Error::new(io::ErrorKind::BrokenPipe, "shared udp channel closed")
        })?;
        let len = data.len().min(buf.len());
        buf[..len].copy_from_slice(&data[..len]);
        Ok((len, addr))
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

// ---------------------------------------------------------------------------
// Shared-path dispatcher (replica of the production one)
// ---------------------------------------------------------------------------

fn udp_msg_type(datagram: &[u8]) -> Option<(u8, Vec<u8>)> {
    let packet = ZCPacket::new_from_buf(BytesMut::from(datagram), ZCPacketType::UDP);
    let msg_type = packet.udp_tunnel_header()?.msg_type;
    Some((msg_type, packet.udp_payload().to_vec()))
}

/// Binds one public socket, starts the announce-aware dispatcher and the
/// channel-fed session layer, and returns the public address.
async fn spawn_shared_udp_echo() -> SocketAddr {
    let socket = Arc::new(UdpSocket::bind("127.0.0.1:0").await.expect("bind shared"));
    let public_addr = socket.local_addr().expect("shared addr");
    let (tx, rx) = tokio::sync::mpsc::channel(1024);
    let shared_socket = Arc::new(SharedUdpSocket {
        local_addr: public_addr,
        sender: socket.clone(),
        inbound: tokio::sync::Mutex::new(rx),
    });
    let layer = Arc::new(UdpSessionLayer::new(shared_socket));

    // Instance side: accept one mux session and echo framed packets through a
    // normal UdpTunnel, exactly like `ensure_shared_udp_network` does.
    {
        let layer = layer.clone();
        tokio::spawn(async move {
            loop {
                let Ok(session) = layer.accept().await else { break };
                let layer = layer.clone();
                tokio::spawn(async move {
                    let peer = session.peer_socket_addr();
                    let info = tunnel_info(public_addr, peer);
                    let Ok(tunnel) =
                        UdpTunnelUpgrader::with_keep_alive(info, layer).upgrade(session)
                    else {
                        return;
                    };
                    let (reader, writer) = tunnel.split();
                    echo_tunnel(reader, writer).await;
                });
            }
        });
    }

    // Public side: announce-aware dispatcher; single-network fallback matches
    // the production listener.
    {
        let socket = socket.clone();
        tokio::spawn(async move {
            let mut buf = vec![0u8; 65535];
            let announces: Arc<Mutex<HashMap<SocketAddr, String>>> =
                Arc::new(Mutex::new(HashMap::new()));
            loop {
                let Ok((len, client)) = socket.recv_from(&mut buf).await else {
                    continue;
                };
                let datagram = &buf[..len];
                let Some((msg_type, payload)) = udp_msg_type(datagram) else {
                    continue;
                };
                if msg_type == UdpPacketType::NetworkAnnounce as u8 {
                    if let Ok(network) = std::str::from_utf8(&payload) {
                        if !network.is_empty() {
                            announces.lock().unwrap().insert(client, network.to_string());
                        }
                    }
                    continue;
                }
                let known = announces.contains_key(&client);
                if !known {
                    // Single-network unambiguity fallback (production listener).
                    announces.lock().unwrap().insert(client, NETWORK.to_string());
                }
                let _ = tx.try_send((datagram.to_vec(), client));
            }
        });
    }

    public_addr
}

/// Baseline: one `UdpSessionLayer` directly on its own socket, like an upstream
/// EasyTier UDP listener.
async fn spawn_baseline_udp_echo() -> SocketAddr {
    let socket = Arc::new(UdpSocket::bind("127.0.0.1:0").await.expect("bind base"));
    let addr = socket.local_addr().expect("base addr");
    let layer = Arc::new(UdpSessionLayer::new(Arc::new(TokioUdpSocket { socket })));
    tokio::spawn(async move {
        loop {
            let Ok(session) = layer.accept().await else { break };
            let peer = session.peer_socket_addr();
            let info = tunnel_info(addr, peer);
            let Ok(tunnel) = UdpTunnelUpgrader::with_keep_alive(info, layer.clone()).upgrade(session)
            else {
                continue;
            };
            let (reader, writer) = tunnel.split();
            tokio::spawn(async move { echo_tunnel(reader, writer).await });
        }
    });
    addr
}

/// Raw datagram echo ceiling.
async fn spawn_raw_udp_echo() -> SocketAddr {
    let socket = UdpSocket::bind("127.0.0.1:0").await.expect("bind raw");
    let addr = socket.local_addr().expect("raw addr");
    tokio::spawn(async move {
        let mut buf = vec![0u8; 65535];
        loop {
            let Ok((len, peer)) = socket.recv_from(&mut buf).await else {
                break;
            };
            let _ = socket.send_to(&buf[..len], peer).await;
        }
    });
    addr
}

fn tunnel_info(local: SocketAddr, peer: SocketAddr) -> dokitier_proto::common::TunnelInfo {
    let local_url: Url = format!("udp://{local}").parse().expect("local url");
    let peer_url: Url = format!("udp://{peer}").parse().expect("peer url");
    dokitier_proto::common::TunnelInfo {
        tunnel_type: "udp".to_owned(),
        local_addr: Some(local_url.into()),
        remote_addr: Some(peer_url.clone().into()),
        resolved_remote_addr: Some(peer_url.into()),
    }
}

async fn echo_tunnel(
    mut reader: std::pin::Pin<Box<dyn dokitier_core::tunnel::ZCPacketStream>>,
    mut writer: std::pin::Pin<Box<dyn dokitier_core::tunnel::ZCPacketSink>>,
) {
    while let Some(item) = reader.next().await {
        let Ok(packet) = item else { break };
        if writer.send(packet).await.is_err() {
            break;
        }
    }
}

// ---------------------------------------------------------------------------
// Clients
// ---------------------------------------------------------------------------

async fn connect_udp_tunnel(remote: SocketAddr, announce: Option<&str>) -> Box<dyn Tunnel> {
    let socket = Arc::new(TokioUdpSocket {
        socket: UdpSocket::bind("127.0.0.1:0").await.expect("bind client"),
    });
    let layer = Arc::new(UdpSessionLayer::new(socket));
    if let Some(network) = announce {
        layer.set_network_name(network);
    }
    let session = layer
        .connect(remote)
        .await
        .unwrap_or_else(|err| panic!("udp connect to {remote} failed: {err}"));
    let local = session.local_socket_addr();
    let peer = session.peer_socket_addr();
    let info = tunnel_info(local, peer);
    UdpTunnelUpgrader::with_keep_alive(info, layer)
        .upgrade(session)
        .expect("upgrade connected udp tunnel")
}

fn data_packet(sequence: u64, payload_len: usize) -> ZCPacket {
    let mut payload = vec![sequence as u8; payload_len.max(8)];
    payload[..8].copy_from_slice(&sequence.to_le_bytes());
    let mut packet = ZCPacket::new_with_payload(&payload);
    packet.fill_peer_manager_hdr(1, 2, PacketType::Data as u8);
    packet
}

fn packet_sequence(packet: &ZCPacket) -> u64 {
    let payload = packet.payload();
    if payload.len() < 8 {
        return u64::MAX;
    }
    let mut seq = [0u8; 8];
    seq.copy_from_slice(&payload[..8]);
    u64::from_le_bytes(seq)
}

// ---------------------------------------------------------------------------
// Measurements
// ---------------------------------------------------------------------------

async fn measure_tunnel_rtt(tunnel: Box<dyn Tunnel>, iterations: usize) -> Vec<u128> {
    let (mut reader, mut writer) = tunnel.split();
    let mut samples = Vec::with_capacity(iterations);
    for sequence in 0..iterations {
        let started = Instant::now();
        writer
            .send(data_packet(sequence as u64, 64))
            .await
            .expect("send ping");
        let echoed = tokio::time::timeout(Duration::from_secs(5), reader.next())
            .await
            .expect("echo timeout")
            .expect("stream ended")
            .expect("tunnel error");
        samples.push(started.elapsed().as_micros());
        assert_eq!(packet_sequence(&echoed), sequence as u64, "echo mismatch");
    }
    samples
}

async fn measure_tunnel_throughput(
    tunnel: Box<dyn Tunnel>,
    packets: usize,
    payload_len: usize,
    window: usize,
) -> f64 {
    let (mut reader, mut writer) = tunnel.split();
    let started = Instant::now();
    let mut next = 0usize;
    let mut inflight = 0usize;
    let mut received = 0usize;
    while received < packets {
        while inflight < window && next < packets {
            writer
                .send(data_packet(next as u64, payload_len))
                .await
                .expect("send data");
            next += 1;
            inflight += 1;
        }
        let packet = tokio::time::timeout(Duration::from_secs(10), reader.next())
            .await
            .expect("echo timeout")
            .expect("stream ended")
            .expect("tunnel error");
        assert_eq!(packet_sequence(&packet), received as u64, "echo order");
        received += 1;
        inflight -= 1;
    }
    secs_to_mib_per_s(packets * payload_len, started.elapsed())
}

async fn measure_raw_udp_rtt(echo: SocketAddr, iterations: usize) -> Vec<u128> {
    let socket = UdpSocket::bind("127.0.0.1:0").await.expect("bind raw client");
    let mut buf = vec![0u8; 65535];
    let mut samples = Vec::with_capacity(iterations);
    for sequence in 0..iterations {
        let payload = (sequence as u64).to_le_bytes();
        let started = Instant::now();
        socket.send_to(&payload, echo).await.expect("raw send");
        let (len, _) = tokio::time::timeout(Duration::from_secs(5), socket.recv_from(&mut buf))
            .await
            .expect("raw echo timeout")
            .expect("raw recv");
        assert_eq!(len, payload.len());
        samples.push(started.elapsed().as_micros());
    }
    samples
}

async fn measure_raw_udp_throughput(
    echo: SocketAddr,
    packets: usize,
    payload_len: usize,
    window: usize,
) -> f64 {
    let socket = UdpSocket::bind("127.0.0.1:0").await.expect("bind raw client");
    let started = Instant::now();
    let mut buf = vec![0u8; 65535];
    let mut next = 0usize;
    let mut inflight = 0usize;
    let mut received = 0usize;
    while received < packets {
        while inflight < window && next < packets {
            let payload = vec![next as u8; payload_len];
            socket.send_to(&payload, echo).await.expect("raw send");
            next += 1;
            inflight += 1;
        }
        let (len, _) = tokio::time::timeout(Duration::from_secs(10), socket.recv_from(&mut buf))
            .await
            .expect("raw echo timeout")
            .expect("raw recv");
        assert_eq!(len, payload_len);
        received += 1;
        inflight -= 1;
    }
    secs_to_mib_per_s(packets * payload_len, started.elapsed())
}

fn secs_to_mib_per_s(bytes: usize, elapsed: Duration) -> f64 {
    bytes as f64 / elapsed.as_secs_f64() / (1024.0 * 1024.0)
}

fn percentile(samples: &mut [u128], percent: f64) -> u128 {
    samples.sort_unstable();
    samples[((samples.len() - 1) as f64 * percent).round() as usize]
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shared_udp_path_echoes_framed_packets() {
    let shared = spawn_shared_udp_echo().await;
    let tunnel = connect_udp_tunnel(shared, Some(NETWORK)).await;
    let (mut reader, mut writer) = tunnel.split();
    for sequence in 0..16u64 {
        writer
            .send(data_packet(sequence, 64))
            .await
            .expect("send");
        let echoed = tokio::time::timeout(Duration::from_secs(5), reader.next())
            .await
            .expect("echo timeout")
            .expect("stream ended")
            .expect("tunnel error");
        assert_eq!(packet_sequence(&echoed), sequence);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "wall-clock benchmark; run with `-- --ignored --nocapture`"]
async fn latency_udp_shared_vs_direct_baseline() {
    let raw = spawn_raw_udp_echo().await;
    let baseline = spawn_baseline_udp_echo().await;
    let shared = spawn_shared_udp_echo().await;

    let _ = measure_raw_udp_rtt(raw, 100).await;
    let baseline_tunnel = connect_udp_tunnel(baseline, None).await;
    let _ = measure_tunnel_rtt(baseline_tunnel, 100).await;
    let shared_tunnel = connect_udp_tunnel(shared, Some(NETWORK)).await;
    let _ = measure_tunnel_rtt(shared_tunnel, 100).await;

    let mut raw_samples = measure_raw_udp_rtt(raw, 1000).await;
    let baseline_tunnel = connect_udp_tunnel(baseline, None).await;
    let mut baseline_samples = measure_tunnel_rtt(baseline_tunnel, 1000).await;
    let shared_tunnel = connect_udp_tunnel(shared, Some(NETWORK)).await;
    let mut shared_samples = measure_tunnel_rtt(shared_tunnel, 1000).await;

    let raw_p50 = percentile(&mut raw_samples, 0.50);
    let raw_p99 = percentile(&mut raw_samples, 0.99);
    let base_p50 = percentile(&mut baseline_samples, 0.50);
    let base_p90 = percentile(&mut baseline_samples, 0.90);
    let base_p99 = percentile(&mut baseline_samples, 0.99);
    let shared_p50 = percentile(&mut shared_samples, 0.50);
    let shared_p90 = percentile(&mut shared_samples, 0.90);
    let shared_p99 = percentile(&mut shared_samples, 0.99);

    eprintln!("--- UDP per-message RTT (64 B payload, 1000 samples, debug build) ---");
    eprintln!("raw udp echo       : p50={raw_p50}us p99={raw_p99}us");
    eprintln!("direct layer+tunnel: p50={base_p50}us p90={base_p90}us p99={base_p99}us");
    eprintln!("shared single-hop  : p50={shared_p50}us p90={shared_p90}us p99={shared_p99}us");
    eprintln!(
        "dispatcher hop     : +{}us p50, +{}us p99 vs direct baseline",
        shared_p50.saturating_sub(base_p50),
        shared_p99.saturating_sub(base_p99),
    );
    if shared_p50 > baseline_p50 + 500 {
        eprintln!("warning: shared hop cost looks unexpectedly high; rerun or check load");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "wall-clock benchmark; run with `-- --ignored --nocapture`"]
async fn throughput_udp_shared_vs_direct_baseline() {
    const PACKETS: usize = 2048;
    const PAYLOAD: usize = 8 * 1024;
    const WINDOW: usize = 16;

    let raw = spawn_raw_udp_echo().await;
    let baseline = spawn_baseline_udp_echo().await;
    let shared = spawn_shared_udp_echo().await;

    let raw_mib = measure_raw_udp_throughput(raw, PACKETS, PAYLOAD, WINDOW).await;
    let baseline_tunnel = connect_udp_tunnel(baseline, None).await;
    let baseline_mib =
        measure_tunnel_throughput(baseline_tunnel, PACKETS, PAYLOAD, WINDOW).await;
    let shared_tunnel = connect_udp_tunnel(shared, Some(NETWORK)).await;
    let shared_mib = measure_tunnel_throughput(shared_tunnel, PACKETS, PAYLOAD, WINDOW).await;

    eprintln!("--- UDP streaming throughput ({PACKETS} x {} KiB, window {WINDOW}) ---", PAYLOAD / 1024);
    eprintln!("raw udp echo       : {raw_mib:.1} MiB/s");
    eprintln!("direct layer+tunnel: {baseline_mib:.1} MiB/s");
    eprintln!("shared single-hop  : {shared_mib:.1} MiB/s");
    if baseline_mib > 0.0 {
        eprintln!(
            "shared vs baseline : {:+.1}%",
            (shared_mib / baseline_mib - 1.0) * 100.0
        );
    }
}
