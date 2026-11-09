//! Agent-side transport abstraction: TCP (length-prefixed JSON) and WebSocket.

use anyhow::{anyhow, bail};
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;

use crate::{read_frame, write_frame};
use tokio::net::TcpStream;
use tokio_tungstenite::{connect_async, tungstenite::Message, MaybeTlsStream, WebSocketStream};



pub enum AgentTransport {
    Ws(WebSocketStream<MaybeTlsStream<TcpStream>>),
    Quic { endpoint: quinn::Endpoint, send: quinn::SendStream, recv: quinn::RecvStream },
}

#[derive(Debug)]
struct SkipServerVerification(std::sync::Arc<rustls::crypto::CryptoProvider>);

impl rustls::client::danger::ServerCertVerifier for SkipServerVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

impl AgentTransport {
    pub async fn connect(url: &str) -> anyhow::Result<Self> {
        if let Some(rest) = url.strip_prefix("quic://") {
            let addr = tokio::net::lookup_host(rest).await?.next().ok_or_else(|| anyhow!("no address for {rest}"))?;
            let provider = rustls::crypto::ring::default_provider();
            let mut crypto = rustls::ClientConfig::builder_with_provider(provider.into())
                .with_protocol_versions(&[&rustls::version::TLS13])?
                .dangerous()
                .with_custom_certificate_verifier(std::sync::Arc::new(SkipServerVerification(
                    rustls::crypto::ring::default_provider().into(),
                )))
                .with_no_client_auth();
            crypto.alpn_protocols = vec![b"dokitier".to_vec()];
            let mut client_config = quinn::ClientConfig::new(std::sync::Arc::new(
                quinn::crypto::rustls::QuicClientConfig::try_from(crypto)?,
            ));
            let mut transport = quinn::TransportConfig::default();
            transport.max_concurrent_bidi_streams(100u32.into());
            client_config.transport_config(std::sync::Arc::new(transport));
            let mut endpoint = quinn::Endpoint::client("0.0.0.0:0".parse()?)?;
            endpoint.set_default_client_config(client_config);
            let conn = match endpoint.connect(addr, "localhost")?.await {
                Ok(conn) => conn,
                Err(err) => { eprintln!("dokitier controller: quic connect failed: {err}"); return Err(err.into()); }
            };
            let (send, recv) = match conn.open_bi().await {
                Ok(streams) => streams,
                Err(err) => { eprintln!("dokitier controller: quic open_bi failed: {err}"); return Err(err.into()); }
            };
            eprintln!("dokitier controller: quic transport connected to {addr}");
            tracing::info!(%addr, "quic transport connected");
            return Ok(Self::Quic { endpoint, send, recv });
        }
        if let Some(rest) = url.strip_prefix("wss://") {
            // Console certificates are self-signed; verify-skip mirrors the
            // QUIC agent path above.
            let _ = rest;
            let provider = rustls::crypto::ring::default_provider();
            let mut crypto = rustls::ClientConfig::builder_with_provider(provider.into())
                .with_safe_default_protocol_versions()?
                .dangerous()
                .with_custom_certificate_verifier(std::sync::Arc::new(SkipServerVerification(
                    rustls::crypto::ring::default_provider().into(),
                )))
                .with_no_client_auth();
            crypto.alpn_protocols = vec![b"http/1.1".to_vec()];
            let connector = tokio_tungstenite::Connector::Rustls(std::sync::Arc::new(crypto));
            let (ws, _) = tokio_tungstenite::connect_async_tls_with_config(
                url,
                None,
                false,
                Some(connector),
            )
            .await?;
            return Ok(Self::Ws(ws));
        }
        if url.starts_with("ws://") {
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
            Self::Quic { send, .. } => write_frame(send, value).await,
        }
    }

    /// Receive one frame (WS text/binary JSON, or a UDP datagram).
    pub async fn recv(&mut self) -> anyhow::Result<Value> {
        match self {
            Self::Quic { recv, .. } => read_frame(recv).await,
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

