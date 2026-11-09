use super::ws_frame;
use super::FromUrl;
use crate::tunnel::common::bind;
use crate::{proto::common::TunnelInfo, socket::tcp::RuntimeTcpSocket};
use anyhow::Context as _;
use bytes::{BufMut, BytesMut};
use cidr::IpCidr;
use dokitier_core::{
    packet::{ZCPacket, ZCPacketType},
    socket::tcp::VirtualTcpSocket,
    tunnel::{IpVersion, Tunnel, TunnelError, wrapper::TunnelWrapper},
};
use forwarded_header_value::ForwardedHeaderValue;
use futures::{Sink, SinkExt, Stream, StreamExt};
use std::{
    net::{IpAddr, SocketAddr},
    pin::Pin,
    sync::{Arc, LazyLock},
    task::{Context, Poll},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncWrite, AsyncWriteExt},
    net::TcpListener,
    sync::mpsc,
    time::timeout,
};
use tokio_rustls::TlsAcceptor;
use tokio_util::either::Either;
use tokio_websockets::{
    ClientBuilder, Limits, MaybeTlsStream, Message, ServerBuilder, WebSocketStream,
};
use zerocopy::AsBytes as _;

pub(crate) const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
pub(crate) const SERVER_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(3);

static TRUSTED_PROXIES: LazyLock<Vec<IpCidr>> = LazyLock::new(|| {
    [
        "127.0.0.0/8",
        "10.0.0.0/8",
        "172.16.0.0/12",
        "192.168.0.0/16",
        "::1/128",
        "fc00::/7",
    ]
    .into_iter()
    .map(|cidr| cidr.parse().unwrap())
    .collect()
});

fn trusted_proxy_contains(ip: IpAddr) -> bool {
    TRUSTED_PROXIES.iter().any(|cidr| match (cidr, ip) {
        (IpCidr::V4(cidr), IpAddr::V4(ip)) => cidr.contains(&ip),
        (IpCidr::V6(cidr), IpAddr::V6(ip)) => cidr.contains(&ip),
        _ => false,
    })
}

fn websocket_error(error: impl std::fmt::Display) -> TunnelError {
    TunnelError::ProtocolError(format!("websocket error: {error}"))
}

fn is_wss(url: &url::Url) -> Result<bool, TunnelError> {
    match url.scheme() {
        "ws" => Ok(false),
        "wss" => Ok(true),
        scheme => Err(TunnelError::InvalidProtocol(scheme.to_owned())),
    }
}

struct WebSocketPacketSink<S> {
    inner: S,
}

impl<S> WebSocketPacketSink<S> {
    fn new(inner: S) -> Self {
        Self { inner }
    }
}

impl<S, E> Sink<ZCPacket> for WebSocketPacketSink<S>
where
    S: Sink<Message, Error = E> + Unpin,
    E: std::fmt::Display,
{
    type Error = TunnelError;

    fn poll_ready(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Pin::new(&mut self.inner)
            .poll_ready(cx)
            .map(|result| result.map_err(websocket_error))
    }

    fn start_send(mut self: Pin<&mut Self>, packet: ZCPacket) -> Result<(), Self::Error> {
        Pin::new(&mut self.inner)
            .start_send(Message::binary(packet.tunnel_payload_bytes().freeze()))
            .map_err(websocket_error)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Pin::new(&mut self.inner)
            .poll_flush(cx)
            .map(|result| result.map_err(websocket_error))
    }

    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Pin::new(&mut self.inner)
            .poll_close(cx)
            .map(|result| result.map_err(websocket_error))
    }
}

// ---------------------------------------------------------------------------
// Single-owner pump
// ---------------------------------------------------------------------------
//
// `futures`' `split()` guards both halves of a `WebSocketStream` with a
// `BiLock`, so the read task and the write task take turns acquiring the same
// stream. Under concurrent bulk traffic in both directions this serializes the
// two tokio tasks against each other, and the interlocked waker hand-off
// inside tokio-websockets (`flushing_waker` in `poll_next_frame`)
// intermittently stalls the write half: measured on a clean docker bridge,
// bidirectional traffic collapsed to 80-180 Mbps per direction while each
// direction alone sustains 400 Mbps. A single pump task owns the stream and
// services both directions from one `select!` loop; the tunnel halves exposed
// to the peer connection are then plain channels with no shared state.
//
// Wire format is untouched: tokio-websockets still performs the HTTP upgrade
// and the frame codec, so peers (including the shared listeners and
// EasyTier-compatible endpoints) interoperate unchanged.

