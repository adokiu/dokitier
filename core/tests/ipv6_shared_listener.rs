//! Focused IPv6 probe for the core-level shared listeners, updated for the
//! single-hop UDP design (`NetworkAnnounce` + `UdpSessionLayer` + handoff).
//!
//! Checkpoints covered here (black box, public API only):
//!
//! 1. `udp://[::]:port` binds as an IPv6 wildcard and `start`/`apply`/
//!    `addr_list` report the *actually bound* address (with brackets), not the
//!    configured string (`:0` included).
//! 2. Runtime advertisement (`advertise_urls()`, never written to config
//!    files) is generated as `scheme://[v6]:port` when the advertised IP is
//!    IPv6 -- via `set_public_ip6`, `DOKITIER_SHARED_ADVERTISE=2001:db8::7`,
//!    `=[2001:db8::7]` and `=[2001:db8::7]:443`.
//! 3. `advertise_host()` precedence (`PUBLIC_IP` v4 before `PUBLIC_IP6`) is
//!    asserted as current behaviour and printed as a WARN: the controller sets
//!    both, so a dual-stack host advertises its IPv4 address even when the
//!    shared listener is IPv6.
//! 4. A `[::]` UDP wildcard is dual-stack on this host (kernel default
//!    `IPV6_V6ONLY=0`): a second `0.0.0.0:port` bind conflicts, and mixing
//!    `udp://[::]:p` with `udp://0.0.0.0:p` fails the whole `apply`; the failed
//!    hot-apply restores the previous address set and leaks no listener (the
//!    old `bound.push(normalized)` task-leak bug is fixed).
//! 5. `ws://`/`wss://`/`quic://` `[::]` listeners bind, report bracketed
//!    addresses and are reachable over IPv6.
//! 6. The UDP session key is now a struct `{ local_port, client: SocketAddr,
//!    conn_id }` (`core/src/shared_listener.rs`), so `SocketAddrV6` equality/
//!    hashing distinguishes IPv6 scope id (and flowinfo), unlike the old
//!    `format!("{local_port}:{client}:{conn_id}")` Display-string key. The last
//!    section documents the std `SocketAddrV6` semantics this relies on.
//!
//! Run:
//!
//! ```text
//! PROTOC=/Users/chuyao/Documents/Code/.protoc/bin/protoc \
//!   CARGO_HOME=/Users/chuyao/Documents/Code/.cargo-home \
//!   cargo test -p dokitier --test ipv6_shared_listener -- --nocapture
//! ```

use std::collections::hash_map::DefaultHasher;
use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV6};
use std::time::Duration;

use serde::Deserialize;
use tokio::net::TcpStream;
use tokio::time::timeout;

const NET: &str = "ipv6-shared-probe";
const ADVERTISE_V6: &str = "2001:db8::7";

/// Mirrors the instance-config TOML shape (`listeners` is
/// `Option<Vec<url::Url>>` in `crates/dokitier-core/src/config/toml.rs`).
#[derive(Debug, Deserialize)]
struct ListenerConfig {
    #[serde(default)]
    listeners: Option<Vec<url::Url>>,
}

fn setenv(key: &str, value: &str) {
    // edition 2024 made process-env mutation unsafe.
    unsafe { std::env::set_var(key, value) };
}

fn unsetenv(key: &str) {
    unsafe { std::env::remove_var(key) };
}

fn free_udp_port() -> u16 {
    std::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
        .expect("reserve ephemeral udp port")
        .local_addr()
        .expect("local addr")
        .port()
}

/// (scheme, address-after-"://", port) of a bound listener string.
fn split_bound(bound: &str) -> (String, String, u16) {
    let (scheme, rest) = bound
        .split_once("://")
        .unwrap_or_else(|| panic!("bound listener `{bound}` has no scheme"));
    let port = rest
        .rsplit(':')
        .next()
        .and_then(|port| port.parse::<u16>().ok())
        .unwrap_or_else(|| panic!("bound listener `{bound}` has no numeric port"));
    (scheme.to_string(), rest.to_string(), port)
}

fn rewrite(network: &str) -> ListenerConfig {
    let input = format!("network_name = \"{network}\"\n");
    let rewritten = dokitier::shared_listener::rewrite_listeners(&input, network);
    toml::from_str(&rewritten).unwrap_or_else(|err| {
        panic!("rewritten config is not parseable as instance config: {err}\n{rewritten}")
    })
}

