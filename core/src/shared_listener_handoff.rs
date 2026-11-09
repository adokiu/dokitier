//! Single-hop ("direct handoff") path for the core-level shared listeners.
//!
//! # 中文摘要
//!
//! 当前 `shared_listener*` 是"公开端口 accept → 读首帧 → 再连 `127.0.0.1`
//! 内部实例 → 双向搬运"的双跳结构。本文件给出单跳（零额外 hop）方案：
//! accept 后**不消费**握手帧（TCP 用 `peek`），仅解析出 `network_name`，
//! 然后把**原始 socket 直接**交给目标实例的 [`PeerManagerCore`]：
//! `PeerManagerCore::add_tunnel_as_server()` 会执行和实例 listener 完全相同的
//! 握手/鉴权/准入流程，数据面从客户端到实例只剩一跳。
//!
//! 本文件只新增代码，不修改 `shared_listener*.rs`；接线方式见
//! [Wiring](#wiring-out-of-scope-of-this-file) 一节（lib.rs 一行 + 三个
//! listener 各约 10 行 + controller_agent 安装 resolver 约 6 行）。
//!
//! # Current path (double hop)
//!
//! * TCP — `core/src/shared_listener.rs::accept_loop`: accept → `read_first_frame`
//!   (consumes the handshake) → `TcpStream::connect(("127.0.0.1", internal_port))`
//!   → `write_all(frame)` → `copy_bidirectional`. Every direction of every chunk
//!   costs one extra user-space copy, one extra syscall pair, one extra task
//!   wakeup, plus a loopback TCP connection per client.
//! * WS — `core/src/shared_listener_ws.rs::handle_session`: HTTP/WS upgrade →
//!   read first binary message → dial `ws://127.0.0.1:<port>` (a **second**
//!   HTTP upgrade) → replay first message → two `pump` tasks. The instance sees
//!   `127.0.0.1` as its peer and every message crosses two extra task queues.
//! * QUIC — `core/src/shared_listener_quic.rs::handle_connection`: accept public
//!   QUIC connection → `accept_bi` → read first frame → dial a **second** QUIC
//!   connection from `loopback_client_endpoint()` to the instance → replay frame
//!   → relay. That is a full extra QUIC handshake (custom EasyTier crypto,
//!   ~1 loopback RTT + packet crypto) *and* double crypto/congestion control on
//!   the data plane.
//! * UDP — `core/src/shared_listener.rs::udp_accept_loop`: terminates the client
//!   mux itself (answers `Syn` with `Sack`), then binds a *new* upstream
//!   `UdpSocket` per session, allocates a new `conn_id`, waits for upstream
//!   `Sack`, and relays. UDP Syn is broadcast to every internal port
//!   ("first reply wins"), so cost is O(number of networks) per handshake.
//!
//! # Target path (single hop)
//!
//! ```text
//! client ──(public socket, unchanged, peeked only)──> PeerManagerCore::add_tunnel_as_server
//! ```
//!
//! The seam already exists and is public; see
//! `crates/dokitier-core/src/instance/mod.rs` (`CoreInstance::peer_manager`,
//! documented exactly as *"Returns the peer manager for tunnels accepted
//! outside this instance's regular listener path, such as the shared Web
//! listener"*), `crates/dokitier-core/src/peers/peer_manager.rs`
//! (`PeerManagerCore::add_tunnel_as_server`) and
//! `crates/dokitier-core/src/tunnel/tcp.rs` (`TcpTunnelUpgrader`). No new
//! instance-side API is required for TCP/WS/QUIC.
//!
//! ## TCP: peek, do not consume
//!
//! `tokio::net::TcpStream::peek` reads the first EasyTier frame **without
//! removing it from the receive queue**, so the handoff keeps byte-for-byte
//! fidelity: the instance parses the very same `HandshakeRequest`, validates
//! magic/version/secret digest itself and sees the real public peer address in
//! `peer_addr()` (the legacy relay always showed `127.0.0.1`). A `peek` that
//! cannot parse a full frame simply returns `None` and the caller runs the old
//! relay path with the same untouched stream, so this path is loss-free.
//!
//! ## WS: replay the already-consumed first message
//!
//! The WS listener has to *consume* the first binary message to learn the
//! network name. [`ws_tunnel_from_accepted`] takes the same upgraded
//! `WebSocketStream` plus that first message and prepends it to the tunnel
//! stream; the rest of the messages are forwarded as `ZCPacket`s with
//! `ZCPacketType::DummyTunnel` (identical to `core/src/tunnel/websocket.rs`).
//!
//! ## QUIC: hand over the live connection, not a new one
//!
//! [`quic_tunnel_from_accepted`] wraps the *already accepted* `quinn::Connection`
//! and its first bidirectional stream into the same `FramedReader`/`FramedWriter`
//! layout that `core/src/tunnel/quic.rs` uses for a listener, and prepends the
//! already-consumed first frame. The public `quinn::Endpoint` is kept alive by
//! [`QuicConnectionKeepAlive`] so the connection is not torn down with the
//! endpoint.
//!
//! ## UDP: needs new APIs (design only)
//!
//! [`UDP`] cannot be handed over with today's public API because
//! `UdpSession` has private fields and no public constructor from an externally
//! owned datagram source (`crates/dokitier-core/src/socket/udp/session.rs`).
//! [`REQUIRED_UDP_HANDOFF_APIS`] lists the exact signatures we would add. Until
//! then the UDP path must stay on the legacy relay. Note that the legacy UDP
//! path has an additional correctness problem documented in
//! `core/tests/udp_shared_probe.rs`: after the first datagram, routing is by
//! `conn_id` only, and `network_name` is not used at all.
//!
//! # Wiring (out of scope of this file)
//!
//! 1. `core/src/lib.rs`: add `pub mod shared_listener_handoff;` (one line; the
//!    only compilation prerequisite of this file).
//! 2. `core/src/controller_agent.rs::spawn`, right after
//!    `crate::shared_listener::start(...)`:
//!
//!    ```ignore
//!    let handoff_manager = manager.clone();
//!    crate::shared_listener_handoff::install_network_resolver(std::sync::Arc::new(
//!        move |network: &str| {
//!            handoff_manager
//!                .instances()
//!                .into_iter()
//!                .find(|instance| {
//!                    instance.peer_manager().network_identity().network_name == network
//!                })
//!                .map(|instance| instance.peer_manager().clone())
//!        },
//!    ));
//!    ```
//!
//!    This pull-style resolver needs no create/delete bookkeeping and is always
//!    consistent with `NativeInstanceManager::instances()`.
//!
//! 3. `core/src/shared_listener.rs::accept_loop` — before `read_first_frame`:
//!
//!    ```ignore
//!    if let Ok(Some(network)) =
//!        crate::shared_listener_handoff::peek_handshake_network(&client).await
//!        && let Some(peer_manager) =
//!            crate::shared_listener_handoff::resolve_network(&network)
//!    {
//!        let local_url = /* tcp://<public listener addr> */;
//!        if crate::shared_listener_handoff::handoff_tcp(&peer_manager, client, local_url)
//!            .await
//!            .is_ok()
//!        {
//!            return;
//!        }
//!        return; // handoff consumed the stream; nothing to relay any more
//!    }
//!    // ... unchanged legacy read_first_frame + loopback relay ...
//!    ```
//!
//! 4. `core/src/shared_listener_ws.rs::handle_session` — after the first message
//!    was decoded and `network` resolved, before `connect_loopback`:
//!
//!    ```ignore
//!    if let Some(peer_manager) = crate::shared_listener_handoff::resolve_network(&network) {
//!        let remote_url = /* ws://<peer> */;
//!        if crate::shared_listener_handoff::handoff_ws(
//!            &peer_manager, client, first, local_url, Some(remote_url),
//!        )
//!        .await
//!        .is_ok()
//!        {
//!            return Ok(());
//!        }
//!        return Ok(()); // the stream was consumed by the handoff attempt
//!    }
//!    // ... unchanged loopback replay/relay ...
//!    ```
//!
//! 5. `core/src/shared_listener_quic.rs` — in `handle_connection`, after
//!    `read_first_frame`, before creating the loopback client connection:
//!
//!    ```ignore
//!    if let Some(network) =
//!        crate::shared_listener_handoff::quic_handshake_network(&frame)
//!        && let Some(peer_manager) =
//!            crate::shared_listener_handoff::resolve_network(&network)
//!    {
//!        let local_url = /* quic://<public listener addr> */;
//!        let remote_url = /* quic://<client.remote_address()> */;
//!        if crate::shared_listener_handoff::handoff_quic(
//!            &peer_manager,
//!            client,
//!            client_send,
//!            client_recv,
//!            frame,
//!            local_url,
//!            remote_url,
//!            endpoint.clone(),
//!        )
//!        .await
//!        .is_ok()
//!        {
//!            return;
//!        }
//!        return;
//!    }
//!    // ... unchanged loopback relay ...
//!    ```
//!
//! # Risks
//!
//! * **Peer address changes from `127.0.0.1` to the real client address.** This
//!   is what the original EasyTier listener does and it improves observed-address
//!   quality (hole punching / NAT type reporting). `add_tunnel_as_server` runs
//!   `check_resolved_remote_addr_not_from_virtual_network`, which rejects a peer
//!   whose *source* address is inside the virtual network; that check is a no-op
//!   for the legacy loopback relay. Keep the legacy relay as the fallback and
//!   watch for "tunnel src is from the same network" errors.
//! * **`peek` semantics.** `peek` cannot wait for *more* than the currently
//!   buffered bytes, so the helper retries with a 1 ms backoff under the same
//!   8 s first-frame budget as the legacy listener. If a platform does not
//!   support `MSG_PEEK`, `peek` returns an error and the caller falls back to
//!   the legacy path (stream untouched).
//! * **No `CoreEvent::TunnelAccepted` / `TunnelAdmissionFailed`.** Those events
//!   are emitted by `crates/dokitier-core/src/peers/admission.rs` on the
//!   listener path only; a direct `add_tunnel_as_server` call does not emit
//!   them. Acceptable (diagnostics only), or emit them from the shared listener.
//! * **Handshake failure cannot fall back.** Once the tunnel is handed over the
//!   stream is consumed by the instance; a failure there means the connection is
//!   dropped, exactly like a failed handshake on a dedicated listener.
//! * **QUIC endpoint lifetime.** The `quinn::Endpoint` must outlive the
//!   connection; [`quic_tunnel_from_accepted`] stores a clone in
//!   [`QuicConnectionKeepAlive`]. Calling it with the *loopback* endpoint or
//!   dropping the public endpoint early would close live connections.
//! * **WS `wss://` is terminated in the shared listener.** The instance tunnel
//!   metadata keeps `tunnel_type = "wss"` but no TLS runs inside the instance,
//!   and forwarded-header (`Forwarded` / `X-Forwarded-For`) handling from
//!   `core/src/tunnel/websocket.rs::upgrade_accepted` does not run. Pass the
//!   real remote URL when calling [`handoff_ws`].
//! * **UDP is not covered.** See [`REQUIRED_UDP_HANDOFF_APIS`]; it needs new
//!   public constructors in `crates/dokitier-core`, which this task deliberately
//!   does not modify.
//!
//! # Performance expectation
//!
//! * TCP: removes 1 loopback TCP connection per client and 2 user-space copies +
//!   2 wakeups per chunk; same framing, same `PeerConn` data plane as a direct
//!   listener, so the remaining cost is the theoretical minimum.
//! * QUIC: removes an entire second QUIC handshake and one full
//!   encrypt/decrypt + congestion-control stack per direction; the largest win.
//! * WS: removes the second HTTP upgrade and one relay task per direction.
//! * `core/tests/handoff_latency.rs` measures the TCP hop cost
//!   (`direct handoff` vs `loopback relay`) and its throughput delta; see that
//!   file's module docs for how to run the ignored benchmarks.

