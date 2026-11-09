//! Black-box probe for the UDP side of `core/src/shared_listener.rs`.
//!
//! The module under test owns a public UDP port shared by every network of one
//! core process, plus a loopback UDP listener per network (the ports are
//! allocated by `shared_listener::rewrite_listeners`). This test only uses
//! public API:
//!
//! * `dokitier::shared_listener::{start, rewrite_listeners, addr_list}`
//! * `dokitier_core` packet types (the exact wire format real peers use)
//! * `dokitier_proto` `HandshakeRequest` (prost-encoded)
//!
//! It binds a fake "internal listener" on the loopback port reserved for a
//! network, then drives the shared port like a real EasyTier peer would:
//! first an EasyTier UDP mux SYN (`new_syn_packet`, see
//! `UdpSessionMode::EasyTierMux`), then data/handshake datagrams with the same
//! connection id.
//!
//! What the test *asserts* is the mechanics that currently work:
//! first-datagram broadcast to every internal port, "first reply wins" session
//! pinning, per-connection-id NAT sessions, malformed datagram dropping. It
//! also prints WARN lines for the design problems it observes (cross-network
//! delivery, `network_name` no longer being used for routing, head-of-line
//! blocking in the accept loop).
//!
//! The ignored test `udp_shared_listener_should_route_by_network_name` encodes
//! the behaviour a network-sharing UDP listener is actually supposed to have;
//! run it with `-- --ignored` after the design is fixed.
//!
//! Run with:
//!
//! ```text
//! CARGO_HOME=/Users/chuyao/Documents/Code/.cargo-home \
//!   cargo test -p dokitier --test udp_shared_probe --offline -- --nocapture
//! ```

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use dokitier_core::packet::{
    PEER_MANAGER_HEADER_SIZE, PacketType, UdpPacketType, ZCPacket, ZCPacketType,
};
use dokitier_proto::peer_rpc::HandshakeRequest;
use prost::Message;
use tokio::net::UdpSocket;
use tokio::time::timeout;

/// How long we wait for a datagram that is supposed to be relayed.
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);
/// How long we wait to prove that a datagram is not delivered.
const DROP_TIMEOUT: Duration = Duration::from_millis(500);

fn free_udp_port() -> u16 {
    let socket = std::net::UdpSocket::bind("127.0.0.1:0").expect("bind ephemeral udp port");
    socket.local_addr().expect("local addr").port()
}

fn handshake_request_bytes(network: &str) -> Vec<u8> {
    HandshakeRequest {
        magic: 0xd1e1_a5e1,
        my_peer_id: 1,
        version: 2,
        features: Vec::new(),
        network_name: network.to_owned(),
        network_secret_digest: vec![0u8; 32],
        p2p_group: String::new(),
    }
    .encode_to_vec()
}

/// Build a datagram in the wire format of an EasyTier UDP mux session:
/// `UDPTunnelHeader(8 B) | PeerManagerHeader(14 B) | payload`.
fn udp_packet(conn_id: u32, packet_type: PacketType, payload: &[u8]) -> Vec<u8> {
    let mut packet = ZCPacket::new_with_payload(payload);
    packet.fill_peer_manager_hdr(1, 0, packet_type as u8);
    let mut packet = packet.convert_type(ZCPacketType::UDP);
    let header = packet.mut_udp_tunnel_header().expect("udp tunnel header");
    header.conn_id.set(conn_id);
    header.msg_type = UdpPacketType::Data as u8;
    header.padding = 0;
    header
        .len
        .set((PEER_MANAGER_HEADER_SIZE + payload.len()) as u16);
    packet.into_bytes().to_vec()
}

fn handshake_datagram(conn_id: u32, network: &str) -> Vec<u8> {
    udp_packet(conn_id, PacketType::HandShake, &handshake_request_bytes(network))
}

fn data_datagram(conn_id: u32, payload: &[u8]) -> Vec<u8> {
    udp_packet(conn_id, PacketType::Data, payload)
}

