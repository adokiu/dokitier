//! Core-level shared listeners.
//!
//! All network instances can share one public endpoint per protocol. Each
//! instance still binds its own loopback listener on an internal port; this
//! module owns the public ports and forwards a connection/datagram to the
//! right network by peeking at the existing EasyTier `HandshakeRequest`
//! (which already carries the network name before encryption).

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use bytes::BytesMut;
use prost::Message;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};

use dokitier_core::socket::udp::{
    UdpSessionLayer, UdpSocketRecvMeta, UdpSocketSendMeta, VirtualUdpSocket,
};
use dokitier_core::tunnel::udp::UdpTunnelUpgrader;
use dokitier_proto::proto::common::TunnelInfo;
use dokitier_core::packet::{
    PacketType, ZCPacket, ZCPacketType, PEER_MANAGER_HEADER_SIZE, UDP_TUNNEL_HEADER_SIZE,
};
use dokitier_proto::peer_rpc::HandshakeRequest;

const FIRST_FRAME_TIMEOUT_SECS: u64 = 8;
const MAX_FIRST_FRAME: usize = 64 * 1024;
const UDP_SESSION_IDLE: Duration = Duration::from_secs(180);

struct SharedState {
    addrs: Mutex<Vec<String>>,
    ports: Mutex<HashMap<String, u16>>,
    udp_ports: Mutex<HashMap<String, u16>>,
    ws_ports: Mutex<HashMap<String, u16>>,
    quic_ports: Mutex<HashMap<String, u16>>,
    udp_sessions: Mutex<HashMap<UdpSessionKey, Arc<UdpSession>>>,
    tasks: Mutex<Vec<tokio::task::JoinHandle<()>>>,
    session_tasks: Mutex<Vec<tokio::task::JoinHandle<()>>>,
    shared_udp: Mutex<HashMap<String, tokio::sync::mpsc::Sender<(Vec<u8>, std::net::SocketAddr)>>>,
    announce: Mutex<HashMap<std::net::SocketAddr, (String, Instant)>>,
    udp_networks: Mutex<std::collections::HashSet<String>>,
}

#[derive(Clone, Copy, Eq, Hash, PartialEq)]
struct UdpSessionKey {
    local_port: u16,
    client: SocketAddr,
    conn_id: u32,
}

struct UdpSession {
    upstream: Option<Arc<UdpSocket>>,
    up_conn_id: u32,
    network: String,
    last: Mutex<Instant>,
    alive: AtomicBool,
    pending: AtomicBool,
}

static STATE: OnceLock<SharedState> = OnceLock::new();
static PUBLIC_IP: std::sync::RwLock<Option<String>> = std::sync::RwLock::new(None);
static PUBLIC_IP6: std::sync::RwLock<Option<String>> = std::sync::RwLock::new(None);

static TCP_ACCEPTED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static TCP_HANDOFF: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static TCP_RELAY: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static UDP_DATAGRAMS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static UDP_SESSIONS_TOTAL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static SHARED_ERRORS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub fn stats() -> serde_json::Value {
    use std::sync::atomic::Ordering::Relaxed;
    serde_json::json!({
        "tcp_accepted": TCP_ACCEPTED.load(Relaxed),
        "tcp_handoff": TCP_HANDOFF.load(Relaxed),
        "tcp_relay": TCP_RELAY.load(Relaxed),
        "udp_datagrams": UDP_DATAGRAMS.load(Relaxed),
        "udp_sessions_total": UDP_SESSIONS_TOTAL.load(Relaxed),
        "udp_no_announce": SHARED_UDP_NO_ANNOUNCE.load(Relaxed),
        "errors": SHARED_ERRORS.load(Relaxed),
    })
}

pub fn set_public_ip(ip: Option<String>) {
    if let Some(ip) = ip.filter(|ip| !ip.is_empty()) {
        *PUBLIC_IP.write().unwrap() = Some(ip);
    }
}

pub fn set_public_ip6(ip: Option<String>) {
    if let Some(ip) = ip.filter(|ip| !ip.is_empty()) {
        *PUBLIC_IP6.write().unwrap() = Some(ip);
    }
}