const WS_PUMP_CHANNEL_CAPACITY: usize = 128;
const WS_PUMP_MAX_BATCH: usize = 32;

struct WsChannelStream {
    rx: mpsc::Receiver<Result<ZCPacket, TunnelError>>,
}

impl Stream for WsChannelStream {
    type Item = Result<ZCPacket, TunnelError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.rx).poll_recv(cx)
    }
}

struct WsChannelSink {
    tx: tokio_util::sync::PollSender<ZCPacket>,
}

impl Sink<ZCPacket> for WsChannelSink {
    type Error = TunnelError;

    fn poll_ready(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(), Self::Error>> {
        // PollSender registers the task waker with the channel's capacity
        // semaphore; waking without registering (e.g. wake_by_ref on Full)
        // would busy-spin instead of yielding until capacity frees up.
        Pin::new(&mut self.tx)
            .poll_reserve(cx)
            .map_err(|_| TunnelError::Shutdown)
    }

    fn start_send(mut self: Pin<&mut Self>, item: ZCPacket) -> Result<(), Self::Error> {
        Pin::new(&mut self.tx)
            .send_item(item)
            .map_err(|_| TunnelError::Shutdown)
    }

    // The channel forwards packets immediately; nothing to flush.
    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn poll_close(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }
}

async fn ws_pump<T>(
    ws: WebSocketStream<T>,
    incoming: mpsc::Sender<Result<ZCPacket, TunnelError>>,
    mut outgoing: mpsc::Receiver<ZCPacket>,
) where
    T: AsyncRead + AsyncWrite + Unpin,
{
    // Split into BiLock halves so one task can poll the read and write halves
    // from the same `select!`. The lock is only ever held for a single poll
    // here, so there is no cross-task contention, and — crucially — the read
    // half keeps making progress while a write flush is waiting for socket
    // writability. A sequential design (finish the flush, then read) would
    // deadlock under full-duplex saturation: neither side drains, the reverse
    // TCP window freezes, and one direction collapses to a trickle.
    let (write, read) = ws.split();
    let mut read = read;
    let mut write = WebSocketPacketSink::new(write);
    let mut flushing = false;
    let mut batch: Vec<ZCPacket> = Vec::with_capacity(WS_PUMP_MAX_BATCH);
    let stats = std::env::var("DOKITIER_WS_PUMP_STATS").is_ok();
    let mut stat_reads: u64 = 0;
    let mut stat_writes: u64 = 0;
    let mut stat_flush_waits: u64 = 0;
    let mut stat_last = std::time::Instant::now();
    loop {
        if stats && stat_last.elapsed() >= std::time::Duration::from_secs(2) {
            let elapsed = stat_last.elapsed().as_secs_f64();
            tracing::info!(
                reads_per_s = (stat_reads as f64 / elapsed) as u64,
                writes_per_s = (stat_writes as f64 / elapsed) as u64,
                flush_pending_s = (stat_flush_waits as f64 / elapsed) as u64,
                "ws pump stats"
            );
            stat_reads = 0;
            stat_writes = 0;
            stat_flush_waits = 0;
            stat_last = std::time::Instant::now();
        }
        tokio::select! {
            message = read.next() => {
                stat_reads += 1;
                match message {
                    Some(message) => {
                        let item = match map_from_ws_message(message).await {
                            Some(item) => item,
                            // Non-binary frame (ping/pong handled internally);
                            // keep looping.
                            None => continue,
                        };
                        if incoming.send(item).await.is_err() {
                            return;
                        }
                    }
                    None => return,
                }
            }
            packet = outgoing.recv(), if !flushing => {
                stat_writes += 1;
                match packet {
                    Some(packet) => batch.push(packet),
                    None => return,
                }
                while batch.len() < WS_PUMP_MAX_BATCH {
                    match outgoing.try_recv() {
                        Ok(packet) => batch.push(packet),
                        Err(_) => break,
                    }
                }
                let mut failed = false;
                for packet in batch.drain(..) {
                    if write.feed(packet).await.is_err() {
                        failed = true;
                        break;
                    }
                }
                if failed {
                    return;
                }
                flushing = true;
            }
            _ = write.flush(), if flushing => {
                stat_flush_waits += 1;
                flushing = false;
            }
        }
    }
}

pub fn build_ws_tunnel<T>(stream: WebSocketStream<T>, info: TunnelInfo) -> Box<dyn Tunnel>
where
    T: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    build_ws_handoff_tunnel(stream, None, info)
}

