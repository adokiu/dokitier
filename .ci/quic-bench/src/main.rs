//! Isolated quinn benchmark: one connection, one bidirectional stream,
//! 1400-byte bulk payloads in one or both directions — replicates the
//! dokitier QUIC tunnel usage to reproduce the bidirectional collapse.
//!
//! Usage: quic-bench [unidir|bidir]
use std::{net::UdpSocket, sync::Arc, time::Instant};

use quinn::{ClientConfig, Connection, Endpoint, ServerConfig};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};

const PAYLOAD: usize = 1400;
const MESSAGES: usize = 300_000; // ~420 MB one way

type Certs = (CertificateDer<'static>, PrivatePkcs8KeyDer<'static>);

fn make_certs() -> Certs {
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let key = PrivatePkcs8KeyDer::from(cert.key_pair.serialize_der());
    (cert.cert.into(), key)
}

fn transport_config() -> quinn::TransportConfig {
    let mut config = quinn::TransportConfig::default();
    config
        .max_concurrent_bidi_streams(u8::MAX.into())
        .max_concurrent_uni_streams(0u8.into())
        .keep_alive_interval(Some(std::time::Duration::from_secs(5)))
        .initial_mtu(1472)
        .min_mtu(1200)
        .enable_segmentation_offload(false)
        .congestion_controller_factory(Arc::new(quinn::congestion::CubicConfig::default()));
    config
}

fn bind_udp(addr: std::net::SocketAddr) -> UdpSocket {
    let socket = UdpSocket::bind(addr).unwrap();
    socket.set_nonblocking(true).unwrap();
    socket
}

fn server_endpoint(certs: Certs) -> Endpoint {
    let socket = bind_udp("127.0.0.1:24444".parse().unwrap());
    let (cert, key) = certs;
    let mut tls = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert], PrivateKeyDer::Pkcs8(key))
        .unwrap();
    tls.alpn_protocols = vec![b"bench".to_vec()];
    let quic_crypto = quinn::crypto::rustls::QuicServerConfig::try_from(tls).unwrap();
    let mut config = ServerConfig::with_crypto(Arc::new(quic_crypto));
    config.transport_config(Arc::new(transport_config()));
    let runtime = quinn::default_runtime().unwrap();
    Endpoint::new(
        quinn::EndpointConfig::default(),
        Some(config),
        socket,
        runtime,
    )
    .unwrap()
}

fn client_endpoint(cert: CertificateDer<'static>) -> Endpoint {
    let socket = bind_udp("127.0.0.1:0".parse().unwrap());
    let mut roots = rustls::RootCertStore::empty();
    roots.add(cert).unwrap();
    let mut tls = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    tls.alpn_protocols = vec![b"bench".to_vec()];
    let quic_crypto = quinn::crypto::rustls::QuicClientConfig::try_from(tls).unwrap();
    let mut config = ClientConfig::new(Arc::new(quic_crypto));
    config.transport_config(Arc::new(transport_config()));
    let runtime = quinn::default_runtime().unwrap();
    let mut endpoint = Endpoint::new(
        quinn::EndpointConfig::default(),
        None,
        socket,
        runtime,
    )
    .unwrap();
    endpoint.set_default_client_config(config);
    endpoint
}

async fn server(connection: Connection, bidir: bool) {
    let (mut send, mut recv) = connection.accept_bi().await.expect("accept bi");
    let mut buf = vec![0u8; 64 * 1024];
    let mut count = 0usize;
    loop {
        match recv.read(&mut buf).await {
            Ok(Some(0)) | Ok(None) => break,
            Ok(Some(n)) => {
                if bidir {
                    if send.write_all(&buf[..n]).await.is_err() {
                        break;
                    }
                }
                count += 1;
                if count >= MESSAGES {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    if bidir {
        let _ = send.finish();
    }
    println!("server forwarded {count} units");
}

#[tokio::main(worker_threads = 2)]
async fn main() {
    let mode = std::env::args().nth(1).unwrap_or_else(|| "unidir".into());
    let bidir = mode == "bidir";
    let certs = make_certs();
    let cert = certs.0.clone();
    let server_ep = server_endpoint(certs);
    let server_addr = server_ep.local_addr().unwrap();
    tokio::spawn(async move {
        if let Some(connecting) = server_ep.accept().await {
            let conn = connecting.await.expect("server handshake");
            server(conn, bidir).await;
        }
    });

    let client_ep = client_endpoint(cert);
    let connection = client_ep
        .connect(server_addr, "localhost")
        .expect("connect")
        .await
        .expect("client handshake");
    let (mut send, mut recv) = connection.open_bi().await.expect("open bi");
    let payload = vec![0u8; PAYLOAD];
    let start = Instant::now();
    let mut received = 0usize;
    if bidir {
        let send_task = tokio::spawn(async move {
            for _ in 0..MESSAGES {
                if send.write_all(&payload).await.is_err() {
                    break;
                }
            }
            let _ = send.finish();
        });
        let mut buf = vec![0u8; 64 * 1024];
        while received < MESSAGES {
            match recv.read(&mut buf).await {
                Ok(Some(0)) | Ok(None) => break,
                Ok(Some(_)) => received += 1,
                Err(_) => break,
            }
        }
        send_task.await.expect("send task");
    } else {
        for _ in 0..MESSAGES {
            send.write_all(&payload).await.expect("send");
        }
        let _ = send.finish();
        let mut buf = vec![0u8; 64 * 1024];
        while let Ok(Some(n)) = recv.read(&mut buf).await {
            if n == 0 {
                break;
            }
        }
    }
    let elapsed = start.elapsed();
    let total = (PAYLOAD * MESSAGES) as f64 / 1e6;
    let mbps = total / elapsed.as_secs_f64() * 8.0;
    let label = if bidir { "bidir(echo)" } else { "unidir" };
    println!("{label}: {elapsed:.2?} for {total:.0} MB -> {mbps:.0} Mbps");
    client_ep.close(0u32.into(), b"done");
}