fn advertise_host() -> Option<String> {
    if let Ok(value) = std::env::var("DOKITIER_SHARED_ADVERTISE") {
        let value = value.trim().trim_start_matches("tcp://").trim_start_matches("udp://").trim_start_matches("ws://").trim_start_matches("wss://").trim_start_matches("quic://");
        if let Some(rest) = value.strip_prefix('[') {
            return rest.split(']').next().map(|host| host.to_string());
        }
        if value.matches(':').count() > 1 {
            return Some(value.to_string());
        }
        if !value.is_empty() {
            return Some(value.split(':').next().unwrap_or(value).to_string());
        }
    }
    if let Some(ip) = PUBLIC_IP.read().unwrap().clone() {
        return Some(ip);
    }
    if let Some(ip) = PUBLIC_IP6.read().unwrap().clone() {
        return Some(ip);
    }
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("8.8.8.8:80").ok()?;
    Some(socket.local_addr().ok()?.ip().to_string())
}

fn url_host_port(host: &str, port: u16) -> String {
    if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

fn configured_addrs() -> Vec<String> {
    let mut result = Vec::new();
    if let Ok(value) = std::env::var("DOKITIER_SHARED_LISTENERS") {
        result.extend(value.split(',').map(str::trim).filter(|v| !v.is_empty()).map(|v| v.to_string()));
    }
    if let Ok(value) = std::env::var("DOKITIER_SHARED_LISTENER") {
        if !value.trim().is_empty() {
            result.push(value.trim().to_string());
        }
    }
    result
}

fn proto_of(addr: &str) -> &str {
    match addr.split_once("://") {
        Some((proto, _)) => proto,
        None => "tcp",
    }
}

fn bind_of(addr: &str) -> &str {
    addr.split_once("://").map(|(_, rest)| rest).unwrap_or(addr)
}

fn normalize(addr: &str) -> String {
    if addr.contains("://") { addr.to_string() } else { format!("tcp://{addr}") }
}

fn shared_ports() -> Vec<u16> {
    let Some(state) = STATE.get() else { return Vec::new() };
    state.addrs.lock().unwrap().iter()
        .filter(|addr| proto_of(addr) == "tcp")
        .filter_map(|addr| bind_of(addr).rsplit(':').next()?.parse().ok())
        .collect()
}

fn shared_quic_ports() -> Vec<u16> {
    let Some(state) = STATE.get() else { return Vec::new() };
    state.addrs.lock().unwrap().iter()
        .filter(|addr| proto_of(addr) == "quic")
        .filter_map(|addr| bind_of(addr).rsplit(':').next()?.parse().ok())
        .collect()
}

fn shared_udp_ports() -> Vec<u16> {
    let Some(state) = STATE.get() else { return Vec::new() };
    state.addrs.lock().unwrap().iter()
        .filter(|addr| proto_of(addr) == "udp")
        .filter_map(|addr| bind_of(addr).rsplit(':').next()?.parse().ok())
        .collect()
}

fn advertise_ip() -> Option<String> {
    advertise_host()
}

/// Starts the shared listeners using `DOKITIER_SHARED_LISTENERS` /
/// `DOKITIER_SHARED_LISTENER`. Returns the first public address when enabled.
pub fn addr() -> Option<String> {
    STATE.get().and_then(|state| state.addrs.lock().unwrap().first().cloned())
}

pub fn addr_list() -> Vec<String> {
    STATE.get().map(|state| state.addrs.lock().unwrap().clone()).unwrap_or_default()
}

fn state() -> &'static SharedState {
    STATE.get_or_init(|| SharedState {
        addrs: Mutex::new(Vec::new()),
        ports: Mutex::new(HashMap::new()),
        udp_ports: Mutex::new(HashMap::new()),
        ws_ports: Mutex::new(HashMap::new()),
        quic_ports: Mutex::new(HashMap::new()),
        udp_sessions: Mutex::new(HashMap::new()),
        tasks: Mutex::new(Vec::new()),
        session_tasks: Mutex::new(Vec::new()),
        shared_udp: Mutex::new(HashMap::new()),
        announce: Mutex::new(HashMap::new()),
        udp_networks: Mutex::new(std::collections::HashSet::new()),
    })
}

fn teardown_shared_tasks() {
    if let Some(state) = STATE.get() {
        if let Ok(mut tasks) = state.tasks.lock() {
            for task in tasks.drain(..) { task.abort(); }
        }
        if let Ok(mut tasks) = state.session_tasks.lock() {
            for task in tasks.drain(..) { task.abort(); }
        }
        if let Ok(mut sessions) = state.udp_sessions.lock() {
            for (_, session) in sessions.drain() {
                session.alive.store(false, Ordering::Relaxed);
            }
        }
        if let Ok(mut announces) = state.announce.lock() {
            announces.clear();
        }
        if let Ok(mut channels) = state.shared_udp.lock() {
            channels.clear();
        }
    }
}