/// Same as [`build_ws_tunnel`], with an already-consumed first frame (the
/// shared listener sniffs the network name before handing the connection over)
/// prepended to the read half.
pub fn build_ws_handoff_tunnel<T>(
    stream: WebSocketStream<T>,
    first: Option<ZCPacket>,
    info: TunnelInfo,
) -> Box<dyn Tunnel>
where
    T: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (incoming_tx, incoming_rx) =
        mpsc::channel::<Result<ZCPacket, TunnelError>>(WS_PUMP_CHANNEL_CAPACITY);
    let (outgoing_tx, outgoing_rx) = mpsc::channel::<ZCPacket>(WS_PUMP_CHANNEL_CAPACITY);
    if let Some(first) = first {
        let _ = incoming_tx.try_send(Ok(first));
    }
    tokio::spawn(ws_pump(stream, incoming_tx, outgoing_rx));
    Box::new(TunnelWrapper::new(
        WsChannelStream { rx: incoming_rx },
        WsChannelSink {
            tx: tokio_util::sync::PollSender::new(outgoing_tx),
        },
        Some(info),
    ))
}

// ---------------------------------------------------------------------------
// Custom data plane for plain `ws://` (true full-duplex)
// ---------------------------------------------------------------------------

/// Builds the tunnel for a plain `ws://` connection from the raw stream
/// recovered after the HTTP upgrade. `client_role` must match the endpoint:
/// the client masks outgoing frames and receives unmasked ones; the server is
/// the mirror image (RFC 6455 §5.1-§5.3).
pub fn build_plain_ws_tunnel<S>(
    stream: S,
    first: Option<ZCPacket>,
    client_role: bool,
    info: TunnelInfo,
) -> Box<dyn Tunnel>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (incoming_tx, incoming_rx) =
        mpsc::channel::<Result<ZCPacket, TunnelError>>(WS_PUMP_CHANNEL_CAPACITY);
    let (outgoing_tx, outgoing_rx) = mpsc::channel::<ZCPacket>(WS_PUMP_CHANNEL_CAPACITY);
    let (control_tx, control_rx) = mpsc::channel::<ws_frame::WsControl>(8);
    if let Some(first) = first {
        let _ = incoming_tx.try_send(Ok(first));
    }
    let (read_half, write_half) = tokio::io::split(stream);
    let reader = tokio::io::BufReader::with_capacity(64 * 1024, read_half);
    tokio::spawn(ws_custom_reader(reader, incoming_tx, control_tx));
    tokio::spawn(ws_custom_writer(write_half, client_role, outgoing_rx, control_rx));
    Box::new(TunnelWrapper::new(
        ws_frame::WsFrameStream::new(incoming_rx),
        ws_frame::WsFrameSink::new(tokio_util::sync::PollSender::new(outgoing_tx)),
        Some(info),
    ))
}

