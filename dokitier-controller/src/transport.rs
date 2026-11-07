//! Agent-side transport abstraction: TCP (length-prefixed JSON) and WebSocket.

use anyhow::{anyhow, bail};
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpStream, UdpSocket},
};
use tokio_tungstenite::{connect_async, tungstenite::Message, MaybeTlsStream, WebSocketStream};

use crate::{read_frame, write_frame};

pub enum AgentTransport {
    Tcp(TcpStream),
    Ws(WebSocketStream<MaybeTlsStream<TcpStream>>),
    Udp { socket: UdpSocket, peer: std::net::SocketAddr },
}

impl AgentTransport {
    pub async fn connect(url: &str) -> anyhow::Result<Self> {
        if let Some(rest) = url.strip_prefix("tcp://") {
            let stream = TcpStream::connect(rest).await?;
            return Ok(Self::Tcp(stream));
        }
        if let Some(rest) = url.strip_prefix("udp://") {
            let peer = tokio::net::lookup_host(rest).await?.next().ok_or_else(|| anyhow!("no address for {rest}"))?;
            let bind = if peer.is_ipv6() { "[::]:0" } else { "0.0.0.0:0" };
            let socket = UdpSocket::bind(bind).await?;
            socket.connect(peer).await?;
            return Ok(Self::Udp { socket, peer });
        }
        if url.starts_with("ws://") || url.starts_with("wss://") {
            let (ws, _) = connect_async(url).await?;
            return Ok(Self::Ws(ws));
        }
        bail!("unsupported controller transport: {url}")
    }

    pub async fn send(&mut self, value: &Value) -> anyhow::Result<()> {
        match self {
            Self::Tcp(stream) => write_frame(stream, value).await,
            Self::Ws(ws) => {
                let text = serde_json::to_string(value)?;
                ws.send(Message::Text(text.into())).await?;
                Ok(())
            }
            Self::Udp { socket, .. } => {
                let buf = serde_json::to_vec(value)?;
                socket.send(&buf).await?;
                Ok(())
            }
        }
    }

    /// Receive one frame; TCP payloads are length-prefixed, WS payloads are text/binary JSON.
    pub async fn recv(&mut self) -> anyhow::Result<Value> {
        match self {
            Self::Tcp(stream) => read_frame(stream).await,
            Self::Udp { socket, peer } => {
                let mut buf = vec![0u8; 64 * 1024];
                let n = socket.recv(&mut buf).await?;
                let _ = peer;
                serde_json::from_slice(&buf[..n]).map_err(|e| anyhow!("invalid JSON frame: {e}"))
            }
            Self::Ws(ws) => loop {
                match ws.next().await {
                    Some(Ok(Message::Text(text))) => {
                        return serde_json::from_str(&text).map_err(|e| anyhow!("invalid JSON frame: {e}"));
                    }
                    Some(Ok(Message::Binary(bin))) => {
                        return serde_json::from_slice(&bin).map_err(|e| anyhow!("invalid JSON frame: {e}"));
                    }
                    Some(Ok(Message::Close(_))) | None => bail!("controller closed websocket"),
                    Some(Ok(_)) => continue,
                    Some(Err(err)) => return Err(err.into()),
                }
            },
        }
    }
}

// Keep a tiny length-prefix reader for TCP handshake tests.
pub async fn read_raw_frame(stream: &mut TcpStream) -> anyhow::Result<Value> {
    let mut len = [0u8; 4];
    stream.read_exact(&mut len).await?;
    let len = u32::from_be_bytes(len) as usize;
    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf).await?;
    serde_json::from_slice(&buf).map_err(|e| e.into())
}

pub async fn write_raw_frame(stream: &mut TcpStream, value: &Value) -> anyhow::Result<()> {
    let buf = serde_json::to_vec(value)?;
    stream.write_all(&(buf.len() as u32).to_be_bytes()).await?;
    stream.write_all(&buf).await?;
    Ok(())
}
