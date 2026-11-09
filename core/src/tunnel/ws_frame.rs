//! Minimal WebSocket data-plane framing for the plain `ws://` tunnel.
//!
//! Why this exists: with any design that polls a single `WebSocketStream` from
//! one task (sequential, BiLock-split across two tasks, or a `select!` pump),
//! full-duplex bulk traffic collapses — the write side waits for the socket to
//! drain while the read side waits for the same task to come back, and the
//! weaker direction starves (measured: 400 Mbps unidirectional, 10-46 Mbps
//! under bidirectional load, while the raw library does 9 Gbps on loopback).
//!
//! This module takes over the data path after the HTTP upgrade:
//! `WebSocketStream::into_inner()` recovers the underlying stream, which is
//! split into independently-polled read and write halves, each driven by its
//! own task. Wire format is standard WebSocket (RFC 6455) binary frames, so
//! dokitier peers, the shared listeners and EasyTier-compatible endpoints all
//! interoperate unchanged. The `wss://` path keeps tokio-websockets (TLS
//! record buffering already gives it full-duplex behaviour).

use bytes::{BufMut, Bytes, BytesMut};
use dokitier_core::packet::ZCPacket;
use dokitier_core::tunnel::TunnelError;
use futures::Sink;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub(super) const OP_CONT: u8 = 0x0;
pub(super) const OP_TEXT: u8 = 0x1;
pub(super) const OP_BINARY: u8 = 0x2;
pub(super) const OP_CLOSE: u8 = 0x8;
pub(super) const OP_PING: u8 = 0x9;
pub(super) const OP_PONG: u8 = 0xA;

/// Appends a WebSocket frame header for `payload_len` bytes.
///
/// `mask_key` is `Some` for client→server frames (RFC 6455 §5.3) and `None`
/// for server→client frames.
pub(super) fn put_frame_header(
    buf: &mut BytesMut,
    opcode: u8,
    payload_len: usize,
    mask_key: Option<[u8; 4]>,
    fin: bool,
) {
    let mut b0 = 0x80u8 | (opcode & 0x0f); // FIN set; our frames are unfragmented
    if !fin {
        b0 &= !0x80u8;
    }
    buf.put_u8(b0);
    let mask_bit = if mask_key.is_some() { 0x80u8 } else { 0 };
    if payload_len < 126 {
        buf.put_u8(mask_bit | payload_len as u8);
    } else if payload_len <= u16::MAX as usize {
        buf.put_u8(mask_bit | 126);
        buf.put_u16(payload_len as u16);
    } else {
        buf.put_u8(mask_bit | 127);
        buf.put_u64(payload_len as u64);
    }
    if let Some(key) = mask_key {
        buf.put_slice(&key);
    }
}

/// XOR-applies the WebSocket masking key in place (self-inverse).
pub(super) fn mask_payload(key: &[u8; 4], payload: &mut [u8]) {
    for (i, byte) in payload.iter_mut().enumerate() {
        *byte ^= key[i % 4];
    }
}

/// One parsed frame: opcode + payload (unmasked).
pub(super) struct WsFrame {
    pub opcode: u8,
    pub fin: bool,
    pub payload: BytesMut,
}