/// Reader task: owns the read half exclusively, so inbound frames never wait
/// for the write side.
async fn ws_custom_reader<R: AsyncRead + Unpin + Send + 'static>(
    mut rd: tokio::io::BufReader<R>,
    incoming: mpsc::Sender<Result<ZCPacket, TunnelError>>,
    control_tx: mpsc::Sender<ws_frame::WsControl>,
) {
    let mut buf = BytesMut::with_capacity(64 * 1024);
    loop {
        match ws_frame::read_frame(&mut rd, &mut buf).await {
            Ok(Some(frame)) => match frame.opcode {
                ws_frame::OP_BINARY => {
                    let packet =
                        ZCPacket::new_from_buf(frame.payload, ZCPacketType::DummyTunnel);
                    if incoming.send(Ok(packet)).await.is_err() {
                        return;
                    }
                }
                ws_frame::OP_PING => {
                    if control_tx
                        .send(ws_frame::WsControl::Pong(frame.payload.freeze()))
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
                ws_frame::OP_CLOSE => {
                    let _ = control_tx.send(ws_frame::WsControl::Close).await;
                    return;
                }
                // Pongs and continuations carry no tunnel data; peer messages
                // are unfragmented (frame_size 4 MB).
                _ => continue,
            },
            Ok(None) => return,
            Err(err) => {
                tracing::debug!(?err, "ws frame read error");
                return;
            }
        }
    }
}

/// Writer task: owns the write half exclusively, so outbound frames never wait
/// for the read side. Batches up to [`WS_PUMP_MAX_BATCH`] frames into one
/// vectored write.
async fn ws_custom_writer<W: AsyncWrite + Unpin + Send + 'static>(
    mut wr: W,
    client_role: bool,
    mut outgoing: mpsc::Receiver<ZCPacket>,
    mut control_rx: mpsc::Receiver<ws_frame::WsControl>,
) {
    let mut controls: Vec<Vec<u8>> = Vec::new();
    let mut batch: Vec<ZCPacket> = Vec::with_capacity(WS_PUMP_MAX_BATCH);
    let mut closed = false;
    loop {
        // Gather pending control frames without blocking.
        while controls.len() < 8 {
            match control_rx.try_recv() {
                Ok(ws_frame::WsControl::Pong(payload)) => {
                    controls.push(ws_frame::control_frame(
                        ws_frame::OP_PONG,
                        &payload,
                        client_role,
                    ));
                }
                Ok(ws_frame::WsControl::Close) => {
                    controls
                        .push(ws_frame::control_frame(ws_frame::OP_CLOSE, &[], client_role));
                    closed = true;
                    break;
                }
                Err(_) => break,
            }
        }
        // Gather pending data frames without blocking.
        if !closed {
            while batch.len() < WS_PUMP_MAX_BATCH {
                match outgoing.try_recv() {
                    Ok(packet) => batch.push(packet),
                    Err(_) => break,
                }
            }
        }
        if controls.is_empty() && batch.is_empty() {
            // Nothing queued: wait for either direction to produce work.
            tokio::select! {
                packet = outgoing.recv(), if !closed => {
                    match packet {
                        Some(packet) => batch.push(packet),
                        None => {
                            controls.push(ws_frame::control_frame(
                                ws_frame::OP_CLOSE,
                                &[],
                                client_role,
                            ));
                            closed = true;
                        }
                    }
                }
                control = control_rx.recv() => {
                    match control {
                        Some(ws_frame::WsControl::Pong(payload)) => {
                            controls.push(ws_frame::control_frame(
                                ws_frame::OP_PONG,
                                &payload,
                                client_role,
                            ));
                        }
                        Some(ws_frame::WsControl::Close) => {
                            controls.push(ws_frame::control_frame(
                                ws_frame::OP_CLOSE,
                                &[],
                                client_role,
                            ));
                            closed = true;
                        }
                        None => return,
                    }
                }
            }
            continue;
        }
        let data: Vec<ZCPacket> = batch.drain(..).collect();
        if write_all_frames(&mut wr, &controls, data, client_role)
            .await
            .is_err()
        {
            return;
        }
        controls.clear();
        if closed {
            return;
        }
    }
}

/// Writes every frame as one vectored operation, falling back to sequential
/// writes when the socket does not support vectored IO.
async fn write_all_frames<W: AsyncWrite + Unpin>(
    wr: &mut W,
    controls: &[Vec<u8>],
    data: Vec<ZCPacket>,
    client_role: bool,
) -> std::io::Result<()> {
    let mut bufs: Vec<Vec<u8>> = Vec::with_capacity(controls.len() + data.len());
    for control in controls {
        bufs.push(control.clone());
    }
    for packet in data {
        let payload = packet.tunnel_payload_bytes().freeze();
        let mut buf = BytesMut::with_capacity(14 + payload.len());
        let mask_key = if client_role {
            Some(ws_frame::rand_mask_key())
        } else {
            None
        };
        ws_frame::put_frame_header(&mut buf, ws_frame::OP_BINARY, payload.len(), mask_key, true);
        buf.put_slice(&payload);
        if let Some(key) = mask_key {
            let header_len = buf.len() - payload.len();
            ws_frame::mask_payload(&key, &mut buf[header_len..]);
        }
        bufs.push(buf.to_vec());
    }
    let mut idx = 0usize;
    let mut off = 0usize;
    while idx < bufs.len() {
        let slices: Vec<std::io::IoSlice> = bufs[idx..]
            .iter()
            .enumerate()
            .map(|(i, buf)| {
                if i == 0 && off > 0 {
                    std::io::IoSlice::new(&buf[off..])
                } else {
                    std::io::IoSlice::new(buf)
                }
            })
            .collect();
        let written = wr.write_vectored(&slices).await?;
        if written == 0 {
            // Vectored IO unsupported or nothing accepted: fall back to
            // sequential writes for the remainder.
            for buf in &bufs[idx..] {
                let start = if idx == 0 && off > 0 { off } else { 0 };
                wr.write_all(&buf[start..]).await?;
            }
            return Ok(());
        }
        off += written;
        while idx < bufs.len() && off >= bufs[idx].len() {
            off -= bufs[idx].len();
            idx += 1;
        }
    }
    Ok(())
}