async fn bind_and_run(addrs: Vec<String>) -> anyhow::Result<Vec<String>> {
    let mut bound = Vec::new();
    let mut tasks: Vec<tokio::task::JoinHandle<()>> = Vec::new();
    let mut ws_addrs: Vec<String> = Vec::new();
    let mut quic_addrs: Vec<String> = Vec::new();
    for addr in &addrs {
        let normalized = normalize(addr);
        match proto_of(&normalized) {
            "ws" | "wss" => {
                ws_addrs.push(normalized);
            }
            "quic" => {
                quic_addrs.push(normalized);
            }
            "tcp" => {
                let listener = match TcpListener::bind(bind_of(&normalized)).await {
                    Ok(listener) => listener,
                    Err(err) => {
                        for task in tasks.drain(..) { task.abort(); }
                        return Err(anyhow::anyhow!("bind {normalized}: {err}"));
                    }
                };
                let actual = listener.local_addr().map(|addr| format!("tcp://{}", addr)).unwrap_or_else(|_| normalized.clone());
                tasks.push(tokio::spawn(accept_loop(listener)));
                bound.push(actual);
            }
            "udp" => {
                let socket = match UdpSocket::bind(bind_of(&normalized)).await {
                    Ok(socket) => socket,
                    Err(err) => {
                        for task in tasks.drain(..) { task.abort(); }
                        return Err(anyhow::anyhow!("bind {normalized}: {err}"));
                    }
                };
                let actual = socket.local_addr().map(|addr| format!("udp://{}", addr)).unwrap_or_else(|_| normalized.clone());
                let socket = Arc::new(socket);
                tasks.push(tokio::spawn(udp_accept_loop(socket.clone())));
                tasks.push(tokio::spawn(udp_sweep_loop()));
                bound.push(actual);
            }
            other => {
                tracing::warn!(%normalized, proto = other, "shared listener protocol not implemented yet; skipped");
            }
        }
    }
    #[cfg(feature = "websocket")]
    if !ws_addrs.is_empty() {
        let resolver: crate::shared_listener_ws::NetworkResolver =
            Arc::new(|name: &str| {
                let port = ws_port_for(name);
                if port == 0 { None } else { Some(port) }
            });
        match crate::shared_listener_ws::bind(ws_addrs, resolver).await {
            Ok((ws_bound, mut ws_tasks)) => {
                bound.extend(ws_bound);
                tasks.append(&mut ws_tasks);
            }
            Err(err) => {
                for task in tasks.drain(..) { task.abort(); }
                return Err(err);
            }
        }
    }
    #[cfg(not(feature = "websocket"))]
    if !ws_addrs.is_empty() {
        tracing::warn!("websocket feature disabled; shared ws listeners skipped");
    }
    #[cfg(feature = "quic")]
    if !quic_addrs.is_empty() {
        let resolver: Arc<dyn Fn(&str) -> Option<u16> + Send + Sync> = Arc::new(|name: &str| {
            let port = quic_port_for(name);
            if port == 0 { None } else { Some(port) }
        });
        match crate::shared_listener_quic::bind(quic_addrs, resolver).await {
            Ok((quic_bound, mut quic_tasks)) => {
                bound.extend(quic_bound);
                tasks.append(&mut quic_tasks);
            }
            Err(err) => {
                for task in tasks.drain(..) { task.abort(); }
                return Err(err);
            }
        }
    }
    #[cfg(not(feature = "quic"))]
    if !quic_addrs.is_empty() {
        tracing::warn!("quic feature disabled; shared quic listeners skipped");
    }
    teardown_shared_tasks();
    let mut current = state().tasks.lock().unwrap();
    *current = tasks;
    *state().addrs.lock().unwrap() = bound.clone();
    Ok(bound)
}

pub async fn start(configured: Vec<String>) -> Option<String> {
    let mut addrs: Vec<String> = configured.into_iter().map(|v| v.trim().to_string()).filter(|v| !v.is_empty()).collect();
    if addrs.is_empty() {
        addrs = configured_addrs();
    }
    if addrs.is_empty() {
        return None;
    }
    match bind_and_run(addrs).await {
        Ok(bound) => {
            eprintln!("shared listeners started: {bound:?}");
            tracing::info!(?bound, "core shared listeners started");
            bound.into_iter().next()
        }
        Err(err) => {
            eprintln!("shared listener bind failed: {err:#}");
            tracing::error!(?err, "shared listener bind failed");
            None
        }
    }
}