/// Reads one complete frame from `rd` using `buf` as the read buffer.
///
/// Returns `Ok(None)` when the peer sent a CLOSE frame (or closed the stream).
/// `expect_masked` mirrors the role: the server receives masked frames, the
/// client receives unmasked ones.
pub(super) async fn read_frame<R: AsyncRead + Unpin>(
    rd: &mut R,
    buf: &mut BytesMut,
) -> std::io::Result<Option<WsFrame>> {
    loop {
        // Frame header: 2 bytes, optional 2/8-byte extended length, optional 4-byte mask.
        if buf.len() < 2 {
            rd.read_buf(buf).await?;
            continue;
        }
        let b0 = buf[0];
        let b1 = buf[1];
        let opcode = b0 & 0x0f;
        let fin = b0 & 0x80 != 0;
        let masked = b1 & 0x80 != 0;
        let raw_len = (b1 & 0x7f) as usize;
        let ext_len = if raw_len == 126 {
            2
        } else if raw_len == 127 {
            8
        } else {
            0
        };
        let header_len = 2 + ext_len + if masked { 4 } else { 0 };
        while buf.len() < header_len {
            rd.read_buf(buf).await?;
        }
        let payload_len = if ext_len == 2 {
            u16::from_be_bytes([buf[2], buf[3]]) as usize
        } else if ext_len == 8 {
            u64::from_be_bytes(buf[2..10].try_into().unwrap()) as usize
        } else {
            raw_len
        };
        while buf.len() < header_len + payload_len {
            rd.read_buf(buf).await?;
            if payload_len == 0 {
                break;
            }
        }
        let mut frame = buf.split_to(header_len + payload_len);
        let mut payload = frame.split_off(header_len);
        // The header portion is [2 B base][ext len][4 B mask]; the mask key is
        // its last 4 bytes.
        let mask_key: Option<[u8; 4]> = if masked {
            let key: [u8; 4] = frame[header_len - 4..header_len].try_into().unwrap();
            Some(key)
        } else {
            None
        };
        if let Some(key) = mask_key {
            // Server role: peer frames are masked; unmask in place.
            mask_payload(&key, &mut payload);
        }
        if opcode == OP_CLOSE {
            return Ok(None);
        }
        return Ok(Some(WsFrame {
            opcode,
            fin,
            payload,
        }));
    }
}

/// Control frames the reader task hands to the writer task (pong/close).
#[derive(Debug)]
pub(super) enum WsControl {
    Pong(Bytes),
    Close,
}

/// Builds a pong (or close) control frame, masked when `mask` is set.
pub(super) fn control_frame(opcode: u8, payload: &[u8], mask: bool) -> Vec<u8> {
    let mask_key = if mask {
        Some(rand_mask_key())
    } else {
        None
    };
    let mut buf = BytesMut::with_capacity(14 + payload.len());
    put_frame_header(&mut buf, opcode, payload.len(), mask_key, true);
    buf.put_slice(payload);
    let mut out = buf.to_vec();
    if let Some(key) = mask_key {
        // Mask only the payload bytes (they follow the header).
        let header_len = out.len() - payload.len();
        mask_payload(&key, &mut out[header_len..]);
    }
    out
}

pub(super) fn rand_mask_key() -> [u8; 4] {
    rand::random::<u32>().to_ne_bytes()
}

/// Poll-based writer half used by the tunnel sink. Mirrors `WsChannelSink`.
pub(super) struct WsFrameSink {
    tx: tokio_util::sync::PollSender<ZCPacket>,
}

impl WsFrameSink {
    pub(super) fn new(tx: tokio_util::sync::PollSender<ZCPacket>) -> Self {
        Self { tx }
    }
}

impl Sink<ZCPacket> for WsFrameSink {
    type Error = TunnelError;

    fn poll_ready(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(), Self::Error>> {
        Pin::new(&mut self.tx)
            .poll_reserve(cx)
            .map_err(|_| TunnelError::Shutdown)
    }

    fn start_send(mut self: Pin<&mut Self>, item: ZCPacket) -> Result<(), Self::Error> {
        Pin::new(&mut self.tx)
            .send_item(item)
            .map_err(|_| TunnelError::Shutdown)
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn poll_close(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }
}

/// Poll-based reader half used by the tunnel stream. Mirrors `WsChannelStream`.
pub(super) struct WsFrameStream {
    rx: tokio::sync::mpsc::Receiver<Result<ZCPacket, TunnelError>>,
}

impl WsFrameStream {
    pub(super) fn new(
        rx: tokio::sync::mpsc::Receiver<Result<ZCPacket, TunnelError>>,
    ) -> Self {
        Self { rx }
    }
}

impl futures::Stream for WsFrameStream {
    type Item = Result<ZCPacket, TunnelError>;

    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.rx).poll_recv(cx)
    }
}
