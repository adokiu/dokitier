//! Core-level shared QUIC listener.
//!
//! One public `quic://` endpoint (a single UDP port) is shared by every network
//! instance. Each instance still binds its own loopback
//! `quic://127.0.0.1:<internal port>` listener; this module owns the public
//! port, terminates the client QUIC connection, peeks at the EasyTier handshake
//! to learn the network name, and re-originates an equivalent QUIC connection
//! to the matching loopback port. The client cannot tell the difference from a
//! dedicated listener.
//!
//! # TLS / ALPN
//!
//! EasyTier's `quic://` tunnel does **not** use TLS. `core/src/tunnel/quic.rs`
//! implements a custom `quinn_proto::crypto` configuration that replaces the
//! TLS handshake with the EasyTier transport-parameter exchange plus an
//! integrity checksum, so there is no certificate to generate, no certificate
//! verification and no ALPN. This module therefore reuses the exact same
//! [`server_config`], [`client_config`] and [`endpoint_config`] as the normal
//! `quic://` tunnel: ALPN stays empty, the custom crypto is identical on both
//! legs, and the public endpoint negotiates the same QUIC versions as an
//! instance listener (`QUIC_VERSION_ETQ1` preferred, legacy version 1 accepted).
//! A real TLS terminator cannot be put in front of this listener because the
//! wire format is not standard TLS-in-QUIC.
//!
//! # Network identification
//!
//! After the QUIC handshake the EasyTier client opens its first bidirectional
//! stream and writes the regular EasyTier framed packet stream: a 4-byte
//! little-endian body length (`TCPTunnelHeader`), a `PeerManagerHeader`, then
//! the protobuf payload. The first payload is
//! `peer_rpc.HandshakeRequest`; its `network_name` (field 5) selects the target
//! network. This is byte-for-byte the same frame the TCP shared listener parses
//! in `core/src/shared_listener.rs`, because `FramedReader`/`FramedWriter` sit
//! above the QUIC stream. The sniffed frame is replayed verbatim to the
//! upstream loopback connection, so the instance's own handshake parser sees
//! exactly what a direct client would have sent.
//!
//! [`resolve`] is invoked once per accepted connection, so hot configuration
//! changes apply to new connections only; already established relays keep their
//! original internal target.
//!
//! # Limitations
//!
//! * Only the first bidirectional stream, i.e. the EasyTier tunnel stream, is
//!   proxied. Additional concurrent bidirectional streams, unidirectional
//!   streams and QUIC datagrams are not forwarded. EasyTier's `quic://` tunnel
//!   does not use them today (`transport_config()` sets
//!   `max_concurrent_uni_streams(0)` and never enables datagrams), so current
//!   peers are fully supported; a future peer using extra streams or datagrams
//!   would need a stream-mapping proxy here.
//! * The connection is terminated and re-originated, so the instance observes
//!   this relay's loopback UDP address instead of the real client address (the
//!   TCP shared listener behaves the same way). Public address advertisement
//!   comes from the shared listener's runtime `advertise_urls()` — it is never
//!   written into any config file.
//! * QUIC connection migration is not preserved: a client changing its UDP path
//!   keeps the public leg alive, but the upstream leg remains a separate
//!   connection owned by this process.
//! * No authentication happens here: like the instance's own `quic://`
//!   listener, the network secret is only validated after the handshake frame
//!   reaches the instance. This module is a demultiplexer, not a security
//!   boundary.
//!
//! The caller registers the module behind the `quic` cargo feature (it depends
//! on `crate::tunnel::quic`):
//!
//! ```ignore
//! #[cfg(feature = "quic")]
//! pub mod shared_listener_quic;
//! ```
//!
//! and drives it from the shared-listener lifecycle code:
//!
//! ```ignore
//! let (bound, tasks) = shared_listener_quic::bind(quic_addrs, resolve).await?;
//! // New listeners are fully bound before `Ok` is returned: on `Err` the
//! // caller keeps the old listeners and nothing is leaked (no task is spawned
//! // until every address has bound successfully). The returned `JoinHandle`s
//! // are aborted by the caller when the address set is hot-updated.
//! ```

