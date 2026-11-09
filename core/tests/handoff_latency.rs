//! Hop-cost measurement + correctness harness for
//! [`core/src/shared_listener_handoff.rs`](../src/shared_listener_handoff.rs).
//!
//! # What this measures
//!
//! The legacy shared listeners accept on the public port, sniff the first
//! EasyTier frame, then dial a *second* loopback listener owned by the instance
//! and relay bytes with `copy_bidirectional`. The direct handoff path gives the
//! accepted stream to the instance's `PeerManagerCore` and removes that second
//! hop. Both paths then run exactly the same instance tunnel (`TcpTunnel` +
//! `PeerConn` framing), so a fair comparison is:
//!
//! ```text
//! direct  : client ──TcpStream──> handoff::tcp_tunnel_from_accepted ──> echo tunnel
//! loopback: client ──TcpStream──> public accept ──connect 127.0.0.1──> echo tunnel
//! ```
//!
//! The echo tunnel is built from the very same handoff helper, so the only
//! difference is the extra loopback proxy hop of the legacy design.
//!
//! # Tests
//!
//! * [`peek_does_not_consume_first_frame`] — proves the core trick: the first
//!   EasyTier handshake frame can be sniffed with `TcpStream::peek` and is
//!   still byte-for-byte readable by the instance afterwards.
//! * [`accepted_stream_becomes_echoing_tunnel`] — proves the direct tunnel
//!   wrapper speaks the real `TcpTunnel` framing.
//! * [`latency_direct_handoff_vs_loopback_proxy`] — **ignored benchmark**,
//!   per-message RTT percentiles for both paths.
//! * [`throughput_direct_handoff_vs_loopback_proxy`] — **ignored benchmark**,
//!   streaming MB/s for both paths.
//!
//! # Running
//!
//! ```text
//! export PROTOC=/Users/chuyao/Documents/Code/.protoc/bin/protoc
//! export CARGO_HOME=/Users/chuyao/Documents/Code/.cargo-home
//! cargo test -p dokitier --test handoff_latency --offline
//! cargo test -p dokitier --test handoff_latency --offline -- --ignored --nocapture
//! ```
//!
//! The benchmarks are ignored by default because wall-clock assertions are
//! flaky on shared CI machines; run them manually and read the report.
//!
//! # Why this file includes the source module
//!
//! The task that introduced the handoff module was only allowed to add files,
//! so `core/src/lib.rs` does not declare `pub mod shared_listener_handoff;`
//! yet. Including the source with `#[path]` compiles exactly the same code in
//! this test crate and keeps the compile check `cargo check`/`cargo test`
//! meaningful until the Lead wires the one-line module declaration.

#[path = "../src/shared_listener_handoff.rs"]
mod handoff;

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use dokitier_core::packet::{
    PacketType, ZCPacket, ZCPacketType, PEER_MANAGER_HEADER_SIZE, TCP_TUNNEL_HEADER_SIZE,
};
use dokitier_core::tunnel::framed::{FramedReader, FramedWriter, TCP_MTU_BYTES};
use dokitier_proto::peer_rpc::HandshakeRequest;
use futures::{SinkExt, StreamExt};
use prost::Message as _;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use url::Url;

const LOCAL_URL: &str = "tcp://0.0.0.0:11010";

/// Builds the first EasyTier frame exactly like a real peer would:
/// `TCPTunnelHeader(4 B) | PeerManagerHeader(14 B) | HandshakeRequest`.
fn first_handshake_frame(network: &str) -> Vec<u8> {
    let handshake = HandshakeRequest {
        magic: 0xd1e1_a5e1,
        my_peer_id: 1,
        version: 2,
        features: Vec::new(),
        network_name: network.to_owned(),
        network_secret_digest: vec![0u8; 32],
        p2p_group: String::new(),
    }
    .encode_to_vec();

    let mut packet = ZCPacket::new_with_payload(&handshake);
    packet.fill_peer_manager_hdr(1, 0, PacketType::HandShake as u8);
    let mut packet = packet.convert_type(ZCPacketType::TCP);
    let body_len = PEER_MANAGER_HEADER_SIZE + packet.payload_len();
    packet
        .mut_tcp_tunnel_header()
        .expect("TCP tunnel header")
        .len
        .set(body_len as u32);
    packet.into_bytes().to_vec()
}