/// Runtime advertised URL for the UDP shared listener (not part of any config
/// file: advertisement is purely in-process state).
fn advertised_udp() -> url::Url {
    dokitier::shared_listener::advertise_urls()
        .into_iter()
        .find(|url| url.scheme() == "udp")
        .expect("no advertised udp listener")
}

fn print_urls(label: &str, urls: &[url::Url]) {
    println!("{label}: {urls:?}");
}

/// Retry `start`/`apply` with fresh ports until the OS lets us bind.
async fn start_udp_wildcard() -> (u16, String) {
    for _ in 0..8 {
        let port = free_udp_port();
        let addr = format!("udp://[::]:{port}");
        if dokitier::shared_listener::start(vec![addr.clone()]).await.is_some() {
            return (port, addr);
        }
    }
    panic!("could not bind a `udp://[::]:port` shared listener");
}

fn hash_of<T: Hash>(value: &T) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ipv6_shared_listener_probe() {
    for key in [
        "DOKITIER_SHARED_ADVERTISE",
        "DOKITIER_SHARED_LISTENERS",
        "DOKITIER_SHARED_LISTENER",
    ] {
        unsetenv(key);
    }

    // =====================================================================
    // 1. udp://[::]:port wildcard bind, reported address, dual-stack
    // =====================================================================
    println!("\n=== 1. udp://[::]:port wildcard bind ===");
    let (udp_port, requested) = start_udp_wildcard().await;
    let bound = dokitier::shared_listener::addr_list();
    println!("requested: {requested}\nbound:     {bound:?}");
    assert_eq!(
        bound,
        vec![format!("udp://[::]:{udp_port}")],
        "start() must report the OS-bound IPv6 wildcard address"
    );
    let (scheme, rest, reported_port) = split_bound(&bound[0]);
    assert_eq!(scheme, "udp");
    assert!(
        rest.starts_with("[::]:"),
        "the reported address must keep the bracketed wildcard: {}",
        bound[0]
    );
    assert_eq!(reported_port, udp_port, "reported port must be the bound port");

    // `[::]` is dual-stack on this host when a second 0.0.0.0:port bind fails.
    let v4_conflict = std::net::UdpSocket::bind((Ipv4Addr::UNSPECIFIED, udp_port)).is_err();
    let v4_send = std::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).map(|socket| {
        socket
            .send_to(b"probe", (Ipv4Addr::LOCALHOST, udp_port))
            .is_ok()
    });
    println!(
        "dual-stack: v4 0.0.0.0:{udp_port} bind conflicts={v4_conflict}, v4 send ok={v4_send:?}"
    );
    if v4_conflict {
        println!(
            "NOTE: `[::]` is dual-stack here (IPV6_V6ONLY=0): it also owns IPv4:{udp_port}. \
             A separate `udp://0.0.0.0:{udp_port}` cannot be added; on Windows (v6only=1) it \
             would be IPv6-only. The shared listener never sets IPV6_V6ONLY explicitly."
        );
    } else {
        println!("NOTE: `[::]` is IPv6-only on this host (IPV6_V6ONLY=1).");
    }

    // =====================================================================
    // 2. runtime advertisement: IPv6 advertised IP must be bracketed and valid
    // =====================================================================
    println!("\n=== 2. advertise_urls() with IPv6 advertise ===");
    dokitier::shared_listener::set_public_ip6(Some(ADVERTISE_V6.to_string()));
    let config = rewrite(NET);
    let internal = config.listeners.as_ref().expect("listeners must be injected");
    print_urls("internal listeners", internal);
    assert!(
        internal.iter().any(|url| url.scheme() == "udp"
            && url.host_str() == Some("127.0.0.1")),
        "internal instance listener must stay loopback: {internal:?}"
    );
    let advertised = dokitier::shared_listener::advertise_urls();
    print_urls("advertise_urls (set_public_ip6)", &advertised);
    let udp_mapped = advertised_udp();
    assert_eq!(
        udp_mapped.host_str(),
        Some(format!("[{ADVERTISE_V6}]").as_str()),
        "advertised listener must bracket the IPv6 public IP: {udp_mapped}"
    );
    assert_eq!(udp_mapped.port(), Some(udp_port), "mapped port must match bound port");
    assert_eq!(
        udp_mapped.as_str(),
        format!("udp://[{ADVERTISE_V6}]:{udp_port}"),
        "advertised IPv6 listener URL must keep brackets"
    );
    println!("PASS advertised listener = {udp_mapped}");

    // 2b. the env override must accept bare / bracketed / host:port IPv6 forms.
    for advertise in [
        ADVERTISE_V6.to_string(),
        format!("[{ADVERTISE_V6}]"),
        format!("[{ADVERTISE_V6}]:443"),
    ] {
        setenv("DOKITIER_SHARED_ADVERTISE", &advertise);
        let _config = rewrite(NET);
        let udp_mapped = advertised_udp();
        assert_eq!(
            udp_mapped.host_str(),
            Some(format!("[{ADVERTISE_V6}]").as_str()),
            "DOKITIER_SHARED_ADVERTISE={advertise} -> {udp_mapped}"
        );
        assert_eq!(udp_mapped.port(), Some(udp_port));
        println!("PASS DOKITIER_SHARED_ADVERTISE={advertise} -> {udp_mapped}");
    }
    unsetenv("DOKITIER_SHARED_ADVERTISE");

    // 2c. current precedence: PUBLIC_IP (IPv4) wins over PUBLIC_IP6.
    dokitier::shared_listener::set_public_ip(Some("203.0.113.7".to_string()));
    let _config = rewrite(NET);
    let udp_mapped = advertised_udp();
    println!("advertise precedence: advertised = {udp_mapped}");
    assert_eq!(
        udp_mapped.host_str(),
        Some("203.0.113.7"),
        "current behaviour: PUBLIC_IP (IPv4) takes precedence over PUBLIC_IP6"
    );
    println!(
        "WARN: `advertise_host()` checks PUBLIC_IP before PUBLIC_IP6 \
         (core/src/shared_listener.rs), and controller_agent sets PUBLIC_IP from \
         detect_public_ips().0; on a dual-stack host the mapped_listeners therefore advertise \
         the IPv4 address even when the shared listener is IPv6-only. An explicit \
         DOKITIER_SHARED_ADVERTISE=<v6> overrides it."
    );

    // =====================================================================
    // 3. UDP session key: struct with SocketAddr -> scope id/flowinfo distinct
    // =====================================================================
    println!("\n=== 3. UDP session key: SocketAddr scope id / flowinfo ===");
    // core/src/shared_listener.rs uses:
    //   struct UdpSessionKey { local_port: u16, client: SocketAddr, conn_id: u32 }
    // so Eq/Hash of `SocketAddr` decide whether two clients collide.
    let scope = |flowinfo: u32, scope_id: u32| {
        SocketAddr::V6(SocketAddrV6::new(
            "fe80::1".parse().unwrap(),
            5000,
            flowinfo,
            scope_id,
        ))
    };
    let mut keys: HashSet<(u16, SocketAddr, u32)> = HashSet::new();
    keys.insert((1234, scope(0, 1), 7));
    keys.insert((1234, scope(0, 2), 7));
    keys.insert((1234, scope(1, 1), 7));
    assert_eq!(
        keys.len(),
        3,
        "scope id and flowinfo must both separate UdpSessionKey entries"
    );
    assert_ne!(hash_of(&scope(0, 1)), hash_of(&scope(0, 2)));
    assert_ne!(hash_of(&scope(1, 1)), hash_of(&scope(2, 1)));
    println!("PASS same link-local addr + conn_id yields distinct keys per scope id / flowinfo");
    println!(
        "NOTE: the old Display-string key `format!(\"{{local_port}}:{{client}}:{{conn_id}}\")` \
         dropped flowinfo (and `SocketAddrV6` Display prints `%scope` only); the current struct \
         key keeps both, so IPv6 link-local clients on different interfaces cannot collide."
    );

    // =====================================================================
    // 4. `:0` is resolved to the real port (tcp + udp)
    // =====================================================================
    println!("\n=== 4. ephemeral `:0` reporting (tcp/udp) ===");
    for scheme in ["tcp", "udp"] {
        let applied = dokitier::shared_listener::apply(vec![format!("{scheme}://[::]:0")])
            .await
            .unwrap_or_else(|err| panic!("apply({scheme}://[::]:0) failed: {err:#}"));
        let bound = dokitier::shared_listener::addr_list();
        println!("{scheme} :0 -> {bound:?}");
        assert_eq!(applied, bound);
        let (_, rest, port) = split_bound(&bound[0]);
        assert!(
            rest.starts_with("[::]:"),
            "bracketed wildcard must survive: {bound:?}"
        );
        assert_ne!(port, 0, "`:0` must be replaced by the OS-assigned port: {bound:?}");
    }

    // =====================================================================
    // 5. `[::]` + `0.0.0.0` same port: clean failure + rollback, no leak
    // =====================================================================
    println!("\n=== 5. mixed `[::]`/`0.0.0.0` same port on a dual-stack host ===");
    let (udp_port, requested) = start_udp_wildcard().await;
    let before = dokitier::shared_listener::addr_list();
    if v4_conflict {
        let free = free_udp_port();
        let conflict_addrs = vec![
            format!("udp://[::]:{free}"),
            format!("udp://0.0.0.0:{free}"),
        ];
        let result = dokitier::shared_listener::apply(conflict_addrs.clone()).await;
        println!("apply({conflict_addrs:?}) -> {result:?}");
        assert!(
            result.is_err(),
            "mixing udp://[::]:p with udp://0.0.0.0:p must fail on a dual-stack host"
        );
        assert_eq!(
            dokitier::shared_listener::addr_list(),
            before,
            "failed apply must roll back to the previous listener set"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
        let leaked = std::net::UdpSocket::bind((Ipv4Addr::UNSPECIFIED, free));
        assert!(
            leaked.is_ok(),
            "failed bind leaked a listener: 0.0.0.0:{free} is still held"
        );
        drop(leaked);
        println!("PASS failed mixed apply rolled back and leaked no listener");
    } else {
        println!("skipped: host is IPv6-only, `[::]`+`0.0.0.0` do not conflict");
    }
    let _ = (udp_port, requested);

    // =====================================================================
    // 6. ws/wss/quic `[::]` listeners bind and are reachable over IPv6
    // =====================================================================
    println!("\n=== 6. ws/wss/quic `[::]` listeners ===");
    let mut addrs = vec![
        "tcp://[::]:0".to_string(),
        "udp://[::]:0".to_string(),
    ];
    #[cfg(feature = "websocket")]
    {
        addrs.push("ws://[::]:0".to_string());
        addrs.push("wss://[::]:0".to_string());
    }
    #[cfg(feature = "quic")]
    addrs.push("quic://[::]:0".to_string());
    let bound = dokitier::shared_listener::apply(addrs.clone())
        .await
        .expect("apply must bind every `[::]:0` listener");
    println!("bound: {bound:?}");
    assert_eq!(bound.len(), addrs.len());
    for addr in &bound {
        let (scheme, rest, port) = split_bound(addr);
        assert!(
            rest.starts_with("[::]:"),
            "`{scheme}` must report the bracketed wildcard: {addr}"
        );
        assert_ne!(port, 0, "`{scheme}` must report the real port: {addr}");
    }

    async fn tcp_reachable(addr: SocketAddr) -> bool {
        matches!(
            timeout(Duration::from_millis(800), TcpStream::connect(addr)).await,
            Ok(Ok(_))
        )
    }

    let find = |scheme: &str| -> u16 {
        bound
            .iter()
            .find(|addr| addr.starts_with(&format!("{scheme}://")))
            .map(|addr| split_bound(addr).2)
            .unwrap_or_else(|| panic!("no `{scheme}` listener in {bound:?}"))
    };
    assert!(
        tcp_reachable((Ipv6Addr::LOCALHOST, find("tcp")).into()).await,
        "tcp://[::]:port must accept ::1"
    );
    let udp_port = find("udp");
    assert!(
        std::net::UdpSocket::bind((Ipv6Addr::UNSPECIFIED, udp_port)).is_err(),
        "udp://[::]:{udp_port} must hold its IPv6 UDP port"
    );
    #[cfg(feature = "websocket")]
    for scheme in ["ws", "wss"] {
        let port = find(scheme);
        assert!(
            tcp_reachable((Ipv6Addr::LOCALHOST, port).into()).await,
            "`{scheme}://[::]:{port}` must accept an IPv6 TCP connection"
        );
    }
    #[cfg(feature = "quic")]
    {
        let port = find("quic");
        assert!(
            std::net::UdpSocket::bind((Ipv6Addr::UNSPECIFIED, port)).is_err(),
            "quic://[::]:{port} must hold its IPv6 UDP port"
        );
    }
    println!("PASS all `[::]` listeners accept IPv6 / hold their ports");

    println!("\nipv6 shared listener probe finished");
}