use std::net::{Ipv4Addr, SocketAddr, ToSocketAddrs};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, anyhow, bail};
use prost::Message;
use quinn::{Connection, Endpoint, RecvStream};

use dokitier_core::packet::{PEER_MANAGER_HEADER_SIZE, TCP_TUNNEL_HEADER_SIZE};
use dokitier_proto::peer_rpc::HandshakeRequest;

use crate::tunnel::quic::{connect_with_etq1, endpoint_config, server_config};

/// How long a client may take to deliver the first EasyTier frame (header and
/// body) after the QUIC handshake completed.
const FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(8);
/// How long a QUIC handshake (public leg and loopback leg alike) may take.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// How long the loopback connect + `open_bi` may take.
const UPSTREAM_CONNECT_TIMEOUT: Duration = Duration::from_secs(8);
/// Same upper bound as the TCP shared listener uses for the sniffed frame.
const MAX_FIRST_FRAME: usize = 64 * 1024;

/// The callback resolving a network name to the loopback port of the matching
/// instance. Returning `None` rejects the connection (the instance for that
/// network is not running).
type ResolveFn = Arc<dyn Fn(&str) -> Option<u16> + Send + Sync>;

fn normalize(addr: &str) -> String {
    let addr = addr.trim();
    if addr.contains("://") {
        addr.to_string()
    } else {
        format!("quic://{addr}")
    }
}

fn proto_of(addr: &str) -> &str {
    match addr.split_once("://") {
        Some((proto, _)) => proto,
        None => "quic",
    }
}

fn bind_of(addr: &str) -> &str {
    addr.split_once("://").map(|(_, rest)| rest).unwrap_or(addr)
}

fn parse_bind_addr(raw: &str) -> anyhow::Result<SocketAddr> {
    raw.to_socket_addrs()
        .with_context(|| format!("resolve shared quic listener address {raw}"))?
        .next()
        .ok_or_else(|| anyhow!("shared quic listener address {raw} resolved to no socket address"))
}

fn quinn_runtime() -> anyhow::Result<Arc<dyn quinn::Runtime>> {
    quinn::default_runtime().ok_or_else(|| anyhow!("no async runtime available for quinn"))
}

fn nonblocking_socket(addr: SocketAddr) -> anyhow::Result<std::net::UdpSocket> {
    let socket = std::net::UdpSocket::bind(addr)
        .with_context(|| format!("bind shared quic listener socket {addr}"))?;
    socket
        .set_nonblocking(true)
        .with_context(|| format!("set shared quic listener socket {addr} non-blocking"))?;
    Ok(socket)
}

fn public_endpoint(addr: SocketAddr) -> anyhow::Result<Endpoint> {
    Endpoint::new(
        endpoint_config(),
        Some(server_config()),
        nonblocking_socket(addr)?,
        quinn_runtime()?,
    )
    .with_context(|| format!("create shared quic listener endpoint on {addr}"))
}

fn loopback_client_endpoint() -> anyhow::Result<Endpoint> {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, 0));
    Endpoint::new(
        endpoint_config(),
        None,
        nonblocking_socket(addr)?,
        quinn_runtime()?,
    )
    .with_context(|| "create shared quic listener loopback client endpoint")
}