/// Hot-applies new shared listener addresses without restarting core.
pub async fn apply(addrs: Vec<String>) -> anyhow::Result<Vec<String>> {
    let addrs: Vec<String> = addrs.into_iter().map(|v| v.trim().to_string()).filter(|v| !v.is_empty()).collect();
    let old_addrs = addr_list();
    if addrs == old_addrs {
        // Idempotent hot apply: avoid rebinding the same sockets (EADDRINUSE).
        return Ok(old_addrs);
    }
    teardown_shared_tasks();
    state().addrs.lock().unwrap().clear();
    if addrs.is_empty() {
        return Ok(Vec::new());
    }
    match bind_and_run(addrs).await {
        Ok(bound) => {
            eprintln!("shared listeners hot updated: {bound:?}");
            tracing::info!(?bound, "shared listeners hot updated");
            Ok(bound)
        }
        Err(err) => {
            if !old_addrs.is_empty() {
                let _ = bind_and_run(old_addrs).await;
            }
            Err(err)
        }
    }
}

fn tcp_port_for(network: &str) -> u16 {
    let Some(state) = STATE.get() else { return 0 };
    let mut ports = state.ports.lock().unwrap();
    if let Some(port) = ports.get(network) {
        return *port;
    }
    let hash = network.bytes().fold(5381u32, |acc, b| acc.wrapping_mul(33).wrapping_add(b as u32));
    for offset in 0..4000u16 {
        let port = 33000u16.wrapping_add((hash as u16).wrapping_add(offset) % 4000);
        if port < 1024 {
            continue;
        }
        if std::net::TcpListener::bind(("127.0.0.1", port)).is_err() {
            continue;
        }
        ports.insert(network.to_string(), port);
        return port;
    }
    0
}

fn udp_port_for(network: &str) -> u16 {
    let Some(state) = STATE.get() else { return 0 };
    let mut ports = state.udp_ports.lock().unwrap();
    if let Some(port) = ports.get(network) {
        return *port;
    }
    let hash = network.bytes().fold(5381u32, |acc, b| acc.wrapping_mul(33).wrapping_add(b as u32));
    for offset in 0..4000u16 {
        let port = 37000u16.wrapping_add((hash as u16).wrapping_add(offset) % 4000);
        if port < 1024 {
            continue;
        }
        let Ok(socket) = std::net::UdpSocket::bind(("127.0.0.1", port)) else { continue };
        drop(socket);
        ports.insert(network.to_string(), port);
        return port;
    }
    0
}

fn quic_port_for(network: &str) -> u16 {
    let Some(state) = STATE.get() else { return 0 };
    let mut ports = state.quic_ports.lock().unwrap();
    if let Some(port) = ports.get(network) {
        return *port;
    }
    let hash = network.bytes().fold(5381u32, |acc, b| acc.wrapping_mul(33).wrapping_add(b as u32));
    for offset in 0..4000u16 {
        let port = 45000u16.wrapping_add((hash as u16).wrapping_add(offset) % 4000);
        if port < 1024 { continue; }
        if std::net::UdpSocket::bind(("127.0.0.1", port)).is_err() { continue; }
        ports.insert(network.to_string(), port);
        return port;
    }
    0
}

fn ws_port_for(network: &str) -> u16 {
    let Some(state) = STATE.get() else { return 0 };
    let mut ports = state.ws_ports.lock().unwrap();
    if let Some(port) = ports.get(network) {
        return *port;
    }
    let hash = network.bytes().fold(5381u32, |acc, b| acc.wrapping_mul(33).wrapping_add(b as u32));
    for offset in 0..4000u16 {
        let port = 41000u16.wrapping_add((hash as u16).wrapping_add(offset) % 4000);
        if port < 1024 { continue; }
        if std::net::TcpListener::bind(("127.0.0.1", port)).is_err() { continue; }
        ports.insert(network.to_string(), port);
        return port;
    }
    0
}

/// Removes listener fields before persisting a network config. Listeners are a
/// core-level concern now; the shared listener injects them at runtime only.
pub fn strip_listeners(toml_text: &str) -> String {
    let Ok(mut value) = toml::from_str::<toml::Value>(toml_text) else { return toml_text.to_string() };
    let Some(table) = value.as_table_mut() else { return toml_text.to_string() };
    table.remove("listeners");
    table.remove("mapped_listeners");
    toml::to_string(&value).unwrap_or_else(|_| toml_text.to_string())
}