fn data_packet(payload: &[u8]) -> ZCPacket {
    let mut packet = ZCPacket::new_with_payload(payload);
    packet.fill_peer_manager_hdr(1, 2, PacketType::Data as u8);
    packet
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

/// Accepts connections and serves them through the **direct handoff** tunnel
/// helper, echoing every framed packet.
async fn spawn_direct_echo_server() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind echo");
    let addr = listener.local_addr().expect("echo addr");
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else { break };
            tokio::spawn(async move {
                let local_url = Url::parse(LOCAL_URL).expect("static local url");
                let Ok(tunnel) = handoff::tcp_tunnel_from_accepted(stream, local_url) else {
                    return;
                };
                let (reader, writer) = tunnel.split();
                echo_tunnel(reader, writer).await;
            });
        }
    });
    addr
}

/// Reads one EasyTier frame from a raw stream; the legacy relay does the same
/// (`core/src/shared_listener.rs::read_first_frame`) before dialing loopback.
async fn read_first_frame_raw(stream: &mut TcpStream) -> Option<Vec<u8>> {
    let mut header = [0u8; TCP_TUNNEL_HEADER_SIZE];
    tokio::time::timeout(Duration::from_secs(3), stream.read_exact(&mut header))
        .await
        .ok()?
        .ok()?;
    let body_len = u32::from_le_bytes(header) as usize;
    if !(PEER_MANAGER_HEADER_SIZE..=handoff::MAX_FIRST_FRAME).contains(&body_len) {
        return None;
    }
    let mut body = vec![0u8; body_len];
    tokio::time::timeout(Duration::from_secs(3), stream.read_exact(&mut body))
        .await
        .ok()?
        .ok()?;
    let mut frame = Vec::with_capacity(TCP_TUNNEL_HEADER_SIZE + body_len);
    frame.extend_from_slice(&header);
    frame.extend_from_slice(&body);
    Some(frame)
}

/// Models the legacy `shared_listener` hop: accept, sniff/forward the first
/// frame, dial the instance's loopback listener, then relay bytes both ways.
async fn spawn_loopback_proxy(upstream: SocketAddr) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind proxy");
    let addr = listener.local_addr().expect("proxy addr");
    tokio::spawn(async move {
        loop {
            let Ok((mut client, _)) = listener.accept().await else { break };
            tokio::spawn(async move {
                let Some(frame) = read_first_frame_raw(&mut client).await else {
                    return;
                };
                let Ok(mut upstream) = TcpStream::connect(upstream).await else {
                    return;
                };
                if upstream.write_all(&frame).await.is_err() {
                    return;
                }
                let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
            });
        }
    });
    addr
}

/// Per-message RTT samples (µs) through a persistent framed connection.
async fn measure_rtt(addr: SocketAddr, iterations: usize) -> Vec<u128> {
    let stream = TcpStream::connect(addr).await.expect("connect");
    stream.set_nodelay(true).expect("nodelay");
    let (read, write) = stream.into_split();
    let mut reader = FramedReader::new(read, TCP_MTU_BYTES);
    let mut writer = FramedWriter::new(write);

    let mut samples = Vec::with_capacity(iterations);
    for sequence in 0..iterations {
        let payload = vec![sequence as u8; 64];
        let started = Instant::now();
        writer
            .send(data_packet(&payload))
            .await
            .expect("send ping");
        let echoed = tokio::time::timeout(Duration::from_secs(5), reader.next())
            .await
            .expect("echo timeout")
            .expect("stream ended")
            .expect("tunnel error");
        samples.push(started.elapsed().as_micros());
        assert_eq!(echoed.payload(), &payload[..], "echo payload mismatch");
    }
    samples
}