/// A real EasyTier UDP peer starts with a mux SYN, not with a handshake.
fn syn_datagram(conn_id: u32, magic: u64) -> Vec<u8> {
    dokitier_core::socket::udp::new_syn_packet(conn_id, magic)
        .into_bytes()
        .to_vec()
}

/// The public `rewrite_listeners` hook is what reserves the loopback UDP port
/// a network's internal listener must bind. Read it back from the rewritten
/// network config.
fn internal_udp_port_for(network: &str) -> u16 {
    let toml_text = format!("network_name = {network:?}\n");
    let rewritten = dokitier::shared_listener::rewrite_listeners(&toml_text, network);
    let value: toml::Value =
        toml::from_str(&rewritten).expect("rewritten config must stay valid toml");
    let listeners = value
        .get("listeners")
        .and_then(|value| value.as_array())
        .unwrap_or_else(|| panic!("rewritten config has no listeners: {rewritten}"));
    for listener in listeners {
        if let Some(url) = listener.as_str() {
            if let Some(port) = url.strip_prefix("udp://127.0.0.1:") {
                return port.parse().expect("loopback udp listener port");
            }
        }
    }
    panic!("rewritten config has no loopback udp listener: {rewritten}");
}

async fn recv_from(socket: &UdpSocket, what: &str) -> (Vec<u8>, SocketAddr) {
    let mut buf = vec![0u8; 65535];
    let (len, from) = timeout(PROBE_TIMEOUT, socket.recv_from(&mut buf))
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for: {what}"))
        .unwrap_or_else(|err| panic!("recv error while waiting for {what}: {err}"));
    (buf[..len].to_vec(), from)
}

async fn expect_no_datagram(socket: &UdpSocket, what: &str) {
    let mut buf = vec![0u8; 65535];
    if let Ok(Ok((len, from))) = timeout(DROP_TIMEOUT, socket.recv_from(&mut buf)).await {
        panic!(
            "unexpected {len}-byte datagram from {from} while checking: {what}"
        );
    }
    println!("ok: {what}");
}

async fn start_shared_listener() -> (u16, SocketAddr) {
    let public_port = free_udp_port();
    let public_url = format!("udp://127.0.0.1:{public_port}");
    let started = dokitier::shared_listener::start(vec![public_url.clone()]).await;
    assert_eq!(
        started.as_deref(),
        Some(public_url.as_str()),
        "shared listener must report the bound public udp address"
    );
    assert!(
        dokitier::shared_listener::addr_list().contains(&public_url),
        "addr_list must expose the public udp listener, got {:?}",
        dokitier::shared_listener::addr_list()
    );
    let public_addr: SocketAddr = format!("127.0.0.1:{public_port}").parse().unwrap();
    println!("ok: shared public udp listener bound on {public_url}");
    (public_port, public_addr)
}

struct FakeNetwork {
    name: String,
    port: u16,
    socket: UdpSocket,
}

async fn bind_fake_network(name: &str) -> FakeNetwork {
    let port = internal_udp_port_for(name);
    assert_ne!(port, 0, "rewrite_listeners must allocate a loopback udp port");
    let socket = UdpSocket::bind(("127.0.0.1", port))
        .await
        .unwrap_or_else(|err| panic!("bind fake internal listener for {name} on {port}: {err}"));
    println!("ok: network {name} maps to loopback udp port {port}");
    FakeNetwork {
        name: name.to_owned(),
        port,
        socket,
    }
}