pub fn rewrite_listeners(toml_text: &str, network: &str) -> String {
    if STATE.get().is_none() {
        return toml_text.to_string();
    }
    let has_tcp = !shared_ports().is_empty();
    let has_udp = !shared_udp_ports().is_empty();
    let has_ws = STATE.get().map(|state| state.addrs.lock().unwrap().iter().any(|addr| matches!(proto_of(addr), "ws" | "wss"))).unwrap_or(false);
    let has_quic = !shared_quic_ports().is_empty();
    if !has_tcp && !has_udp && !has_ws && !has_quic {
        return toml_text.to_string();
    }
    let Ok(mut value) = toml::from_str::<toml::Value>(toml_text) else { return toml_text.to_string() };
    let Some(table) = value.as_table_mut() else { return toml_text.to_string() };
    let mut listeners = Vec::new();
    if has_tcp {
        let port = tcp_port_for(network);
        if port != 0 {
            listeners.push(toml::Value::String(format!("tcp://127.0.0.1:{port}")));
        }
    }
    if has_udp {
        state().udp_networks.lock().unwrap().insert(network.to_string());
        let port = udp_port_for(network);
        if port != 0 {
            listeners.push(toml::Value::String(format!("udp://127.0.0.1:{port}")));
        }
    }
    if has_ws {
        let port = ws_port_for(network);
        if port != 0 {
            listeners.push(toml::Value::String(format!("ws://127.0.0.1:{port}")));
        }
    }
    if has_quic {
        let port = quic_port_for(network);
        if port != 0 {
            listeners.push(toml::Value::String(format!("quic://127.0.0.1:{port}")));
        }
    }
    if listeners.is_empty() {
        return toml_text.to_string();
    }
    table.insert("listeners".to_string(), toml::Value::Array(listeners));
    toml::to_string(&value).unwrap_or_else(|_| toml_text.to_string())
}

/// Public URLs of the shared listeners, using the advertised host.
///
/// This is runtime-only state (the instance's `mapped_listeners` are no longer
/// written into any config file): peer discovery advertises these addresses so
/// peers can dial the public shared ports instead of the instance's loopback
/// listeners.
pub fn advertise_urls() -> Vec<url::Url> {
    let Some(ip) = advertise_ip() else {
        return Vec::new();
    };
    let mut urls = Vec::new();
    let mut push = |scheme: &str, port: u16| {
        if let Ok(url) = url::Url::parse(&format!("{scheme}://{}", url_host_port(&ip, port))) {
            urls.push(url);
        }
    };
    for port in shared_ports() {
        push("tcp", port);
    }
    for port in shared_udp_ports() {
        push("udp", port);
    }
    for port in shared_quic_ports() {
        push("quic", port);
    }
    if let Some(state) = STATE.get() {
        for addr in state.addrs.lock().unwrap().iter() {
            let proto = proto_of(addr);
            if matches!(proto, "ws" | "wss")
                && let Some(port) = bind_of(addr).rsplit(':').next().and_then(|p| p.parse::<u16>().ok())
            {
                push(proto, port);
            }
        }
    }
    urls
}

async fn read_first_frame(stream: &mut TcpStream) -> std::io::Result<(Vec<u8>, String)> {
    let mut header = [0u8; 4];
    timeout_read(stream, &mut header).await?;
    let len = u32::from_le_bytes(header) as usize;
    if len < PEER_MANAGER_HEADER_SIZE || len > MAX_FIRST_FRAME {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "invalid first frame length"));
    }
    let mut body = vec![0u8; len];
    timeout_read(stream, &mut body).await?;
    let payload = body.get(PEER_MANAGER_HEADER_SIZE..).unwrap_or_default();
    let request = HandshakeRequest::decode(payload)
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "invalid handshake request"))?;
    let mut frame = Vec::with_capacity(header.len() + body.len());
    frame.extend_from_slice(&header);
    frame.extend_from_slice(&body);
    Ok((frame, request.network_name))
}

#[allow(dead_code)]
fn network_from_udp(buf: &[u8]) -> Option<String> {
    let mut packet = ZCPacket::new_from_buf(BytesMut::from(buf), ZCPacketType::UDP);
    let header = packet.peer_manager_header()?;
    if header.packet_type != PacketType::HandShake as u8 {
        return None;
    }
    let request = HandshakeRequest::decode(packet.payload()).ok()?;
    Some(request.network_name)
}

async fn timeout_read(stream: &mut TcpStream, buf: &mut [u8]) -> std::io::Result<()> {
    tokio::time::timeout(Duration::from_secs(FIRST_FRAME_TIMEOUT_SECS), stream.read_exact(buf))
        .await
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "first frame timeout"))?
        .map(|_| ())
}