/// Streaming throughput (MiB/s): send `packets` frames of `payload` bytes while
/// draining the echoes concurrently.
async fn measure_throughput(addr: SocketAddr, packets: usize, payload: usize) -> f64 {
    let stream = TcpStream::connect(addr).await.expect("connect");
    stream.set_nodelay(true).expect("nodelay");
    let (read, write) = stream.into_split();
    let mut reader = FramedReader::new(read, TCP_MTU_BYTES);
    let mut writer = FramedWriter::new(write);
    let started = Instant::now();

    let sender = async move {
        for sequence in 0..packets {
            if writer
                .send(data_packet(&vec![sequence as u8; payload]))
                .await
                .is_err()
            {
                break;
            }
        }
    };
    let receiver = async move {
        for _ in 0..packets {
            match tokio::time::timeout(Duration::from_secs(10), reader.next()).await {
                Ok(Some(Ok(_))) => {}
                _ => break,
            }
        }
    };
    tokio::join!(sender, receiver);

    let seconds = started.elapsed().as_secs_f64();
    (packets * payload) as f64 / seconds / (1024.0 * 1024.0)
}

fn percentile(samples: &mut [u128], percent: f64) -> u128 {
    samples.sort_unstable();
    let index = ((samples.len() - 1) as f64 * percent).round() as usize;
    samples[index]
}

#[tokio::test]
async fn peek_does_not_consume_first_frame() {
    let frame = first_handshake_frame("probe-net");
    let frame_len = frame.len();

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let client = tokio::spawn(async move {
        let mut stream = TcpStream::connect(addr).await.expect("connect");
        stream.write_all(&frame).await.expect("write frame");
        stream.flush().await.expect("flush");
        tokio::time::sleep(Duration::from_millis(300)).await;
        stream
    });

    let (mut server, _) = listener.accept().await.expect("accept");

    // 1. The helper must decode the network name from the peeked bytes.
    let network = handoff::peek_handshake_network(&server)
        .await
        .expect("peek must not fail");
    assert_eq!(network.as_deref(), Some("probe-net"));

    // A second peek must be idempotent (nothing was consumed).
    let network_again = handoff::peek_handshake_network(&server)
        .await
        .expect("peek must not fail");
    assert_eq!(network_again.as_deref(), Some("probe-net"));

    // 2. The frame must still be byte-for-byte readable afterwards; this is what
    //    the instance's PeerConn handshake will read.
    let mut read_back = vec![0u8; frame_len];
    tokio::time::timeout(Duration::from_secs(2), server.read_exact(&mut read_back))
        .await
        .expect("read back timeout")
        .expect("read back");
    assert_eq!(
        read_back,
        first_handshake_frame("probe-net"),
        "peek must not consume the handshake frame"
    );

    drop(client.await);
}

#[tokio::test]
async fn accepted_stream_becomes_echoing_tunnel() {
    let server = spawn_direct_echo_server().await;
    let stream = TcpStream::connect(server).await.expect("connect");
    stream.set_nodelay(true).expect("nodelay");
    let (read, write) = stream.into_split();
    let mut reader = FramedReader::new(read, TCP_MTU_BYTES);
    let mut writer = FramedWriter::new(write);

    for sequence in 0u8..16 {
        let payload = vec![sequence; 32];
        writer
            .send(data_packet(&payload))
            .await
            .expect("send packet");
        let echoed = tokio::time::timeout(Duration::from_secs(5), reader.next())
            .await
            .expect("echo timeout")
            .expect("stream ended")
            .expect("tunnel error");
        assert_eq!(echoed.payload(), &payload[..]);
    }
}

#[test]
fn unknown_network_has_no_handoff_target() {
    assert!(handoff::resolve_network("__no_such_network__").is_none());
    assert!(!handoff::udp_direct_handoff_supported());
    assert!(!handoff::REQUIRED_UDP_HANDOFF_APIS.is_empty());
}