/// Mechanics probe: first-datagram broadcast, reply-wins session pinning,
/// per-conn-id NAT sessions and malformed datagram handling.
#[tokio::test]
async fn udp_shared_listener_broadcast_and_session_probe() {
    println!(
        "layouts: peer_manager_header={PEER_MANAGER_HEADER_SIZE}B, udp datagram = \
         UDPTunnelHeader(8B) + PeerManagerHeader({PEER_MANAGER_HEADER_SIZE}B) + payload"
    );
    let (_public_port, public_addr) = start_shared_listener().await;

    let net_a = bind_fake_network("udp-shared-probe-net-a").await;
    let net_b = bind_fake_network("udp-shared-probe-net-b").await;
    assert_ne!(net_a.port, net_b.port, "networks must get distinct ports");

    let peer = UdpSocket::bind("127.0.0.1:0").await.unwrap();

    // ---- 1. first datagram (mux SYN) is broadcast to every network ------
    let conn1 = 0x1111_1111;
    let syn1 = syn_datagram(conn1, 0xaaaa_bbbb_cccc_dddd);
    peer.send_to(&syn1, public_addr).await.unwrap();
    let (got_a, from_a) = recv_from(&net_a.socket, "SYN broadcast to network A").await;
    let (got_b, from_b) = recv_from(&net_b.socket, "SYN broadcast to network B").await;
    assert_eq!(got_a, syn1, "network A must receive the client SYN unchanged");
    assert_eq!(got_b, syn1, "network B must receive the client SYN unchanged");
    assert_ne!(from_a, from_b, "each candidate network sees its own upstream socket");
    println!(
        "WARN: the first datagram is delivered to *every* network (A got {unexpected} as well), \
         so network isolation is not preserved",
        unexpected = if got_a == syn1 { "the SYN" } else { "?" }
    );

    // ---- 2. first reply wins and pins the peer to that network ----------
    net_b.socket.send_to(b"sack-from-network-b", from_b).await.unwrap();
    let (reply, reply_from) = recv_from(&peer, "SACK relayed from the winning network").await;
    assert_eq!(reply, b"sack-from-network-b");
    assert_eq!(
        reply_from, public_addr,
        "the relayed reply must appear to come from the shared public port"
    );
    println!("ok: first reply (network B) relayed back to the peer");

    // ---- 3. follow-up datagrams of the same conn_id stay on that network -
    let follow_up = data_datagram(conn1, b"follow-up-datagram");
    peer.send_to(&follow_up, public_addr).await.unwrap();
    let (got, _) = recv_from(&net_b.socket, "follow-up pinned to network B").await;
    assert_eq!(got, follow_up, "follow-up must be forwarded unchanged");
    expect_no_datagram(&net_a.socket, "follow-up must not reach the losing network A").await;

    // ---- 4. network name in HandshakeRequest is no longer used ----------
    // (`network_from_udp` is currently dead code; routing is by conn_id only.)
    let handshake_for_a = handshake_datagram(conn1, &net_a.name);
    peer.send_to(&handshake_for_a, public_addr).await.unwrap();
    let (misrouted, _) = recv_from(
        &net_b.socket,
        "handshake addressed to network A on the pinned connection",
    )
    .await;
    assert_eq!(misrouted, handshake_for_a);
    expect_no_datagram(&net_a.socket, "network A must not see its own handshake on this conn_id").await;
    println!(
        "WARN: handshake for network {} was delivered to network {} (conn_id pinning ignores \
         HandshakeRequest.network_name)",
        net_a.name, net_b.name
    );

    // ---- 5. a new conn_id (re)triggers a broadcast for any datagram -----
    let conn2 = 0x2222_2222;
    let handshake2 = handshake_datagram(conn2, &net_a.name);
    peer.send_to(&handshake2, public_addr).await.unwrap();
    let (got_a2, _) = recv_from(&net_a.socket, "new-conn handshake broadcast to A").await;
    let (got_b2, _) = recv_from(&net_b.socket, "new-conn handshake broadcast to B").await;
    assert_eq!(got_a2, handshake2);
    assert_eq!(got_b2, handshake2);
    println!(
        "WARN: a HandshakeRequest datagram for network {} was broadcast to every network, \
         including the network secret digest (if attached)",
        net_a.name
    );

    // Nobody answers the new conn_id: no session, no reply to the peer.
    expect_no_datagram(&peer, "peer gets no reply when no internal network answers").await;
    // The accept loop waits for a winning reply before reading more datagrams;
    // give it time to finish that wait before the next case.
    tokio::time::sleep(Duration::from_millis(1_700)).await;

    // ---- 6. malformed / too-short datagrams are dropped -----------------
    let stranger = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    stranger.send_to(&[0x01, 0x02, 0x03, 0x04], public_addr).await.unwrap();
    expect_no_datagram(&net_a.socket, "short (< 8 byte) datagram must not be broadcast to A").await;
    expect_no_datagram(&net_b.socket, "short (< 8 byte) datagram must not be broadcast to B").await;

    // ---- 7. head-of-line blocking in the accept loop --------------------
    let slow_conn = 0x3333_3333;
    peer.send_to(&syn_datagram(slow_conn, 0x31), public_addr).await.unwrap();
    let _ = recv_from(&net_a.socket, "stalled SYN broadcast to A").await;
    let _ = recv_from(&net_b.socket, "stalled SYN broadcast to B").await;
    // Nobody replies to the stalled SYN, so the accept loop is now waiting the
    // full 1.5s winner-reply timeout before it reads the next datagram.
    tokio::time::sleep(Duration::from_millis(100)).await;
    let next_conn = 0x4444_4444;
    let next_syn = syn_datagram(next_conn, 0x41);
    let sent_at = Instant::now();
    peer.send_to(&next_syn, public_addr).await.unwrap();
    let _ = recv_from(&net_a.socket, "second peer datagram queued behind the stalled one").await;
    let delay = sent_at.elapsed();
    println!(
        "WARN: datagram of another new connection was delayed by the 1.5s reply timeout of the \
         previous first datagram: {delay:?}"
    );
    assert!(
        delay >= Duration::from_millis(800),
        "expected head-of-line blocking of ~1.5s, got {delay:?}"
    );

    println!("udp shared listener broadcast/session probe finished");
}

