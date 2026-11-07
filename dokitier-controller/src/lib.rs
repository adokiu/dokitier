//! Shared protocol between dokitier core agents and the standalone console.
//!
//! Frames are UTF-8 JSON. TCP uses a 32-bit big-endian length prefix so the
//! same payload can also be carried by WebSocket text frames.

use anyhow::{anyhow, bail};
use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const MAX_FRAME: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentFrame {
    Hello {
        node_id: String,
        token: String,
        #[serde(default)]
        hostname: String,
        #[serde(default)]
        version: String,
        #[serde(default)]
        networks: Vec<serde_json::Value>,
    },
    Heartbeat {
        node_id: String,
        #[serde(default)]
        token: String,
        #[serde(default)]
        ts: i64,
    },
    Snapshot {
        node_id: String,
        data: serde_json::Value,
    },
    Result {
        node_id: String,
        request_id: String,
        ok: bool,
        #[serde(default)]
        error: Option<String>,
    },
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ConsoleFrame {
    Ack,
    Error { error: String },
    Command {
        request_id: String,
        command: ConsoleCommand,
    },
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ConsoleCommand {
    GetConfig,
    ApplyConfig { toml: String },
    RestartInstance,
}

pub fn hash_token(token: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    B64.encode(hasher.finalize())
}

pub async fn read_frame<R: AsyncRead + Unpin>(reader: &mut R) -> anyhow::Result<serde_json::Value> {
    let mut len = [0u8; 4];
    reader.read_exact(&mut len).await?;
    let len = u32::from_be_bytes(len) as usize;
    if len == 0 || len > MAX_FRAME {
        bail!("invalid frame length: {len}");
    }
    let mut buf = vec![0u8; len];
    reader.read_exact(&mut buf).await?;
    serde_json::from_slice(&buf).map_err(|e| anyhow!("invalid frame JSON: {e}"))
}

pub async fn write_frame<W: AsyncWrite + Unpin>(
    writer: &mut W,
    value: &serde_json::Value,
) -> anyhow::Result<()> {
    let buf = serde_json::to_vec(value)?;
    if buf.len() > MAX_FRAME {
        bail!("frame too large");
    }
    writer.write_all(&(buf.len() as u32).to_be_bytes()).await?;
    writer.write_all(&buf).await?;
    writer.flush().await?;
    Ok(())
}
