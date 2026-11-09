//! Black-box tests for the UDP single-hop path of `core/src/shared_listener.rs`.
//!
//! The interesting items are private: [`SharedUdpSocket`] (one channel-fed
//! receive view over the shared public `UdpSocket`), `ensure_shared_udp_network`
//! (creates the per-network `UdpSessionLayer` handed to the owning instance),
//! and the datagram dispatcher inside `udp_accept_loop`:
//!
//! ```text
//! NetworkAnnounce -> remember client -> network, create that network's layer
//! other datagram  -> announced network, else the single exposed udp network,
//!                    else drop
//! ```
//!
//! This test therefore only uses public API and real bytes on real sockets:
//!
//! * `dokitier::shared_listener::{start, rewrite_listeners, addr_list}` — the
//!   public entry points that own the shared public UDP port and register the
//!   networks a core exposes (`rewrite_listeners` inserts into the private
//!   `udp_networks` set the single-network fallback consults).
//! * `dokitier::shared_listener_handoff::{register_instance, ...}` — the public
//!   network -> `PeerManagerCore` registry the dispatcher resolves networks
//!   through. A real `PeerManagerCore` comes from a real core instance created
//!   with `dokitier::instance::factory::create_native_instance`; no TUN device,
//!   listener or external network is needed because the assertions stop at the
//!   mux layer.
//! * `dokitier_core::socket::udp::{new_network_announce_packet, new_syn_packet,
//!   new_sack_packet}` — the exact EasyTier wire format a real peer sends.
//!
//! The scenarios prove:
//!
//! 1. A single exposed UDP-shared network routes a SYN that arrives *without* a
//!    preceding `NetworkAnnounce`, and the reply is the expected SACK sent from
//!    the shared public socket (single-hop: the SACK is produced by the network
//!    layer and written straight out of the public socket).
//! 2. A client that announced an unknown network is not silently routed through
//!    the unique exposed network.
//! 3. With two exposed networks, a SYN without an announce is dropped (no SACK).
//! 4. With two exposed networks, `NetworkAnnounce` selects the announced network
//!    for the following SYN; exactly one SACK comes back (a broadcast fan-out
//!    would produce one SACK per network).
//!
//! Run with:
//!
//! ```text
//! PROTOC=/Users/chuyao/Documents/Code/.protoc/bin/protoc \
//!   CARGO_HOME=/Users/chuyao/Documents/Code/.cargo-home \
//!   cargo test --offline -p dokitier --test shared_udp_direct -- --nocapture
//! ```
//!
//! The scenarios deliberately live in one `#[tokio::test]`: `shared_listener`
//! keeps process-global state (`STATE`, `udp_networks`) that `start()` does not
//! reset, so the "exactly one exposed network" fallback can only be observed
//! before a second network is registered in the process.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use dokitier::instance::factory::NativeCoreInstance;
use dokitier::shared_listener;
use dokitier::shared_listener_handoff;
use dokitier_core::config::NetworkIdentity;
use dokitier_core::config::toml::{ConfigLoader as _, TomlConfig};
use dokitier_core::instance::factory::create_native_instance;
use dokitier_core::peers::peer_manager::PeerManagerCore;
use dokitier_core::socket::udp::{new_network_announce_packet, new_sack_packet, new_syn_packet};
use tokio::net::UdpSocket;
use tokio::time::timeout;

/// How long a positive expectation waits for the expected datagram.
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(3);
/// How long a negative expectation waits to prove that nothing arrives.
const SILENCE_TIMEOUT: Duration = Duration::from_millis(700);
/// Give the dispatcher time to consume the `NetworkAnnounce` before the SYN.
/// Both datagrams travel from the same socket to the same socket, so this is
/// only a safety margin for the UDP receive ordering.
const ANNOUNCE_SETTLE: Duration = Duration::from_millis(100);

/// The shared listener only relays to a network that resolves to a peer
/// manager. Real instances are used instead of a mock: `PeerManagerCore` has no
/// public constructor, but a native `CoreInstance` is public and cheap to build
/// on its own executor (construction does not start TUN/listeners/STUN).
///
/// The returned instances must stay alive for the whole scenario: the
/// dispatcher's `ensure_shared_udp_network` task owns the handoff side of the
/// layer and keeps resolving the registered `PeerManagerCore` while it runs.
fn spawn_network_instance(network: &str) -> Arc<NativeCoreInstance> {
    let instance = create_native_instance(isolated_config(network))
        .unwrap_or_else(|err| panic!("create native instance for network {network}: {err:#}"));
    let peer_manager: Arc<PeerManagerCore> = instance.peer_manager().clone();
    shared_listener_handoff::register_instance(network, peer_manager);
    instance
}

