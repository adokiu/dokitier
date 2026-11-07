//! Shared protocol between dokitier core agents and the standalone console.
//!
//! Frames are UTF-8 JSON. TCP uses a 32-bit big-endian length prefix so the
//! same payload can also be carried by WebSocket text frames.

use anyhow::{anyhow, bail};
use aes_gcm::{Aes256Gcm, KeyInit, Nonce, aead::{Aead, OsRng, rand_core::RngCore}};
use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub mod transport;

pub const MAX_FRAME: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ConsoleFrame {
    Challenge {
        nonce: String,
        ts: i64,
    },
    ChallengeOk {
        proof: String,
    },
    Ack,
    Error { error: String },
    Command {
        request_id: String,
        command: ConsoleCommand,
    },
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentFrame {
    Hello {
        node_id: String,
        #[serde(default)]
        nonce: String,
        #[serde(default)]
        ts: i64,
        #[serde(default)]
        proof: String,
        #[serde(default)]
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
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ConsoleCommand {
    GetConfig,
    ApplyConfig { toml: String },
    RestartInstance,
}

pub fn proof(token: &str, role: &str, node_id: &str, nonce: &str, ts: i64) -> String {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(token.as_bytes()).expect("hmac key");
    mac.update(role.as_bytes());
    mac.update(b"|");
    mac.update(node_id.as_bytes());
    mac.update(b"|");
    mac.update(nonce.as_bytes());
    mac.update(b"|");
    mac.update(ts.to_string().as_bytes());
    B64.encode(mac.finalize().into_bytes())
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

/// Encrypted session envelope. Both sides derive the same key from
/// token + challenge nonce + timestamp, so no extra key exchange is needed.
pub struct SessionCipher {
    cipher: Aes256Gcm,
}

impl SessionCipher {
    pub fn new(token: &str, nonce: &str, ts: i64) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(b"dokitier-session|");
        hasher.update(token.as_bytes());
        hasher.update(b"|");
        hasher.update(nonce.as_bytes());
        hasher.update(b"|");
        hasher.update(ts.to_string().as_bytes());
        let key = hasher.finalize();
        Self { cipher: Aes256Gcm::new(&key) }
    }

    pub fn encrypt(&self, value: &serde_json::Value) -> serde_json::Value {
        let plaintext = serde_json::to_vec(value).unwrap_or_default();
        let mut nonce_bytes = [0u8; 12];
        OsRng.fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);
        let ciphertext = self.cipher.encrypt(nonce, plaintext.as_ref()).unwrap_or_default();
        let mut out = nonce_bytes.to_vec();
        out.extend_from_slice(&ciphertext);
        serde_json::json!({"type": "enc", "data": B64.encode(out)})
    }

    pub fn decrypt(&self, value: &serde_json::Value) -> Option<serde_json::Value> {
        let data = value.get("data")?.as_str()?;
        let raw = B64.decode(data).ok()?;
        if raw.len() < 13 { return None; }
        let (nonce_bytes, ciphertext) = raw.split_at(12);
        let plaintext = self.cipher.decrypt(Nonce::from_slice(nonce_bytes), ciphertext).ok()?;
        serde_json::from_slice(&plaintext).ok()
    }
}