/// Decodes the sniffed EasyTier frame and returns the network name.
fn decode_first_frame(frame: &[u8]) -> anyhow::Result<String> {
    if frame.len() < TCP_TUNNEL_HEADER_SIZE + PEER_MANAGER_HEADER_SIZE {
        bail!("first EasyTier frame is too short: {} bytes", frame.len());
    }
    let body_len = u32::from_le_bytes(
        frame[..TCP_TUNNEL_HEADER_SIZE]
            .try_into()
            .expect("slice is exactly the tunnel header size"),
    ) as usize;
    if body_len < PEER_MANAGER_HEADER_SIZE || body_len > MAX_FIRST_FRAME {
        bail!("invalid first EasyTier frame body length {body_len}");
    }
    let body = &frame[TCP_TUNNEL_HEADER_SIZE..];
    if body.len() != body_len {
        bail!(
            "first EasyTier frame length mismatch: header says {body_len}, got {}",
            body.len()
        );
    }
    let payload = &body[PEER_MANAGER_HEADER_SIZE..];
    let request = HandshakeRequest::decode(payload)
        .context("decode EasyTier HandshakeRequest from first frame")?;
    Ok(request.network_name)
}

/// Reads the first EasyTier framed packet from the tunnel stream and sniffs the
/// network name from its `HandshakeRequest` payload.
async fn read_first_frame(stream: &mut RecvStream) -> anyhow::Result<(Vec<u8>, String)> {
    let mut header = [0u8; TCP_TUNNEL_HEADER_SIZE];
    tokio::time::timeout(FIRST_FRAME_TIMEOUT, stream.read_exact(&mut header))
        .await
        .map_err(|_| anyhow!("timed out waiting for the first EasyTier frame header"))?
        .context("read the first EasyTier frame header")?;

    let body_len = u32::from_le_bytes(header) as usize;
    if body_len < PEER_MANAGER_HEADER_SIZE || body_len > MAX_FIRST_FRAME {
        bail!("invalid first EasyTier frame body length {body_len}");
    }

    let mut frame = vec![0u8; TCP_TUNNEL_HEADER_SIZE + body_len];
    frame[..TCP_TUNNEL_HEADER_SIZE].copy_from_slice(&header);
    tokio::time::timeout(
        FIRST_FRAME_TIMEOUT,
        stream.read_exact(&mut frame[TCP_TUNNEL_HEADER_SIZE..]),
    )
    .await
    .map_err(|_| anyhow!("timed out waiting for the first EasyTier frame body"))?
    .context("read the first EasyTier frame body")?;

    let network = decode_first_frame(&frame)?;
    Ok((frame, network))
}

/// Binds every `quic://` address in `addrs` and serves the shared endpoint.
///
/// Returns the actually bound public addresses (`quic://<addr>`, with `:0`
/// replaced by the OS-assigned port) and one accept-loop task per address. The
/// handles are intentionally returned so the caller can abort them when a new
/// address set is hot-applied.
///
/// Addresses with another protocol prefix are skipped with a warning; a
/// non-empty `addrs` that contains no `quic://` address yields
/// `Ok((Vec::new(), Vec::new()))`. Any bind/create failure returns `Err` before
/// a single task is spawned, so the caller can simply keep its previous
/// listeners (nothing to leak on the failed attempt).
///
/// `resolve` maps the sniffed `network_name` to the loopback port of the
/// instance's own `quic://127.0.0.1:<port>` listener; `None` means "no instance
/// for this network" and the connection is closed.
pub async fn bind(
    addrs: Vec<String>,
    resolve: Arc<dyn Fn(&str) -> Option<u16> + Send + Sync>,
) -> anyhow::Result<(Vec<String>, Vec<tokio::task::JoinHandle<()>>)> {
    let mut listeners: Vec<(Endpoint, String)> = Vec::new();
    for raw in addrs {
        let normalized = normalize(&raw);
        let proto = proto_of(&normalized);
        if proto != "quic" {
            tracing::warn!(
                %normalized,
                proto,
                "shared quic listener skipped an address with another protocol"
            );
            continue;
        }
        let bind_addr = parse_bind_addr(bind_of(&normalized))?;
        let endpoint = public_endpoint(bind_addr)?;
        let local = endpoint
            .local_addr()
            .context("query bound shared quic listener address")?;
        let public = format!("quic://{local}");
        tracing::info!(%public, "shared quic listener bound");
        listeners.push((endpoint, public));
    }

    let bound: Vec<String> = listeners
        .iter()
        .map(|(_, public)| public.clone())
        .collect();
    if bound.is_empty() {
        return Ok((bound, Vec::new()));
    }

    // The loopback client endpoint is shared by every public listener and every
    // relayed connection. It must not be created before the public binds are
    // validated, so a failure above leaves the caller's old listeners intact.
    let upstream_endpoint = loopback_client_endpoint()?;

    let mut tasks = Vec::with_capacity(listeners.len());
    for (endpoint, _) in listeners {
        tasks.push(tokio::spawn(accept_loop(
            endpoint,
            upstream_endpoint.clone(),
            resolve.clone(),
        )));
    }
    Ok((bound, tasks))
}