async fn map_from_ws_message(
    message: Result<Message, tokio_websockets::Error>,
) -> Option<Result<ZCPacket, TunnelError>> {
    let message = match message {
        Ok(message) => message,
        Err(error) => {
            tracing::error!(?error, "recv from websocket error");
            return Some(Err(websocket_error(error)));
        }
    };
    if message.is_close() {
        tracing::warn!("recv close message from websocket");
        return None;
    }
    if !message.is_binary() {
        let message = format!("{message:?}");
        tracing::error!(?message, "Invalid packet");
        return Some(Err(TunnelError::InvalidPacket(message)));
    }
    Some(Ok(ZCPacket::new_from_buf(
        BytesMut::from(message.into_payload().as_bytes()),
        ZCPacketType::DummyTunnel,
    )))
}

#[derive(Debug)]
struct SkipServerVerification(Arc<rustls::crypto::CryptoProvider>);

impl SkipServerVerification {
    fn new(provider: Arc<rustls::crypto::CryptoProvider>) -> Arc<Self> {
        Arc::new(Self(provider))
    }
}

impl rustls::client::danger::ServerCertVerifier for SkipServerVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

fn get_insecure_tls_client_config() -> rustls::ClientConfig {
    // Library callers may not have initialized a process-wide provider.
    // Use the same explicit backend for TLS and signature verification.
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut config = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .expect("ring supports the default TLS protocol versions")
        .dangerous()
        .with_custom_certificate_verifier(SkipServerVerification::new(provider))
        .with_no_client_auth();
    config.enable_sni = true;
    config.enable_early_data = false;
    config
}

fn get_insecure_tls_cert<'a>() -> (
    Vec<rustls::pki_types::CertificateDer<'a>>,
    rustls::pki_types::PrivateKeyDer<'a>,
) {
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let cert_der = cert.cert.der().clone();
    let private_key = cert.signing_key.serialize_der();
    let private_key = rustls::pki_types::PrivatePkcs8KeyDer::from(private_key);
    (vec![cert_der], private_key.into())
}

pub(crate) async fn upgrade_accepted<S>(
    stream: S,
    local_url: url::Url,
) -> Result<Box<dyn Tunnel>, TunnelError>
where
    S: VirtualTcpSocket,
{
    let peer_addr = stream.peer_addr()?;
    let mut remote_url = socket_url(local_url.scheme(), peer_addr);
    let stream = if is_wss(&local_url)? {
        let (certificates, private_key) = get_insecure_tls_cert();
        // Do not rely on another tunnel initializing the global provider.
        let config = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .with_context(|| "Failed to select TLS protocol versions")?
        .with_no_client_auth()
        .with_single_cert(certificates, private_key)
        .with_context(|| "Failed to create server config")?;
        Either::Left(TlsAcceptor::from(Arc::new(config)).accept(stream).await?)
    } else {
        Either::Right(stream)
    };

    let (request, stream) = ServerBuilder::new()
        .limits(Limits::unlimited())
        .max_headers(128)
        .accept(stream)
        .await
        .map_err(websocket_error)?;

    if trusted_proxy_contains(peer_addr.ip())
        && let Some(forwarded) = request
            .headers()
            .get("Forwarded")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| ForwardedHeaderValue::from_forwarded(value).ok())
            .or_else(|| {
                request
                    .headers()
                    .get("X-Forwarded-For")
                    .and_then(|value| value.to_str().ok())
                    .and_then(|value| ForwardedHeaderValue::from_x_forwarded_for(value).ok())
            })
        && let Some(ip) = forwarded.remotest_forwarded_for_ip()
    {
        remote_url
            .set_host(Some(&ip.to_string()))
            .map_err(|_| TunnelError::InvalidAddr(format!("invalid forwarded ip {ip}")))?;
        remote_url
            .query_pairs_mut()
            .append_pair("proxy", &peer_addr.to_string());
    }

    let plain_ws = !is_wss(&local_url)?;
    let remote_url: crate::proto::common::Url = remote_url.into();
    let info = TunnelInfo {
        tunnel_type: local_url.scheme().to_owned(),
        local_addr: Some(local_url.into()),
        remote_addr: Some(remote_url.clone()),
        resolved_remote_addr: Some(remote_url),
    };
    if !plain_ws {
        // wss keeps the tokio-websockets pump (TLS record buffering already
        // gives it healthy full-duplex behaviour).
        Ok(build_ws_tunnel(stream, info))
    } else {
        match stream.into_inner() {
            // Plain ws: custom full-duplex data plane on the raw TCP stream.
            Either::Right(tcp) => Ok(build_plain_ws_tunnel(tcp, None, false, info)),
            Either::Left(_) => unreachable!("plain ws upgrade cannot be TLS"),
        }
    }
}