async fn accept_loop(listener: TcpListener) {
    loop {
        let (mut client, peer) = match listener.accept().await {
            Ok(value) => value,
            Err(err) => {
                tracing::warn!(?err, "shared listener accept failed");
                tokio::time::sleep(Duration::from_millis(200)).await;
                continue;
            }
        };
        tokio::spawn(async move {
            TCP_ACCEPTED.fetch_add(1, Ordering::Relaxed);
            // Fast path: peek (do not consume) the handshake and hand the
            // accepted socket directly to the instance peer manager. This
            // removes the loopback relay hop and its throughput cost.
            if let Ok(Some(network)) = crate::shared_listener_handoff::peek_handshake_network(&client).await {
                if let Some(peer_manager) = crate::shared_listener_handoff::resolve_network(&network) {
                    if let Ok(local_addr) = client.local_addr() {
                        if let Ok(local_url) = url::Url::parse(&format!("tcp://{local_addr}")) {
                            match crate::shared_listener_handoff::handoff_tcp(&peer_manager, client, local_url).await {
                                Ok(()) => {
                                    TCP_HANDOFF.fetch_add(1, Ordering::Relaxed);
                                    return;
                                }
                                Err(err) => {
                                    SHARED_ERRORS.fetch_add(1, Ordering::Relaxed);
                                    tracing::warn!(%network, ?err, "shared tcp handoff failed");
                                    return;
                                }
                            }
                        }
                    }
                }
            }
            let (frame, network) = match read_first_frame(&mut client).await {
                Ok(value) => value,
                Err(err) => {
                    tracing::debug!(%peer, ?err, "shared listener dropped connection before handshake");
                    return;
                }
            };
            let Some(state) = STATE.get() else { return };
            let port = state.ports.lock().unwrap().get(&network).copied();
            let Some(port) = port else {
                tracing::warn!(%peer, %network, "shared listener has no instance for network");
                return;
            };
            let mut upstream = match TcpStream::connect(("127.0.0.1", port)).await {
                Ok(stream) => stream,
                Err(err) => {
                    tracing::warn!(%network, %port, ?err, "shared listener internal connect failed");
                    return;
                }
            };
            if upstream.write_all(&frame).await.is_err() {
                return;
            }
            TCP_RELAY.fetch_add(1, Ordering::Relaxed);
            if let Err(err) = tokio::io::copy_bidirectional(&mut client, &mut upstream).await {
                tracing::debug!(%peer, %network, ?err, "shared listener relay ended");
            }
        });
    }
}

static SHARED_UDP_RX: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static SHARED_UDP_TX: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static SHARED_UDP_ROUTED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static SHARED_UDP_DROPPED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static SHARED_UDP_NO_ANNOUNCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static SHARED_UDP_FULL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

struct SharedUdpSocket {
    local_addr: std::net::SocketAddr,
    sender: Arc<UdpSocket>,
    inbound: tokio::sync::Mutex<tokio::sync::mpsc::Receiver<(Vec<u8>, std::net::SocketAddr)>>,
}

#[async_trait::async_trait]
impl VirtualUdpSocket for SharedUdpSocket {
    fn local_addr(&self) -> std::io::Result<std::net::SocketAddr> {
        Ok(self.local_addr)
    }

    async fn send_to(&self, data: &[u8], addr: std::net::SocketAddr) -> std::io::Result<usize> {
        let result = self.sender.send_to(data, addr).await;
        if result.is_ok() {
            SHARED_UDP_TX.fetch_add(1, Ordering::Relaxed);
        }
        result
    }

    async fn recv_from(&self, buf: &mut [u8]) -> std::io::Result<(usize, std::net::SocketAddr)> {
        let mut inbound = self.inbound.lock().await;
        let (data, addr) = inbound
            .recv()
            .await
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::BrokenPipe, "shared udp closed"))?;
        let len = data.len().min(buf.len());
        buf[..len].copy_from_slice(&data[..len]);
        SHARED_UDP_RX.fetch_add(1, Ordering::Relaxed);
        Ok((len, addr))
    }

    async fn send_to_with_meta(
        &self,
        data: &[u8],
        addr: std::net::SocketAddr,
        _meta: UdpSocketSendMeta,
    ) -> std::io::Result<usize> {
        self.send_to(data, addr).await
    }

    async fn recv_from_with_meta(
        &self,
        buf: &mut [u8],
    ) -> std::io::Result<(usize, std::net::SocketAddr, UdpSocketRecvMeta)> {
        let (len, addr) = self.recv_from(buf).await?;
        Ok((len, addr, UdpSocketRecvMeta::default()))
    }
}