async fn accept_loop(endpoint: Endpoint, upstream: Endpoint, resolve: ResolveFn) {
    loop {
        let Some(incoming) = endpoint.accept().await else {
            tracing::debug!("shared quic listener endpoint closed");
            break;
        };
        let peer = incoming.remote_address();
        let connecting = match incoming.accept() {
            Ok(connecting) => connecting,
            Err(err) => {
                tracing::debug!(%peer, ?err, "shared quic listener rejected the connection attempt");
                continue;
            }
        };
        let upstream = upstream.clone();
        let resolve = resolve.clone();
        let public_endpoint = endpoint.clone();
        tokio::spawn(async move {
            let client = match tokio::time::timeout(HANDSHAKE_TIMEOUT, connecting).await {
                Ok(Ok(client)) => client,
                Ok(Err(err)) => {
                    tracing::debug!(%peer, ?err, "shared quic listener handshake failed");
                    return;
                }
                Err(_) => {
                    tracing::debug!(%peer, "shared quic listener handshake timed out");
                    return;
                }
            };
            handle_connection(client, upstream, resolve, public_endpoint).await;
        });
    }
}

async fn handle_connection(
    client: Connection,
    upstream_endpoint: Endpoint,
    resolve: ResolveFn,
    public_endpoint: Endpoint,
) {
    let peer = client.remote_address();

    // EasyTier opens its tunnel stream right after the handshake; accept it and
    // sniff the (unencrypted) handshake frame that carries the network name.
    let (client_send, mut client_recv) =
        match tokio::time::timeout(HANDSHAKE_TIMEOUT, client.accept_bi()).await {
            Ok(Ok(streams)) => streams,
            Ok(Err(err)) => {
                tracing::debug!(%peer, ?err, "shared quic listener client closed before opening a stream");
                return;
            }
            Err(_) => {
                tracing::debug!(%peer, "shared quic listener client opened no stream in time");
                client.close(0u32.into(), b"handshake timeout");
                return;
            }
        };

    let (frame, network) = match read_first_frame(&mut client_recv).await {
        Ok(value) => value,
        Err(err) => {
            tracing::debug!(%peer, ?err, "shared quic listener dropped a connection before the handshake");
            client.close(0u32.into(), b"invalid handshake");
            return;
        }
    };

    if let Some(peer_manager) = crate::shared_listener_handoff::resolve_network(&network) {
        let local_url = url::Url::parse(&format!("quic://{}", public_endpoint.local_addr().unwrap_or(peer)))
            .expect("valid quic url");
        let remote_url = url::Url::parse(&format!("quic://{peer}")).expect("valid quic url");
        match crate::shared_listener_handoff::handoff_quic(
            &peer_manager,
            client,
            client_send,
            client_recv,
            frame,
            local_url,
            remote_url,
            public_endpoint,
        )
        .await
        {
            Ok(()) => return,
            Err(err) => {
                tracing::warn!(%network, error = %err, "shared quic direct handoff failed");
                return;
            }
        }
    }

    let Some(port) = resolve(&network) else {
        tracing::warn!(%peer, %network, "shared quic listener has no instance for the network");
        client.close(0u32.into(), b"unknown network");
        return;
    };

    let target = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let upstream = match tokio::time::timeout(
        UPSTREAM_CONNECT_TIMEOUT,
        connect_with_etq1(&upstream_endpoint, target, "localhost"),
    )
    .await
    {
        Ok(Ok(connection)) => connection,
        Ok(Err(err)) => {
            tracing::warn!(%peer, %network, %port, ?err, "shared quic listener internal connect failed");
            client.close(0u32.into(), b"upstream unavailable");
            return;
        }
        Err(_) => {
            tracing::warn!(%peer, %network, %port, "shared quic listener internal connect timed out");
            client.close(0u32.into(), b"upstream unavailable");
            return;
        }
    };

    let (mut upstream_send, upstream_recv) =
        match tokio::time::timeout(UPSTREAM_CONNECT_TIMEOUT, upstream.open_bi()).await {
            Ok(Ok(streams)) => streams,
            Ok(Err(err)) => {
                tracing::warn!(%peer, %network, %port, ?err, "shared quic listener failed to open the internal tunnel stream");
                client.close(0u32.into(), b"upstream unavailable");
                upstream.close(0u32.into(), b"shared quic relay failed");
                return;
            }
            Err(_) => {
                tracing::warn!(%peer, %network, %port, "shared quic listener timed out opening the internal tunnel stream");
                client.close(0u32.into(), b"upstream unavailable");
                upstream.close(0u32.into(), b"shared quic relay failed");
                return;
            }
        };

    // Replay the sniffed handshake frame before starting the raw byte pump, so
    // the instance's handshake parser sees an unmodified EasyTier frame stream.
    if let Err(err) = upstream_send.write_all(&frame).await {
        tracing::warn!(%peer, %network, %port, ?err, "shared quic listener failed to replay the handshake frame");
        client.close(0u32.into(), b"relay failed");
        upstream.close(0u32.into(), b"relay failed");
        return;
    }

    tracing::debug!(%peer, %network, %port, "shared quic listener relaying a tunnel");
    relay(client, upstream, client_send, client_recv, upstream_send, upstream_recv).await;
}