#![allow(dead_code)]

use std::collections::HashMap;
use std::io;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use bytes::BytesMut;
use dokitier_core::packet::{
    PacketType, ZCPacket, ZCPacketType, PEER_MANAGER_HEADER_SIZE, TCP_TUNNEL_HEADER_SIZE,
};
use dokitier_core::peers::peer_manager::PeerManagerCore;
use dokitier_core::socket::tcp::{VirtualTcpSocket, VirtualTcpSplit};
use dokitier_core::tunnel::tcp::TcpTunnelUpgrader;
use dokitier_core::tunnel::{Tunnel, TunnelError};
use dokitier_proto::common::TunnelInfo;
use dokitier_proto::peer_rpc::HandshakeRequest;
use prost::Message as _;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;
use url::Url;

#[cfg(feature = "websocket")]
use bytes::Bytes;
#[cfg(feature = "websocket")]
use futures::StreamExt as _;
#[cfg(feature = "websocket")]
use tokio_websockets::WebSocketStream;

#[cfg(feature = "quic")]
use dokitier_core::tunnel::framed::{FramedReader, FramedWriter};
#[cfg(feature = "quic")]
use dokitier_core::tunnel::wrapper::TunnelWrapper;
#[cfg(feature = "quic")]
use quinn::{Connection, Endpoint, RecvStream, SendStream};