/// The behaviour a shared UDP listener is supposed to have: after the mux
/// session is established, a `HandshakeRequest` naming network A must be
/// delivered to network A only. Currently this fails because the first
/// datagram is broadcast to all networks and the winner is picked by reply
/// timing, so `HandshakeRequest.network_name` never influences routing.
///
/// Run with:
/// `cargo test -p dokitier --test udp_shared_probe --offline -- --ignored --nocapture`
#[tokio::test]
#[ignore = "documents the intended network_name routing contract; currently broken"]
async fn udp_shared_listener_should_route_by_network_name() {
    let (_public_port, public_addr) = start_shared_listener().await;
    let net_a = bind_fake_network("udp-shared-contract-net-a").await;
    let net_b = bind_fake_network("udp-shared-contract-net-b").await;
    let peer = UdpSocket::bind("127.0.0.1:0").await.unwrap();

    let conn = 0x5151_5151;
    peer.send_to(&syn_datagram(conn, 0x5151), public_addr).await.unwrap();
    let (_, from_a) = recv_from(&net_a.socket, "SYN broadcast to A").await;
    let (_, from_b) = recv_from(&net_b.socket, "SYN broadcast to B").await;
    // Force the *wrong* network (B) to win the race, as reply timing decides.
    net_b.socket.send_to(b"sack-b", from_b).await.unwrap();
    let _ = recv_from(&peer, "SACK relayed from B").await;
    let _ = from_a;

    // The peer now names network A in its handshake; a routing listener must
    // deliver it to A. The current broadcast/pinning implementation sends it to
    // B instead, so this read times out (the test failure is the point).
    let handshake = handshake_datagram(conn, &net_a.name);
    peer.send_to(&handshake, public_addr).await.unwrap();
    let (got, _) = recv_from(&net_a.socket, "handshake routed by network_name to network A").await;
    assert_eq!(got, handshake);
    expect_no_datagram(&net_b.socket, "network B must not receive network A's handshake").await;
}