/// Minimal core config: no TUN, no listeners, no STUN, no P2P, no discovery.
/// Mirrors the native factory's own isolated test configuration.
fn isolated_config(network: &str) -> TomlConfig {
    let config = TomlConfig::default();
    config.set_network_identity(NetworkIdentity::new(network.to_owned(), String::new()));
    let mut flags = config.get_flags();
    flags.no_tun = true;
    flags.enable_ipv6 = false;
    flags.disable_upnp = true;
    flags.disable_p2p = true;
    flags.disable_tcp_hole_punching = true;
    flags.disable_udp_hole_punching = true;
    flags.multi_thread = false;
    flags.multi_thread_count = 2;
    config.set_flags(flags);
    config.set_listeners(Vec::new());
    config.set_stun_servers(Some(Vec::new()));
    config.set_stun_servers_v6(Some(Vec::new()));
    config.set_tcp_stun_servers(Some(Vec::new()));
    config
}

/// `rewrite_listeners` is the public representation of "this core exposes a
/// network over the shared listeners": for UDP it also reserves the instance's
/// loopback port and, crucially, records the network in the private
/// `udp_networks` set consulted by the dispatcher's unique-network fallback.
fn expose_udp_network(network: &str) {
    let toml_text = format!("network_name = {network:?}\n");
    let rewritten = shared_listener::rewrite_listeners(&toml_text, network);
    assert!(
        rewritten.contains("udp://127.0.0.1:"),
        "rewrite_listeners must inject a loopback udp listener for {network}: {rewritten}"
    );
}

/// Starts the shared listener on an ephemeral loopback UDP port and returns the
/// public socket address clients must talk to.
async fn start_shared_listener() -> SocketAddr {
    let started = shared_listener::start(vec!["udp://127.0.0.1:0".to_owned()])
        .await
        .expect("shared_listener::start must bind udp://127.0.0.1:0");
    assert!(
        started.starts_with("udp://127.0.0.1:"),
        "shared listener must report the bound public udp address, got {started}"
    );
    assert!(
        shared_listener::addr_list().contains(&started),
        "addr_list must expose {started}, got {:?}",
        shared_listener::addr_list()
    );
    let port: u16 = started
        .rsplit(':')
        .next()
        .and_then(|port| port.parse().ok())
        .unwrap_or_else(|| panic!("cannot parse public udp port from {started}"));
    println!("ok: shared public udp listener on udp://127.0.0.1:{port}");
    SocketAddr::from(([127, 0, 0, 1], port))
}

/// Restarts the shared listener on a fresh ephemeral port. `start()` tears down
/// the previous per-network layers/channels/announce map while keeping the
/// process-global `udp_networks` set: exactly what a scenario that must not see
/// the previous channels needs.
async fn restart_shared_listener() -> SocketAddr {
    let public = start_shared_listener().await;
    // The old public socket is only released once its accept loop is aborted;
    // give the runtime one scheduling turn before reusing it.
    tokio::time::sleep(Duration::from_millis(50)).await;
    public
}

async fn bind_client() -> UdpSocket {
    UdpSocket::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral client udp socket")
}

async fn announce(socket: &UdpSocket, public: SocketAddr, network: &str) {
    let packet = new_network_announce_packet(network).into_bytes();
    socket
        .send_to(&packet, public)
        .await
        .unwrap_or_else(|err| panic!("send NetworkAnnounce for {network}: {err}"));
    println!("client -> {public}: NetworkAnnounce({network})");
    tokio::time::sleep(ANNOUNCE_SETTLE).await;
}

async fn send_syn(socket: &UdpSocket, public: SocketAddr, conn_id: u32, magic: u64) {
    let packet = new_syn_packet(conn_id, magic).into_bytes();
    socket
        .send_to(&packet, public)
        .await
        .unwrap_or_else(|err| panic!("send mux SYN conn_id={conn_id:#010x}: {err}"));
    println!("client -> {public}: SYN conn_id={conn_id:#010x} magic={magic:#018x}");
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect::<Vec<_>>().join("")
}

/// Waits for the SACK that the routed network layer must send back through the
/// shared public socket.
async fn expect_sack(
    socket: &UdpSocket,
    public: SocketAddr,
    conn_id: u32,
    magic: u64,
    what: &str,
) {
    let expected = new_sack_packet(conn_id, magic).into_bytes();
    let mut buf = vec![0u8; 65535];
    let (len, from) = timeout(RESPONSE_TIMEOUT, socket.recv_from(&mut buf))
        .await
        .unwrap_or_else(|_| {
            panic!(
                "timed out waiting for SACK ({what}); shared_listener::stats()={}",
                shared_listener::stats()
            )
        })
        .unwrap_or_else(|err| panic!("recv error while waiting for SACK ({what}): {err}"));
    let got = &buf[..len];
    assert_eq!(
        from, public,
        "SACK ({what}) must be sent from the shared public socket {public}, not {from}"
    );
    assert_eq!(
        hex(got),
        hex(&expected),
        "unexpected SACK datagram ({what}): got {len} bytes"
    );
    println!("ok: public socket replied with the expected SACK ({what})");
}

