//! In-process throughput benchmark for the real deployed WebSocket tunnel
//! path (`dokitier::tunnel::websocket::build_ws_tunnel`, the pump-based
//! pipeline shipped in build93) versus a raw `futures::split` WebSocketStream
//! pipeline, over loopback TCP. No TUN, no instance runtime — isolates the
//! tunnel layer from the rest of the stack.
//!
//! ```text
//! cargo test -p dokitier --test ws_throughput --release -- --ignored --nocapture
//! ```
//!
//! The benchmarks are ignored by default (wall-clock flaky on shared machines).

use std::time::{Duration, Instant};

use dokitier::proto::common::TunnelInfo;
use dokitier::tunnel::websocket::build_ws_tunnel;
use dokitier_core::packet::ZCPacket;
use futures::{SinkExt, StreamExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_websockets::{ClientBuilder, Message, ServerBuilder, WebSocketStream};

const PAYLOAD: usize = 1400;
const PACKETS: usize = 250_000; // ~350 MB one way

fn tunnel_info() -> TunnelInfo {
    TunnelInfo {
        tunnel_type: "ws".to_owned(),
        local_addr: Some("ws://127.0.0.1:1".parse::<url::Url>().unwrap().into()),
        remote_addr: None,
        resolved_remote_addr: None,
    }
}

async fn echo_server(stream: TcpStream) {
    let (_req, ws) = ServerBuilder::new().accept(stream).await.expect("upgrade");
    let tunnel = build_ws_tunnel(ws, tunnel_info());
    let (mut stream, mut sink) = tunnel.split();
    while let Some(item) = futures::StreamExt::next(&mut stream).await {
        match item {
            Ok(packet) => {
                if futures::SinkExt::feed(&mut sink, packet).await.is_err() {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    let _ = futures::SinkExt::flush(&mut sink).await;
}

async fn echo_server_raw(stream: TcpStream) {
    let (_req, ws) = ServerBuilder::new().accept(stream).await.expect("upgrade");
    let (mut sink, mut stream): (
        futures::stream::SplitSink<WebSocketStream<TcpStream>, Message>,
        _,
    ) = ws.split();
    while let Some(item) = stream.next().await {
        match item {
            Ok(m) => {
                if sink.feed(Message::binary(m.into_payload().to_vec())).await.is_err() {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    let _ = sink.flush().await;
}

async fn run_client(
    stream: TcpStream,
    raw: bool,
) -> (usize, Duration) {
    let (client, _) = ClientBuilder::from_uri("ws://127.0.0.1:1".parse().unwrap())
        .connect_on(stream)
        .await
        .expect("upgrade");
    let payload = vec![0u8; PAYLOAD];
    let mut received = 0usize;
    let start;
    // Interleave send and read: a send-all-then-read echo client deadlocks
    // once the in-flight data exceeds both socket buffers.
    if raw {
        let (mut sink, mut stream): (
            futures::stream::SplitSink<WebSocketStream<TcpStream>, Message>,
            _,
        ) = client.split();
        start = Instant::now();
        let send_task = tokio::spawn(async move {
            for _ in 0..PACKETS {
                futures::SinkExt::feed(&mut sink, Message::binary(payload.clone()))
                    .await
                    .expect("feed");
            }
            futures::SinkExt::flush(&mut sink).await.expect("flush");
        });
        while received < PACKETS {
            match stream.next().await {
                Some(Ok(m)) => {
                    let _ = m;
                    received += 1;
                }
                _ => break,
            }
        }
        send_task.await.expect("send task");
    } else {
        let tunnel = build_ws_tunnel(client, tunnel_info());
        let (mut stream, mut sink) = tunnel.split();
        start = Instant::now();
        let send_task = tokio::spawn(async move {
            for _ in 0..PACKETS {
                futures::SinkExt::feed(&mut sink, ZCPacket::new_with_payload(&payload))
                    .await
                    .expect("feed");
            }
            futures::SinkExt::flush(&mut sink).await.expect("flush");
        });
        while received < PACKETS {
            match stream.next().await {
                Some(Ok(packet)) => {
                    let _ = packet;
                    received += 1;
                }
                _ => break,
            }
        }
        send_task.await.expect("send task");
    }
    (received, start.elapsed())
}

async fn bench(raw: bool) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept");
        if raw {
            echo_server_raw(stream).await;
        } else {
            echo_server(stream).await;
        }
    });
    let stream = TcpStream::connect(addr).await.expect("connect");
    let (received, elapsed) = run_client(stream, raw).await;
    assert_eq!(received, PACKETS, "echo count mismatch");
    let total = (PAYLOAD * PACKETS) as f64 / 1e6;
    let mbps = total / elapsed.as_secs_f64() * 8.0;
    let label = if raw { "raw-split" } else { "dokitier-pump" };
    println!("{label}: echoed {received} packets ({total:.0} MB) in {elapsed:.2?} -> {mbps:.0} Mbps");
}

#[tokio::main(worker_threads = 2)]
async fn main() {
    // Unused main for a test binary; the tests below drive everything.
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn throughput_pump_vs_raw() {
    match std::env::var("WS_BENCH_MODE").as_deref() {
        Ok("pump") => bench(false).await,
        Ok("raw") => bench(true).await,
        _ => {
            bench(false).await;
            bench(true).await;
        }
    }
}

// Silence unusedDuration warning if not referenced on some feature sets.
#[allow(dead_code)]
fn _unused(_d: Duration) {}