fn socket_url(scheme: &str, addr: SocketAddr) -> url::Url {
    let mut url = url::Url::parse(&format!("{scheme}://0.0.0.0"))
        .expect("WebSocket transport scheme should be a valid URL scheme");
    url.set_ip_host(addr.ip()).unwrap();
    url.set_port(Some(addr.port())).unwrap();
    url
}

#[derive(Debug)]
pub struct WsTunnelListener {
    addr: url::Url,
    listener: Option<TcpListener>,
    socket_mark: Option<u32>,
}

impl WsTunnelListener {
    pub fn new(addr: url::Url) -> Self {
        WsTunnelListener {
            addr,
            listener: None,
            socket_mark: None,
        }
    }

    pub fn set_socket_mark(&mut self, socket_mark: Option<u32>) {
        self.socket_mark = socket_mark;
    }

    async fn listen_tunnel(&mut self) -> Result<(), TunnelError> {
        self.listener = None;

        let addr = SocketAddr::from_url(self.addr.clone(), IpVersion::Both).await?;
        let listener = bind::<TcpListener>()
            .addr(addr)
            .only_v6(true)
            .maybe_socket_mark(self.socket_mark)
            .call()
            .await?;

        self.addr
            .set_port(Some(listener.local_addr()?.port()))
            .unwrap();
        self.listener = Some(listener);

        Ok(())
    }

    async fn accept_tunnel(&mut self) -> Result<Box<dyn Tunnel>, TunnelError> {
        loop {
            let listener = self.listener.as_ref().unwrap();
            // only fail on tcp accept error
            let (stream, _) = listener.accept().await?;
            stream.set_nodelay(true).unwrap();
            match timeout(
                SERVER_HANDSHAKE_TIMEOUT,
                upgrade_accepted(RuntimeTcpSocket::new(stream), self.addr.clone()),
            )
            .await
            {
                Ok(Ok(tunnel)) => return Ok(tunnel),
                e => {
                    tracing::error!(?e, ?self, "Failed to accept ws/wss tunnel");
                    continue;
                }
            }
        }
    }
}

#[async_trait::async_trait]
impl dokitier_core::socket::SocketListener for WsTunnelListener {
    type Accepted = Box<dyn Tunnel>;

    async fn listen(&mut self) -> anyhow::Result<()> {
        Ok(self.listen_tunnel().await?)
    }

    async fn accept(&mut self) -> anyhow::Result<Self::Accepted> {
        Ok(self.accept_tunnel().await?)
    }

    fn local_url(&self) -> url::Url {
        self.addr.clone()
    }
}