/// Same cap as `core/src/shared_listener*.rs`: the first EasyTier frame is a
/// handshake, never a data packet.
pub const MAX_FIRST_FRAME: usize = 64 * 1024;

/// Same budget as the legacy listeners for the first frame / message.
pub const FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(8);

/// `peek` returns whatever is already buffered; poll it again after this pause
/// until a full frame is available or [`FIRST_FRAME_TIMEOUT`] expires.
const PEEK_RETRY_INTERVAL: Duration = Duration::from_millis(1);

// ---------------------------------------------------------------------------
// Network -> peer manager registry
// ---------------------------------------------------------------------------

/// Resolves an EasyTier network name to the peer manager of the instance that
/// serves it in this process.
pub type NetworkResolver = Arc<dyn Fn(&str) -> Option<Arc<PeerManagerCore>> + Send + Sync>;

#[derive(Default)]
struct RegistryState {
    /// Pull-style resolver installed by the native composition root
    /// (`controller_agent::spawn`). Preferred: no create/delete bookkeeping.
    resolver: Option<NetworkResolver>,
    /// Push-style registrations, useful for embeddings/tests.
    instances: HashMap<String, Arc<PeerManagerCore>>,
}

fn registry() -> &'static Mutex<RegistryState> {
    static REGISTRY: OnceLock<Mutex<RegistryState>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(RegistryState::default()))
}