async fn ensure_shared_udp_network(socket: &Arc<UdpSocket>, network: &str) {
    let network = network.to_string();
    {
        let channels = state().shared_udp.lock().unwrap();
        if channels.contains_key(&network) {
            return;
        }
    }
    tracing::info!(%network, "shared udp ensure layer");
    // The layer is instance-agnostic (it only demuxes UDP mux sessions), but
    // the admission target must NOT be cached here: instances restart on every
    // config re-apply, and a cached `PeerManagerCore` would keep admitting
    // tunnels into the zombie instance.
    if crate::shared_listener_handoff::resolve_network(&network).is_none() {
        tracing::warn!(%network, "shared udp no instance for network");
        return;
    }
    let local_addr = match socket.local_addr() {
        Ok(addr) => addr,
        Err(_) => return,
    };
    let (tx, rx) = tokio::sync::mpsc::channel(1024);
    let shared_socket = Arc::new(SharedUdpSocket {
        local_addr,
        sender: socket.clone(),
        inbound: tokio::sync::Mutex::new(rx),
    });
    let layer = Arc::new(UdpSessionLayer::new(shared_socket));
    {
        let mut channels = state().shared_udp.lock().unwrap();
        if channels.contains_key(&network) {
            return;
        }
        channels.insert(network.clone(), tx);
    }
    let local_url = url::Url::parse(&format!("udp://{local_addr}")).ok();
    state().session_tasks.lock().unwrap().push(tokio::spawn(async move {
        loop {
            let session = match layer.accept().await {
                Ok(session) => session,
                Err(err) => {
                    tracing::warn!(%network, ?err, "shared udp accept loop ended");
                    break;
                }
            };
            let peer = session.peer_socket_addr();
            // Resolve the admission target per session: instances restart on
            // config re-apply, and the registry always maps a network to the
            // currently live instance's peer manager.
            let Some(peer_manager) = crate::shared_listener_handoff::resolve_network(&network)
            else {
                tracing::warn!(%network, %peer, "shared udp no live instance, dropping session");
                drop(session);
                continue;
            };
            let remote_url = url::Url::parse(&format!("udp://{peer}")).ok();
            let info = TunnelInfo {
                tunnel_type: "udp".to_string(),
                local_addr: local_url.clone().map(Into::into),
                remote_addr: remote_url.clone().map(Into::into),
                // Match the TCP/QUIC handoff: the resolved remote address must
                // be the real client address so admission, hole-punch
                // cooperation and edge reporting all see a usable peer.
                resolved_remote_addr: remote_url.clone().map(Into::into),
            };
            let tunnel = match UdpTunnelUpgrader::with_keep_alive(info, layer.clone())
                .upgrade(session)
            {
                Ok(tunnel) => tunnel,
                Err(err) => {
                    tracing::warn!(%network, %peer, ?err, "shared udp tunnel upgrade failed");
                    continue;
                }
            };
            tracing::info!(%network, %peer, "shared udp admitting tunnel as server");
            // Admit concurrently: `add_tunnel_as_server` awaits the full noise
            // handshake. A serialized accept loop would let the next accepted
            // session's 128-slot ring fill up (dropping handshake frames)
            // while the previous handshake is still in flight.
            let network = network.clone();
            tokio::spawn(async move {
                if let Err(err) = peer_manager.add_tunnel_as_server(tunnel).await {
                    // A failed admission must not tear down the accept loop:
                    // dropping the layer would close every shared UDP session
                    // of this network.
                    tracing::warn!(%network, %peer, ?err, "shared udp admit failed");
                }
            });
        }
    }));
}

fn udp_header(buf: &[u8]) -> Option<(u32, u8)> {
    let packet = ZCPacket::new_from_buf(BytesMut::from(buf), ZCPacketType::UDP);
    let header = packet.udp_tunnel_header()?;
    Some((header.conn_id.get(), header.msg_type))
}

fn udp_payload(buf: &[u8]) -> Option<(u32, u8, Vec<u8>)> {
    let packet = ZCPacket::new_from_buf(BytesMut::from(buf), ZCPacketType::UDP);
    let (conn_id, msg_type) = {
        let header = packet.udp_tunnel_header()?;
        (header.conn_id.get(), header.msg_type)
    };
    let payload = packet.udp_payload_bytes().to_vec();
    Some((conn_id, msg_type, payload))
}

fn build_udp_packet(conn_id: u32, msg_type: u8, payload: &[u8]) -> Vec<u8> {
    let mut buf = BytesMut::new();
    buf.resize(UDP_TUNNEL_HEADER_SIZE + payload.len(), 0);
    buf[UDP_TUNNEL_HEADER_SIZE..].copy_from_slice(payload);
    let mut packet = ZCPacket::new_from_buf(buf, ZCPacketType::UDP);
    if let Some(header) = packet.mut_udp_tunnel_header() {
        header.msg_type = msg_type;
        header.conn_id.set(conn_id);
        header.len.set(payload.len() as u16);
    }
    packet.into_bytes().to_vec()
}