#[tokio::test]
async fn malformed_first_frame_is_not_routed() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let client = tokio::spawn(async move {
        let mut stream = TcpStream::connect(addr).await.expect("connect");
        // 4-byte length that is below the peer-manager header size.
        stream.write_all(&3u32.to_le_bytes()).await.expect("write");
        stream.flush().await.expect("flush");
        tokio::time::sleep(Duration::from_millis(200)).await;
    });

    let (server, _) = listener.accept().await.expect("accept");
    let network = handoff::peek_handshake_network(&server)
        .await
        .expect("peek must not fail");
    assert_eq!(network, None);
    client.await.expect("client task");
}

/// Ignored latency benchmark: per-message RTT with the direct handoff path vs
/// the legacy loopback relay.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "wall-clock benchmark; run with `-- --ignored --nocapture`"]
async fn latency_direct_handoff_vs_loopback_proxy() {
    let direct = spawn_direct_echo_server().await;
    let proxied = spawn_loopback_proxy(direct).await;

    // Warm up both paths, then interleave the measured runs to reduce drift.
    let _ = measure_rtt(direct, 100).await;
    let _ = measure_rtt(proxied, 100).await;
    let mut direct_samples = measure_rtt(direct, 1000).await;
    let mut proxied_samples = measure_rtt(proxied, 1000).await;

    let direct_p50 = percentile(&mut direct_samples, 0.50);
    let direct_p90 = percentile(&mut direct_samples, 0.90);
    let direct_p99 = percentile(&mut direct_samples, 0.99);
    let proxied_p50 = percentile(&mut proxied_samples, 0.50);
    let proxied_p90 = percentile(&mut proxied_samples, 0.90);
    let proxied_p99 = percentile(&mut proxied_samples, 0.99);

    eprintln!("--- per-message RTT (64 B payload, 1000 samples) ---");
    eprintln!(
        "direct handoff : p50={direct_p50}us p90={direct_p90}us p99={direct_p99}us"
    );
    eprintln!(
        "loopback relay : p50={proxied_p50}us p90={proxied_p90}us p99={proxied_p99}us"
    );
    if direct_p50 > 0 {
        eprintln!(
            "hop cost       : +{}us p50 (+{:.1}%), +{}us p99",
            proxied_p50.saturating_sub(direct_p50),
            (proxied_p50 as f64 / direct_p50 as f64 - 1.0) * 100.0,
            proxied_p99.saturating_sub(direct_p99),
        );
    }
    eprintln!(
        "note: one extra loopback hop costs at least one connect() per client and \
         two extra user copies + two task wakeups per relayed chunk; QUIC additionally \
         pays a second full QUIC handshake and double crypto, so its win is larger."
    );

    // Soft sanity check only: on a loopback pair the relay must not be faster.
    if proxied_p50 < direct_p50 {
        eprintln!("warning: loopback relay measured faster than direct handoff; rerun");
    }
}

/// Ignored throughput benchmark: streaming MiB/s with the direct handoff path
/// vs the legacy loopback relay.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "wall-clock benchmark; run with `-- --ignored --nocapture`"]
async fn throughput_direct_handoff_vs_loopback_proxy() {
    let direct = spawn_direct_echo_server().await;
    let proxied = spawn_loopback_proxy(direct).await;

    let direct_mib = measure_throughput(direct, 2048, 16 * 1024).await;
    let proxied_mib = measure_throughput(proxied, 2048, 16 * 1024).await;

    eprintln!("--- streaming throughput (2048 x 16 KiB frames, echoed) ---");
    eprintln!("direct handoff : {direct_mib:.1} MiB/s");
    eprintln!("loopback relay : {proxied_mib:.1} MiB/s");
    if direct_mib > 0.0 {
        eprintln!(
            "throughput     : {:+.1}% for the direct path",
            (direct_mib / proxied_mib - 1.0) * 100.0
        );
    }
}