/// Installs the process-wide pull resolver. Returns `false` when a resolver was
/// already installed (it is never replaced silently).
pub fn install_network_resolver(resolver: NetworkResolver) -> bool {
    let mut state = registry().lock().unwrap();
    if state.resolver.is_some() {
        return false;
    }
    state.resolver = Some(resolver);
    true
}

/// Removes the installed pull resolver. Returns the removed resolver, if any.
pub fn clear_network_resolver() -> Option<NetworkResolver> {
    registry().lock().unwrap().resolver.take()
}

/// Push-registers one instance's peer manager for a network name.
pub fn register_instance(network_name: impl Into<String>, peer_manager: Arc<PeerManagerCore>) {
    registry()
        .lock()
        .unwrap()
        .instances
        .insert(network_name.into(), peer_manager);
}

/// Removes a push-registered instance.
pub fn unregister_instance(network_name: &str) {
    registry().lock().unwrap().instances.remove(network_name);
}

/// Resolves `network` to the peer manager that should accept the connection.
///
/// Push registrations win over the pull resolver so embeddings can override a
/// single network without replacing the process-wide resolver.
pub fn resolve_network(network: &str) -> Option<Arc<PeerManagerCore>> {
    let (resolver, direct) = {
        let state = registry().lock().unwrap();
        (state.resolver.clone(), state.instances.get(network).cloned())
    };
    direct.or_else(|| resolver.and_then(|resolve| resolve(network)))
}