/// Pumps bytes in both directions until the tunnel ends or either QUIC
/// connection closes, then closes both connections.
async fn relay(
    client: Connection,
    upstream: Connection,
    mut client_send: quinn::SendStream,
    mut client_recv: quinn::RecvStream,
    mut upstream_send: quinn::SendStream,
    mut upstream_recv: quinn::RecvStream,
) {
    let peer = client.remote_address();
    let client_watch = client.clone();
    let upstream_watch = upstream.clone();

    // `copy` returns on EOF (half close) or on stream/connection error. A
    // client half close is forwarded as a stream FIN via `finish()`, matching
    // normal TCP/QUIC tunnel teardown.
    let relay = async {
        tokio::join!(
            async {
                let result = tokio::io::copy(&mut client_recv, &mut upstream_send).await;
                let _ = upstream_send.finish();
                result
            },
            async {
                let result = tokio::io::copy(&mut upstream_recv, &mut client_send).await;
                let _ = client_send.finish();
                result
            },
        )
    };
    tokio::pin!(relay);

    tokio::select! {
        (up, down) = &mut relay => {
            if let Err(err) = up {
                tracing::debug!(%peer, ?err, "shared quic listener upstream relay direction ended");
            }
            if let Err(err) = down {
                tracing::debug!(%peer, ?err, "shared quic listener downstream relay direction ended");
            }
        }
        reason = client_watch.closed() => {
            tracing::debug!(%peer, ?reason, "shared quic listener client connection closed");
        }
        reason = upstream_watch.closed() => {
            tracing::debug!(%peer, ?reason, "shared quic listener internal connection closed");
        }
    }

    drop(relay);
    client.close(0u32.into(), b"shared quic relay done");
    upstream.close(0u32.into(), b"shared quic relay done");
}