pub(crate) async fn upgrade_connected<S>(
    stream: S,
    remote_url: url::Url,
) -> Result<Box<dyn Tunnel>, TunnelError>
where
    S: VirtualTcpSocket,
{
    let is_wss = is_wss(&remote_url)?;
    let local_addr = stream.local_addr()?;
    let resolved_remote_addr = stream.peer_addr()?;
    let info = TunnelInfo {
        tunnel_type: remote_url.scheme().to_owned(),
        local_addr: Some(
            super::build_url_from_socket_addr(&local_addr.to_string(), remote_url.scheme()).into(),
        ),
        remote_addr: Some(remote_url.clone().into()),
        resolved_remote_addr: Some(
            super::build_url_from_socket_addr(
                &resolved_remote_addr.to_string(),
                remote_url.scheme(),
            )
            .into(),
        ),
    };

    let client = ClientBuilder::from_uri(http::Uri::try_from(remote_url.to_string()).unwrap())
        .max_headers(128);
    let stream: MaybeTlsStream<S> = if is_wss {
        let tls = tokio_rustls::TlsConnector::from(Arc::new(get_insecure_tls_client_config()));
        let sni = remote_url.domain().unwrap_or("localhost").to_owned();
        let server_name = rustls::pki_types::ServerName::try_from(sni)
            .map_err(|_| TunnelError::InvalidProtocol("Invalid SNI".to_owned()))?;
        MaybeTlsStream::Rustls(tls.connect(server_name, stream).await?)
    } else {
        MaybeTlsStream::Plain(stream)
    };

    let (client, _) = client.connect_on(stream).await.map_err(websocket_error)?;
    if is_wss {
        Ok(build_ws_tunnel(client, info))
    } else {
        match client.into_inner() {
            MaybeTlsStream::Plain(tcp) => Ok(build_plain_ws_tunnel(tcp, None, true, info)),
            MaybeTlsStream::Rustls(_) => unreachable!("plain ws upgrade cannot be TLS"),
        }
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use dokitier_core::socket::SocketListener;
    use futures::SinkExt;
    use std::io;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpSocket,
    };

    struct FailingWebSocketSink {
        close_called: bool,
    }

    impl Sink<Message> for FailingWebSocketSink {
        type Error = io::Error;

        fn poll_ready(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn start_send(self: Pin<&mut Self>, _item: Message) -> Result<(), Self::Error> {
            Ok(())
        }

        fn poll_flush(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "send failed",
            )))
        }

        fn poll_close(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            self.close_called = true;
            Poll::Ready(Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "close failed",
            )))
        }
    }

    #[tokio::test]
    async fn packet_sink_maps_send_and_close_errors_independently() {
        let mut sink = WebSocketPacketSink::new(FailingWebSocketSink {
            close_called: false,
        });

        sink.send(ZCPacket::new_with_payload(b"packet"))
            .await
            .expect_err("send should fail");
        sink.close().await.expect_err("close should fail");
        assert!(sink.inner.close_called);
    }

    #[tokio::test]
    async fn ws_forwarded() {
        let mut listener = WsTunnelListener::new("ws://127.0.0.1:25559".parse().unwrap());
        listener.listen().await.unwrap();

        let server_task = tokio::spawn(async move {
            let tunnel = listener.accept().await.unwrap();

            let remote_addr = tunnel
                .info()
                .unwrap()
                .remote_addr
                .unwrap()
                .url
                .parse::<url::Url>()
                .unwrap();

            assert_eq!(remote_addr.host_str().unwrap(), "203.0.113.5");
            let proxy_addr = remote_addr
                .query_pairs()
                .find(|(k, _)| k == "proxy")
                .map(|(_, v)| v.into_owned())
                .unwrap();
            assert_eq!(proxy_addr, "127.0.0.1:25560");

            tunnel
        });

        let socket = TcpSocket::new_v4().unwrap();
        socket.bind("127.0.0.1:25560".parse().unwrap()).unwrap();
        let mut stream = socket
            .connect("127.0.0.1:25559".parse().unwrap())
            .await
            .unwrap();

        let handshake = "GET / HTTP/1.1\r\n\
                         Host: 127.0.0.1:25559\r\n\
                         Upgrade: websocket\r\n\
                         Connection: Upgrade\r\n\
                         Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
                         Sec-WebSocket-Version: 13\r\n\
                         X-Forwarded-For: 203.0.113.5, 192.168.1.1\r\n\
                         \r\n";

        stream.write_all(handshake.as_bytes()).await.unwrap();

        let mut buf = [0u8; 1024];
        let bytes_read = stream.read(&mut buf).await.unwrap();
        let response = String::from_utf8_lossy(&buf[..bytes_read]);

        assert!(response.contains("101 Switching Protocols"));

        let _tunnel = server_task.await.unwrap();
    }
}