/// Snapshot of the push-registered network names, for diagnostics.
pub fn registered_networks() -> Vec<String> {
    let mut networks: Vec<String> = registry().lock().unwrap().instances.keys().cloned().collect();
    networks.sort();
    networks
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Decodes the network name from a link-layer payload that starts with the
/// `PeerManagerHeader` (a WebSocket binary message body, or a TCP/QUIC frame
/// body). This is the same decode the legacy listeners perform.
pub fn handshake_network_from_tunnel_payload(payload: &[u8]) -> Option<String> {
    if payload.len() < PEER_MANAGER_HEADER_SIZE {
        return None;
    }
    let packet = ZCPacket::new_from_buf(BytesMut::from(payload), ZCPacketType::DummyTunnel);
    let header = packet.peer_manager_header()?;
    if header.packet_type != PacketType::HandShake as u8 {
        return None;
    }
    HandshakeRequest::decode(packet.payload())
        .ok()
        .map(|request| request.network_name)
}

/// Decodes the network name from one length-prefixed EasyTier TCP/QUIC frame
/// (`TCPTunnelHeader | PeerManagerHeader | HandshakeRequest`).
pub fn handshake_network_from_frame(frame: &[u8]) -> Option<String> {
    if frame.len() < TCP_TUNNEL_HEADER_SIZE + PEER_MANAGER_HEADER_SIZE {
        return None;
    }
    let body_len =
        u32::from_le_bytes(frame[..TCP_TUNNEL_HEADER_SIZE].try_into().ok()?) as usize;
    if !(PEER_MANAGER_HEADER_SIZE..=MAX_FIRST_FRAME).contains(&body_len) {
        return None;
    }
    let body_end = TCP_TUNNEL_HEADER_SIZE.checked_add(body_len)?;
    if frame.len() < body_end {
        return None;
    }
    handshake_network_from_tunnel_payload(&frame[TCP_TUNNEL_HEADER_SIZE..body_end])
}

fn socket_url(scheme: &str, addr: SocketAddr) -> Url {
    let mut url = Url::parse(&format!("{scheme}://0.0.0.0"))
        .expect("static transport URL should be valid");
    url.set_ip_host(addr.ip())
        .expect("socket IP should be a valid URL host");
    url.set_port(Some(addr.port()))
        .expect("transport URL should accept a port");
    url
}

// ---------------------------------------------------------------------------
// TCP: zero-copy single-hop handoff
// ---------------------------------------------------------------------------

/// Peeks (does **not** consume) the first EasyTier frame from `stream` and
/// returns the frame bytes.
///
/// Returns `Ok(None)` when the peer closed the connection, sent a malformed
/// frame, or did not complete a frame within [`FIRST_FRAME_TIMEOUT`]. In all
/// `None` cases the stream is untouched and the caller may run the legacy
/// `read_first_frame` + loopback relay path.
pub async fn peek_first_frame(stream: &TcpStream) -> io::Result<Option<Vec<u8>>> {
    let deadline = tokio::time::Instant::now() + FIRST_FRAME_TIMEOUT;
    let mut buf = vec![0u8; TCP_TUNNEL_HEADER_SIZE + MAX_FIRST_FRAME];
    loop {
        let read = match tokio::time::timeout_at(deadline, stream.peek(buf.as_mut_slice())).await {
            Ok(result) => result?,
            Err(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "timed out waiting for the first EasyTier frame",
                ));
            }
        };
        if read == 0 {
            return Ok(None);
        }
        if read >= TCP_TUNNEL_HEADER_SIZE {
            let body_len = u32::from_le_bytes(
                buf[..TCP_TUNNEL_HEADER_SIZE]
                    .try_into()
                    .expect("slice is exactly the tunnel header size"),
            ) as usize;
            if body_len < PEER_MANAGER_HEADER_SIZE || body_len > MAX_FIRST_FRAME {
                return Ok(None);
            }
            let frame_len = TCP_TUNNEL_HEADER_SIZE + body_len;
            if read >= frame_len {
                return Ok(Some(buf[..frame_len].to_vec()));
            }
            // The frame is segmented across TCP segments. `peek` cannot wait
            // for more bytes once anything is buffered, so back off briefly.
        }
        tokio::time::sleep(PEEK_RETRY_INTERVAL).await;
    }
}

/// Peeks the first frame and returns the EasyTier network name it carries.
///
/// The stream is never consumed: the returned name is only a routing hint, the
/// instance re-validates the full handshake itself.
pub async fn peek_handshake_network(stream: &TcpStream) -> io::Result<Option<String>> {
    Ok(peek_first_frame(stream)
        .await?
        .as_deref()
        .and_then(handshake_network_from_frame))
}