/// Proves that no further datagram arrives, e.g. that one routed SYN produced
/// exactly one SACK and no cross-network duplicate.
async fn expect_no_datagram(socket: &UdpSocket, what: &str) {
    let mut buf = vec![0u8; 65535];
    match timeout(SILENCE_TIMEOUT, socket.recv_from(&mut buf)).await {
        Ok(Ok((len, from))) => panic!(
            "unexpected {len}-byte datagram from {from} while expecting silence: {what} \
             (payload={})",
            hex(&buf[..len])
        ),
        Ok(Err(err)) => panic!("recv error while expecting silence ({what}): {err}"),
        Err(_) => println!("ok: no datagram, as expected ({what})"),
    }
}

/// The whole routing matrix in one test; see the module docs for why the phases
/// cannot be independent test functions (process-global `udp_networks`).
#[tokio::test]
async fn shared_udp_direct_routing_matrix() {
    let net_a = "udp-direct-net-a";
    let net_b = "udp-direct-net-b";

    // ------------------------------------------------------------------
    // Phase 1: exactly one exposed udp network, SYN without announce.
    // ------------------------------------------------------------------
    let public = start_shared_listener().await;
    expose_udp_network(net_a);
    let instances = vec![spawn_network_instance(net_a)];

    let conn_fallback = 0x1111_0001;
    let magic_fallback = 0xaaaa_0001_0001_0001;
    let client = bind_client().await;
    send_syn(&client, public, conn_fallback, magic_fallback).await;
    expect_sack(
        &client,
        public,
        conn_fallback,
        magic_fallback,
        "single exposed network, no announce: routed by the unique-network fallback",
    )
    .await;
    expect_no_datagram(
        &client,
        "one routed SYN must yield exactly one SACK (no duplicate from a second network)",
    )
    .await;
    println!("PASS 1: single-network fallback routes the SYN and the public socket replies with SACK");

    // ------------------------------------------------------------------
    // Phase 1b: the client announced an unknown network. The announce
    // mapping is authoritative; it must not fall back to the unique
    // network, otherwise the mapping would be meaningless.
    // ------------------------------------------------------------------
    let conn_unknown = 0x1111_0002;
    let client = bind_client().await;
    announce(&client, public, "udp-direct-net-not-registered").await;
    send_syn(&client, public, conn_unknown, 0xbbbb_0002_0002_0002).await;
    expect_no_datagram(
        &client,
        "announce for an unregistered network must not fall back to the unique exposed network",
    )
    .await;
    println!("PASS 2: announce for a different network is not silently rerouted to the unique network");

    // ------------------------------------------------------------------
    // Phase 2: a second udp network appears. No announce => ambiguous =>
    // the dispatcher must drop the datagram.
    // ------------------------------------------------------------------
    // Restart clears the per-network channels and the announce map (the
    // process-global `udp_networks` set intentionally survives), so both
    // announce-routed networks below get a freshly created layer.
    let public = restart_shared_listener().await;
    expose_udp_network(net_b);
    let mut instances = instances;
    instances.push(spawn_network_instance(net_b));

    let client = bind_client().await;
    send_syn(&client, public, 0x2222_0001, 0xcccc_0001_0001_0001).await;
    expect_no_datagram(
        &client,
        "SYN without announce while two udp networks are exposed must be dropped",
    )
    .await;
    println!("PASS 3: multi-network SYN without announce is dropped (no SACK)");

    // ------------------------------------------------------------------
    // Phase 3: announce selects the network. Two exposed networks and two
    // resolvable instances mean a broadcast would produce two SACKs.
    // ------------------------------------------------------------------
    let conn_a = 0x3333_0001;
    let magic_a = 0xdddd_0001_0001_0001;
    let client = bind_client().await;
    announce(&client, public, net_a).await;
    send_syn(&client, public, conn_a, magic_a).await;
    expect_sack(
        &client,
        public,
        conn_a,
        magic_a,
        "multi-network, announce net-a: routed to net-a",
    )
    .await;
    expect_no_datagram(
        &client,
        "announce net-a must deliver the SYN to net-a only (no net-b SACK)",
    )
    .await;
    println!("PASS 4: announce selects net-a in a multi-network setup");

    let conn_b = 0x3333_0002;
    let magic_b = 0xdddd_0002_0002_0002;
    let client = bind_client().await;
    announce(&client, public, net_b).await;
    send_syn(&client, public, conn_b, magic_b).await;
    expect_sack(
        &client,
        public,
        conn_b,
        magic_b,
        "multi-network, announce net-b: routed to net-b",
    )
    .await;
    expect_no_datagram(
        &client,
        "announce net-b must deliver the SYN to net-b only (no net-a SACK)",
    )
    .await;
    println!("PASS 5: announce selects net-b in a multi-network setup");

    // Keep the instances alive until every datagram was handled.
    drop(instances);
    println!("PASS: shared udp single-hop routing matrix finished");
}
