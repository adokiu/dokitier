//! Agent-side transport abstraction: TCP (length-prefixed JSON) and WebSocket.

use anyhow::{anyhow, bail};
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::net::TcpStream;
use tokio_tungstenite::{connect_async, tungstenite::Message, MaybeTlsStream, WebSocketStream};



pub enum AgentTransport {
    Ws(WebSocketStream<MaybeTlsStream<TcpStream>>),
}

impl AgentTransport {
    pub async fn connect(url: &str) -> anyhow::Result<Self> {
        if url.starts_with("ws://") || url.starts_with("wss://") {
            let (ws, _) = connect_async(url).await?;
            return Ok(Self::Ws(ws));
        }
        bail!("unsupported controller transport: {url}")
    }

    pub async fn send(&mut self, value: &Value) -> anyhow::Result<()> {
        match self {
            Self::Ws(ws) => {
                let text = serde_json::to_string(value)?;
                ws.send(Message::Text(text.into())).await?;
                Ok(())
            }
        }
    }

    /// Receive one frame (WS text/binary JSON, or a UDP datagram).
    pub async fn recv(&mut self) -> anyhow::Result<Value> {
        match self {
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