/// One accepted public TCP connection wrapped as a [`VirtualTcpSocket`] so it
/// can feed the regular `TcpTunnel` framer without any extra hop.
struct AcceptedTcpSocket {
    stream: TcpStream,
    local: SocketAddr,
    peer: SocketAddr,
}

impl AsyncRead for AcceptedTcpSocket {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        std::pin::Pin::new(&mut self.get_mut().stream).poll_read(cx, buf)
    }
}

impl AsyncWrite for AcceptedTcpSocket {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<io::Result<usize>> {
        std::pin::Pin::new(&mut self.get_mut().stream).poll_write(cx, buf)
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        std::pin::Pin::new(&mut self.get_mut().stream).poll_flush(cx)
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        std::pin::Pin::new(&mut self.get_mut().stream).poll_shutdown(cx)
    }

    fn poll_write_vectored(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> std::task::Poll<io::Result<usize>> {
        std::pin::Pin::new(&mut self.get_mut().stream).poll_write_vectored(cx, bufs)
    }

    fn is_write_vectored(&self) -> bool {
        self.stream.is_write_vectored()
    }
}

impl VirtualTcpSocket for AcceptedTcpSocket {
    fn into_split(self) -> VirtualTcpSplit {
        let Self { stream, .. } = self;
        let (read, write) = stream.into_split();
        (Box::new(read), Box::new(write))
    }

    fn local_addr(&self) -> io::Result<SocketAddr> {
        Ok(self.local)
    }