fn handshake_network(payload: &[u8]) -> Option<String> {
    let packet = ZCPacket::new_from_buf(BytesMut::from(payload), ZCPacketType::DummyTunnel);
    let header = packet.peer_manager_header()?;
    if header.packet_type != PacketType::HandShake as u8 { return None; }
    HandshakeRequest::decode(packet.payload()).ok().map(|req| req.network_name)
}

/// UDP has no network name in its first datagram (Syn only carries conn_id and
/// random magic), so a shared port works by trying every network instance:
/// the Syn is sent to all internal ports, the instance that accepts replies
/// first owns the client, the remaining candidates are dropped.
async fn udp_accept_loop(socket: Arc<UdpSocket>) {
    let local_port = socket.local_addr().map(|addr| addr.port()).unwrap_or(0);
    let mut buf = vec![0u8; 65535];
    loop {
        let (len, client) = match socket.recv_from(&mut buf).await {
            Ok(value) => value,
            Err(err) => {
                tracing::debug!(?err, "shared udp listener recv error");
                continue;
            }
        };
        let diag = UDP_DATAGRAMS.fetch_add(1, Ordering::Relaxed);
        if diag < 20 {
            tracing::debug!(%client, len, datagram = diag, "shared udp datagram");
        }
        let datagram = &buf[..len];
        let Some((conn_id, msg_type, payload)) = udp_payload(datagram) else { continue };
        if msg_type == dokitier_core::packet::UdpPacketType::NetworkAnnounce as u8 {
            if let Ok(network) = std::str::from_utf8(&payload) {
                if !network.is_empty() {
                    tracing::info!(%client, %network, "shared udp announce");
                    ensure_shared_udp_network(&socket, network).await;
                    state().announce.lock().unwrap().insert(client, (network.to_string(), Instant::now()));
                }
            }
            continue;
        }

        let announce = state().announce.lock().unwrap().get(&client).cloned();
        let announced = announce.map(|(network, at)| (network, at));
        let network = match announced {
            Some((network, at)) => {
                if at.elapsed() >= Duration::from_secs(180) {
                    state().announce.lock().unwrap().remove(&client);
                    continue;
                }
                network
            }
            None => {
                let n = SHARED_UDP_NO_ANNOUNCE.fetch_add(1, Ordering::Relaxed);
                if n < 20 {
                    tracing::debug!(%client, dropped = n, networks = ?state().udp_networks.lock().unwrap(), "shared udp no announce");
                }
                // Single-network compatibility: if this core exposes exactly
                // one UDP-shared network, it is unambiguous who owns the flow.
                let networks: Vec<String> = state().udp_networks.lock().unwrap().iter().cloned().collect();
                if networks.len() != 1 { continue; }
                let network = networks[0].clone();
                ensure_shared_udp_network(&socket, &network).await;
                network
            }
        };
        let channels = state().shared_udp.lock().unwrap();
        if let Some(tx) = channels.get(&network) {
            match tx.try_send((datagram.to_vec(), client)) {
                Ok(()) => {
                    SHARED_UDP_ROUTED.fetch_add(1, Ordering::Relaxed);
                }
                Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                    SHARED_UDP_FULL.fetch_add(1, Ordering::Relaxed);
                }
                Err(_) => {
                    SHARED_UDP_DROPPED.fetch_add(1, Ordering::Relaxed);
                }
            }
        } else {
            SHARED_UDP_DROPPED.fetch_add(1, Ordering::Relaxed);
        }
    }
}

fn rand_u32() -> u32 {
    use std::hash::{BuildHasher, Hasher};
    std::collections::hash_map::RandomState::new().build_hasher().finish() as u32
}

fn rand_u64() -> u64 {
    ((rand_u32() as u64) << 32) | rand_u32() as u64
}

async fn udp_sweep_loop() {
    loop {
        tokio::time::sleep(Duration::from_secs(60)).await;
        let Some(state) = STATE.get() else { continue };
        let mut sessions = state.udp_sessions.lock().unwrap();
        sessions.retain(|_, session| {
            let idle = session.last.lock().map(|last| last.elapsed() > UDP_SESSION_IDLE).unwrap_or(true);
            if idle {
                session.alive.store(false, Ordering::Relaxed);
            }
            !idle
        });
    }
}
