//! Isolated tokio-websockets throughput benchmark: in-process client+server
//! over a loopback TcpStream, 1400-byte binary messages, unidir and bidir.
//! Mirrors the dokitier pump usage (SinkExt::feed batches + flush).
use std::time::Instant;

use futures::{SinkExt, StreamExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_websockets::{ClientBuilder, Message, ServerBuilder, WebSocketStream};

const PAYLOAD: usize = 1400;
const MESSAGES: usize = 600_000; // ~840 MB

type Ws = WebSocketStream<TcpStream>;

/// Mirrors dokitier's `ws_pump` (core/src/tunnel/websocket.rs): single task
/// owns the stream, outgoing packets arrive over an mpsc channel, batches are
/// fed and flushed, incoming frames forwarded to an mpsc channel.
async fn pump(
    mut ws: Ws,
    mut outgoing: tokio::sync::mpsc::Receiver<Message>,
    incoming: tokio::sync::mpsc::Sender<Message>,
) {
    let mut batch: Vec<Message> = Vec::with_capacity(32);
    loop {
        while batch.len() < 32 {
            match outgoing.try_recv() {
                Ok(m) => batch.push(m),
                Err(_) => break,
            }
        }
        if !batch.is_empty() {
            for m in batch.drain(..) {
                if ws.feed(m).await.is_err() {
                    return;
                }
            }
            if ws.flush().await.is_err() {
                return;
            }
            continue;
        }
        tokio::select! {
            message = ws.next() => {
                match message {
                    Some(Ok(m)) => {
                        if incoming.send(m).await.is_err() {
                            return;
                        }
                    }
                    _ => return,
                }
            }
            packet = outgoing.recv() => {
                match packet {
                    Some(m) => batch.push(m),
                    None => return,
                }
            }
        }
    }
}

/// Channels exposed as sink/stream, like dokitier's WsChannelSink/WsChannelStream.
struct ChanSink {
    tx: tokio_util::sync::PollSender<Message>,
}

impl futures::Sink<Message> for ChanSink {
    type Error = tokio_websockets::Error;
    fn poll_ready(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::pin::Pin::new(&mut self.tx)
            .poll_reserve(cx)
            .map_err(|_| tokio_websockets::Error::AlreadyClosed)
    }
    fn start_send(
        mut self: std::pin::Pin<&mut Self>,
        item: Message,
    ) -> Result<(), Self::Error> {
        std::pin::Pin::new(&mut self.tx)
            .send_item(item)
            .map_err(|_| tokio_websockets::Error::AlreadyClosed)
    }
    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }
    fn poll_close(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }
}

struct ChanStream {
    rx: tokio::sync::mpsc::Receiver<Message>,
}

impl futures::Stream for ChanStream {
    type Item = Result<Message, tokio_websockets::Error>;
    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        match std::pin::Pin::new(&mut self.rx).poll_recv(cx) {
            std::task::Poll::Ready(Some(m)) => std::task::Poll::Ready(Some(Ok(m))),
            std::task::Poll::Ready(None) => std::task::Poll::Ready(None),
            std::task::Poll::Pending => std::task::Poll::Pending,
        }
    }
}

async fn server(stream: TcpStream, bidir: bool, done: tokio::sync::oneshot::Sender<()>) {
    let (_req, ws) = ServerBuilder::new().accept(stream).await.expect("upgrade");
    if bidir {
        // dokitier-style pump on the server side: inbound frames forwarded to a
        // channel (consumed by the "peer connection" task), egress frames fed
        // from another channel at full rate.
        let (in_tx, mut in_rx) = tokio::sync::mpsc::channel::<Message>(128);
        let (out_tx, out_rx) = tokio::sync::mpsc::channel::<Message>(128);
        // consumer: drop inbound as fast as possible (stand-in for PeerConn)
        tokio::spawn(async move {
            while in_rx.recv().await.is_some() {}
        });
        // egress generator: push MESSAGES frames as fast as the channel takes
        tokio::spawn(async move {
            let payload = vec![0u8; PAYLOAD];
            for _ in 0..MESSAGES {
                if out_tx.send(Message::binary(payload.clone())).await.is_err() {
                    return;
                }
            }
        });
        pump(ws, out_rx, in_tx).await;
        let _ = done.send(());
        return;
    }
    let (mut sink, mut stream) = ws.split();
    let mut count = 0usize;
    while count < MESSAGES {
        match stream.next().await {
            Some(Ok(m)) => {
                if bidir {
                    sink.feed(Message::binary(m.into_payload().to_vec()))
                        .await
                        .expect("feed");
                    count += 1;
                    if count % 32 == 0 {
                        sink.flush().await.expect("flush");
                    }
                } else {
                    count += 1;
                }
            }
            _ => break,
        }
    }
    if bidir {
        sink.flush().await.expect("flush");
    }
    let _ = done.send(());
}

#[tokio::main(worker_threads = 2)]
async fn main() {
    let mode = std::env::args().nth(1).unwrap_or_default();
    let bidir = mode == "bidir";
    let use_pump = mode == "pump" || mode == "srv";
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().unwrap();

    let (done_tx, done_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept");
        server(stream, mode == "srv", done_tx).await;
    });

    let stream = TcpStream::connect(addr).await.expect("connect");
    let (client, _) = ClientBuilder::from_uri(format!("ws://{addr}").parse().expect("uri"))
        .connect_on(stream)
        .await
        .expect("upgrade");

    let payload = vec![0u8; PAYLOAD];
    let start = Instant::now();
    let mut received = 0usize;
    if use_pump {
        // dokitier-style pipeline: client sink -> channel -> pump -> ws
        let (in_tx, mut in_rx) = tokio::sync::mpsc::channel::<Message>(128);
        let (out_tx, out_rx) = tokio::sync::mpsc::channel::<Message>(128);
        tokio::spawn(pump(client, out_rx, in_tx));
        let send_task = tokio::spawn(async move {
            let mut sink = ChanSink {
                tx: tokio_util::sync::PollSender::new(out_tx),
            };
            for _ in 0..MESSAGES {
                futures::SinkExt::feed(&mut sink, Message::binary(payload.clone()))
                    .await
                    .expect("feed");
            }
        });
        while received < MESSAGES {
            match in_rx.recv().await {
                Some(_) => received += 1,
                None => break,
            }
        }
        send_task.await.expect("send task");
    } else {
        let (mut sink, mut stream): (futures::stream::SplitSink<Ws, Message>, _) = client.split();
        if bidir {
            let send_task = tokio::spawn(async move {
                for _ in 0..MESSAGES {
                    sink.feed(Message::binary(payload.clone()))
                        .await
                        .expect("feed");
                }
                sink.flush().await.expect("flush");
            });
            while received < MESSAGES {
                if stream.next().await.is_some() {
                    received += 1;
                } else {
                    break;
                }
            }
            send_task.await.expect("send task");
        } else {
            for i in 0..MESSAGES {
                sink.feed(Message::binary(payload.clone()))
                    .await
                    .expect("feed");
                if i % 32 == 31 {
                    sink.flush().await.expect("flush");
                }
            }
            sink.flush().await.expect("flush");
        }
    }
    let elapsed = start.elapsed();
    let _ = done_rx.await;
    let total = (PAYLOAD * MESSAGES) as f64 / 1e6;
    let mbps = total / elapsed.as_secs_f64() * 8.0;
    let mode_name = if bidir { "bidir(echo)" } else { "unidir(send)" };
    println!("{mode_name}: {elapsed:.2?} for {total:.0} MB -> {mbps:.0} Mbps");
}