    fn peer_addr(&self) -> io::Result<SocketAddr> {
        Ok(self.peer)
    }
}

/// Wraps an accepted public `TcpStream` into the same tunnel that the instance
/// listener would have produced for a loopback connection.
///
/// This mirrors `crates/dokitier-core/src/connectivity/protocol/raw.rs::
/// upgrade_accepted_tcp_with_local_url` (which is `pub(crate)`) using the public
/// `TcpTunnelUpgrader` + `VirtualTcpSocket` contract.
pub fn tcp_tunnel_from_accepted(
    stream: TcpStream,
    local_url: Url,
) -> Result<Box<dyn Tunnel>, TunnelError> {
    let local = stream.local_addr()?;
    let peer = stream.peer_addr()?;
    let scheme = local_url.scheme().to_owned();
    let remote_url = socket_url(&scheme, peer);
    let info = TunnelInfo {
        tunnel_type: scheme,
        local_addr: Some(local_url.into()),
        remote_addr: Some(remote_url.clone().into()),
        resolved_remote_addr: Some(remote_url.into()),
    };
    TcpTunnelUpgrader::new(info).upgrade(AcceptedTcpSocket {
        stream,
        local,
        peer,
    })
}

/// Hands a peeked, still-unconsumed accepted TCP stream directly to the target
/// instance's peer manager.
///
/// The instance performs the exact handshake/admission it performs for a
/// listener-accepted connection; the client's bytes are never copied through
/// this process-level relay.
pub async fn handoff_tcp(
    peer_manager: &Arc<PeerManagerCore>,
    stream: TcpStream,
    local_url: Url,
) -> anyhow::Result<()> {
    let tunnel = tcp_tunnel_from_accepted(stream, local_url)?;
    peer_manager.add_tunnel_as_server(tunnel).await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// WebSocket: reuse the already upgraded stream, replay the sniffed message
// ---------------------------------------------------------------------------

/// Wraps an already upgraded server-side `WebSocketStream` into a tunnel,
/// prepending the first message that was consumed while sniffing the network.
#[cfg(feature = "websocket")]
pub fn ws_tunnel_from_accepted<S>(
    client: WebSocketStream<S>,
    first_message: Bytes,
    local_url: Url,
    remote_url: Option<Url>,
) -> Result<Box<dyn Tunnel>, TunnelError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let info = TunnelInfo {
        tunnel_type: local_url.scheme().to_owned(),
        local_addr: Some(local_url.into()),
        remote_addr: remote_url.clone().map(Into::into),
        resolved_remote_addr: remote_url.map(Into::into),
    };

    let first =
        ZCPacket::new_from_buf(BytesMut::from(&first_message[..]), ZCPacketType::DummyTunnel);
    Ok(crate::tunnel::websocket::build_ws_handoff_tunnel(
        client,
        Some(first),
        info,
    ))
}

/// Hands an upgraded WebSocket connection directly to the target instance.
#[cfg(feature = "websocket")]
pub async fn handoff_ws<S>(
    peer_manager: &Arc<PeerManagerCore>,
    client: WebSocketStream<S>,
    first_message: Bytes,
    local_url: Url,
    remote_url: Option<Url>,
) -> anyhow::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let tunnel = ws_tunnel_from_accepted(client, first_message, local_url, remote_url)?;
    peer_manager.add_tunnel_as_server(tunnel).await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// QUIC: keep the accepted connection and its first stream, drop the second
// ---------------------------------------------------------------------------

/// `FramedReader` packet cap used by `core/src/tunnel/quic.rs` for the server
/// side of a QUIC session.
#[cfg(feature = "quic")]
pub const QUIC_FRAME_MAX_PACKET_SIZE: usize = 2000;

/// Keeps the public `quinn::Endpoint` alive for as long as the handed-over
/// tunnel lives, and closes the connection when the tunnel is dropped (the same
/// contract as `ConnWrapper` in `core/src/tunnel/quic.rs`).
#[cfg(feature = "quic")]
pub struct QuicConnectionKeepAlive {
    connection: Connection,
    endpoint: Endpoint,
}

#[cfg(feature = "quic")]
impl Drop for QuicConnectionKeepAlive {
    fn drop(&mut self) {
        let _ = &self.endpoint;
        self.connection.close(0u32.into(), b"handoff tunnel closed");
    }
}

/// An `AsyncRead` that yields already-consumed bytes before delegating to the
/// underlying QUIC stream.
#[cfg(feature = "quic")]
struct PrefixedReader<R> {
    prefix: Vec<u8>,
    pos: usize,
    inner: R,
}

#[cfg(feature = "quic")]
impl<R> PrefixedReader<R> {
    fn new(prefix: Vec<u8>, inner: R) -> Self {
        Self {
            prefix,
            pos: 0,
            inner,
        }
    }
}

#[cfg(feature = "quic")]
impl<R: AsyncRead + Unpin> AsyncRead for PrefixedReader<R> {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        if self.pos < self.prefix.len() {
            let remaining = self.prefix.len() - self.pos;
            let take = remaining.min(buf.remaining());
            if take > 0 {
                let start = self.pos;
                buf.put_slice(&self.prefix[start..start + take]);
                self.pos += take;
            }
            return std::task::Poll::Ready(Ok(()));
        }
        std::pin::Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

/// Decodes the network name from the first frame read off the client's QUIC
/// stream (same layout as TCP: `TCPTunnelHeader | PeerManagerHeader | ...`).
#[cfg(feature = "quic")]
pub fn quic_handshake_network(first_frame: &[u8]) -> Option<String> {
    handshake_network_from_frame(first_frame)
}

/// Wraps the accepted public QUIC connection and its first bidirectional stream
/// into the instance's tunnel layout, prepending the already-consumed frame.
///
/// `endpoint` is the public endpoint that accepted the connection; it is kept
/// alive for the tunnel's lifetime.
#[cfg(feature = "quic")]
#[allow(clippy::too_many_arguments)]
pub fn quic_tunnel_from_accepted(
    connection: Connection,
    send: SendStream,
    recv: RecvStream,
    first_frame: Vec<u8>,
    local_url: Url,
    remote_url: Url,
    endpoint: Endpoint,
) -> Result<Box<dyn Tunnel>, TunnelError> {
    let info = TunnelInfo {
        tunnel_type: "quic".to_owned(),
        local_addr: Some(local_url.into()),
        remote_addr: Some(remote_url.clone().into()),
        resolved_remote_addr: Some(remote_url.into()),
    };
    let reader = FramedReader::new(
        PrefixedReader::new(first_frame, recv),
        QUIC_FRAME_MAX_PACKET_SIZE,
    );
    let writer = FramedWriter::new(send);
    let keep_alive = Box::new(QuicConnectionKeepAlive {
        connection,
        endpoint,
    });
    Ok(Box::new(TunnelWrapper::new_with_associate_data(
        reader,
        writer,
        Some(info),
        Some(keep_alive),
    )))
}

/// Hands the accepted public QUIC connection directly to the target instance.
#[cfg(feature = "quic")]
#[allow(clippy::too_many_arguments)]
pub async fn handoff_quic(
    peer_manager: &Arc<PeerManagerCore>,
    connection: Connection,
    send: SendStream,
    recv: RecvStream,
    first_frame: Vec<u8>,
    local_url: Url,
    remote_url: Url,
    endpoint: Endpoint,
) -> anyhow::Result<()> {
    let tunnel = quic_tunnel_from_accepted(
        connection,
        send,
        recv,
        first_frame,
        local_url,
        remote_url,
        endpoint,
    )?;
    peer_manager.add_tunnel_as_server(tunnel).await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// UDP: design only, blocked on new public APIs
// ---------------------------------------------------------------------------

/// One API that has to be added before the shared UDP port can be handed over
/// without a second loopback socket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequiredApi {
    /// Crate-relative file that must gain the API.
    pub file: &'static str,
    /// Proposed signature.
    pub signature: &'static str,
    /// Why the current public surface cannot express this.
    pub why: &'static str,
}

/// Minimal public API additions required for a single-hop shared UDP listener.
///
/// The shared UDP listener owns one public `UdpSocket` per address and must
/// demultiplex EasyTier mux sessions by `(client addr, conn_id)` whose first
/// datagram (`Syn`) does **not** carry the network name. Today
/// `UdpSession` (`crates/dokitier-core/src/socket/udp/session.rs`) has private
/// fields and no constructor from an externally owned datagram source, so the
/// listener has to keep terminating one mux locally and re-originate a second
/// one over loopback.
///
/// The recommended shape: the shared listener implements `VirtualUdpSocket`
/// (public trait, `crates/dokitier-core/src/socket/udp/virtual_socket.rs`) over
/// the public socket plus a per-client demux queue, then for each new session
/// builds a `UdpSession` and lets the instance's regular UDP tunnel admission
/// run. No new `PeerManagerCore` entry point is needed:
/// `UdpTunnelUpgrader::new(info)` (public) + `add_tunnel_as_server` (public)
/// already cover the admission side.
pub const REQUIRED_UDP_HANDOFF_APIS: &[RequiredApi] = &[
    RequiredApi {
        file: "crates/dokitier-core/src/socket/udp/session.rs",
        signature: "impl UdpSession { pub fn from_accepted_socket(\
                    socket: Arc<dyn VirtualUdpSocket>, peer_addr: SocketAddr, \
                    protocol: UdpSessionProtocol) -> Self }",
        why: "UdpSession fields are private and there is no public constructor \
              from an externally owned socket; the shared listener cannot build \
              a per-session view of its one public socket.",
    },
    RequiredApi {
        file: "crates/dokitier-core/src/socket/udp/virtual_socket.rs",
        signature: "pub trait VirtualUdpSocket { /* add */ fn clone_for_session(\
                    &self, peer: SocketAddr, conn_id: u32) -> Arc<dyn VirtualUdpSocket>; }",
        why: "One public socket serves every network/session; the per-session \
              handle must route recv/send by the demux key and must not close \
              the shared socket on drop.",
    },
    RequiredApi {
        file: "crates/dokitier-core/src/socket/udp/session.rs",
        signature: "pub enum UdpSessionAcceptKind { ... /* add */ \
                    Shared { socket: Arc<dyn VirtualUdpSocket> } }",
        why: "Listening requests currently assume the instance binds its own \
              socket; a shared public socket must be injected instead of bound.",
    },
    RequiredApi {
        file: "crates/dokitier-core/src/listener/plan.rs",
        signature: "pub fn unresolved_udp_session_listen_request(\
                    /* add */ shared: Option<Arc<dyn VirtualUdpSocket>>) \
                    -> UdpSessionListenRequest",
        why: "The instance listener plan must be able to consume a host-provided \
              socket for the shared UDP endpoint instead of a loopback bind.",
    },
];

/// Whether `udp://` connections can be handed over with today's public API.
///
/// Always `false` for now; see [`REQUIRED_UDP_HANDOFF_APIS`].
pub const fn udp_direct_handoff_supported() -> bool {
    false
}
