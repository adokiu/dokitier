pub(crate) mod acl;
pub(crate) mod admission;
pub(crate) mod attached;
pub(crate) mod conn;
pub mod p2p_group;
pub mod context;
pub mod credential_manager;
pub mod error;
pub mod foreign_network;
pub mod peer_center;
pub mod peer_manager;
pub(crate) mod peer_rpc;
pub mod public_ipv6;
pub(crate) mod relay_peer_map;
pub mod bandwidth_probe;
pub mod link_policy;
pub mod transport_metrics;
pub(crate) mod route;
pub use route::RouteCostMode;
pub(crate) mod traffic_metrics;
mod util;
pub(crate) mod whitelist;

#[cfg(test)]
pub(crate) mod test_support;
#[cfg(test)]
mod tests;

use crate::packet::ZCPacket;
use std::sync::Arc;
use tokio::sync::mpsc::error::{SendError, TryRecvError, TrySendError};

use self::conn::peer_conn::PeerConnId;
use crate::config::PeerId;

/// The local entry point that created a peer connection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PeerConnectionOrigin {
    Manual,
    Direct,
    Listener,
    TcpHolePunch,
    UdpHolePunch,
    Attached,
}

impl PeerConnectionOrigin {
    /// Automatic P2P paths are subject to the P2P-group isolation policy.
    /// Explicit `[[peer]]` connectors arrive as Manual/Listener and bypass it.
    pub fn is_auto_p2p(&self) -> bool {
        matches!(
            self,
            PeerConnectionOrigin::Direct
                | PeerConnectionOrigin::TcpHolePunch
                | PeerConnectionOrigin::UdpHolePunch
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PeerPacketIngress {
    Local,
    Peer {
        peer_id: PeerId,
        conn_id: PeerConnId,
        origin: PeerConnectionOrigin,
    },
}

impl PeerPacketIngress {
    pub(crate) fn is_attached(self) -> bool {
        matches!(
            self,
            Self::Peer {
                origin: PeerConnectionOrigin::Attached,
                ..
            }
        )
    }

    pub(crate) fn peer_connection(self) -> Option<(PeerId, PeerConnId)> {
        match self {
            Self::Local => None,
            Self::Peer {
                peer_id, conn_id, ..
            } => Some((peer_id, conn_id)),
        }
    }
}

#[derive(Debug)]
pub(crate) struct PeerPacketEnvelope {
    packet: ZCPacket,
    ingress: PeerPacketIngress,
}

impl PeerPacketEnvelope {
    fn local(packet: ZCPacket) -> Self {
        Self {
            packet,
            ingress: PeerPacketIngress::Local,
        }
    }

    fn from_peer(packet: ZCPacket, ingress: PeerPacketIngress) -> Self {
        debug_assert!(matches!(ingress, PeerPacketIngress::Peer { .. }));
        Self { packet, ingress }
    }

    pub(crate) fn into_parts(self) -> (ZCPacket, PeerPacketIngress) {
        (self.packet, self.ingress)
    }
}

/// Fan-in to the packet router.
///
/// The router runs one worker per shard so a single slow packet (route
/// resolution for a relayed destination, RPC-triggering control packet, a
/// congested peer) cannot head-of-line block every other flow. Sharding is by
/// `(from_peer_id, to_peer_id)` so all packets of one peer pair stay ordered in
/// one worker — the protocols above (KCP/QUIC/TCP-over-tunnel) rely on that.
#[derive(Clone)]
pub struct PacketRecvChan(Arc<Vec<tokio::sync::mpsc::Sender<PeerPacketEnvelope>>>);

/// Per-queue depth. These queues sit between the connection read loops and the
/// router workers; they absorb bursts without ever parking a read loop.
const ROUTER_QUEUE_DEPTH: usize = 1024;

impl PacketRecvChan {
    fn shard_for(&self, packet: &ZCPacket) -> usize {
        if self.0.len() <= 1 {
            return 0;
        }
        let header = packet.peer_manager_header();
        let key = header
            .map(|h| (h.from_peer_id.get() as u64) << 32 | h.to_peer_id.get() as u64)
            .unwrap_or(0);
        // Fibonacci hashing: cheap, and adjacent peer ids do not collide.
        ((key.wrapping_mul(0x9E37_79B9_7F4A_7C15)) >> 32) as usize % self.0.len()
    }

    /// Injects a packet produced by a local subsystem. Local injection never carries
    /// the privileges of an attached peer connection.
    pub async fn send(&self, packet: ZCPacket) -> Result<(), SendError<ZCPacket>> {
        let shard = self.shard_for(&packet);
        self.0[shard]
            .send(PeerPacketEnvelope::local(packet))
            .await
            .map_err(|error| SendError(error.0.packet))
    }

    fn try_send_envelope(
        &self,
        envelope: PeerPacketEnvelope,
    ) -> Result<(), TrySendError<PeerPacketEnvelope>> {
        let shard = self.shard_for(&envelope.packet);
        self.0[shard].try_send(envelope)
    }
}

pub struct PacketRecvChanReceiver(tokio::sync::mpsc::Receiver<PeerPacketEnvelope>);

impl PacketRecvChanReceiver {
    pub async fn recv(&mut self) -> Option<ZCPacket> {
        self.0.recv().await.map(|envelope| envelope.packet)
    }
}

pub fn create_packet_recv_chan() -> (PacketRecvChan, PacketRecvChanReceiver) {
    let (sender, receivers) = create_sharded_packet_recv_chan(1);
    (sender, receivers.into_iter().next().expect("one shard"))
}

/// Router fan-in with `shards` independent queues. Each receiver is drained by
/// its own worker (see `PeerManager::start_peer_recv`).
pub fn create_sharded_packet_recv_chan(
    shards: usize,
) -> (PacketRecvChan, Vec<PacketRecvChanReceiver>) {
    let shards = shards.max(1);
    let mut senders = Vec::with_capacity(shards);
    let mut receivers = Vec::with_capacity(shards);
    for _ in 0..shards {
        let (sender, receiver) = tokio::sync::mpsc::channel(ROUTER_QUEUE_DEPTH);
        senders.push(sender);
        receivers.push(PacketRecvChanReceiver(receiver));
    }
    (PacketRecvChan(Arc::new(senders)), receivers)
}

/// Packets dropped because the router queue was full.
///
/// Exposed for the metrics frame and the 1-in-64 diagnostic log; the data path
/// only does one relaxed increment when it happens.
pub static ROUTER_QUEUE_FULL_DROPS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

/// Hand a packet to the router.
///
/// This is the upstream edge of the data plane, so it must never park: the
/// previous `send().await` fallback propagated backpressure from a busy router
/// into every connection read loop, which in turn stalled unrelated flows.
/// When the bounded queue is full the packet is dropped (data traffic is
/// droppable by definition) and the drop is counted so congestion is visible
/// instead of silently blocking.
pub(crate) fn send_peer_packet_to_chan(
    sender: &PacketRecvChan,
    packet: ZCPacket,
    ingress: PeerPacketIngress,
) -> Result<(), SendError<ZCPacket>> {
    let envelope = PeerPacketEnvelope::from_peer(packet, ingress);
    match sender.try_send_envelope(envelope) {
        Ok(()) => Ok(()),
        Err(TrySendError::Full(envelope)) => {
            ROUTER_QUEUE_FULL_DROPS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Err(SendError(envelope.packet))
        }
        Err(TrySendError::Closed(envelope)) => Err(SendError(envelope.packet)),
    }
}

pub(crate) async fn recv_packet_envelope_from_chan(
    packet_recv_chan_receiver: &mut PacketRecvChanReceiver,
) -> Result<PeerPacketEnvelope, anyhow::Error> {
    match packet_recv_chan_receiver.0.try_recv() {
        Ok(packet) => Ok(packet),
        Err(TryRecvError::Empty) => packet_recv_chan_receiver
            .0
            .recv()
            .await
            .ok_or(anyhow::anyhow!("recv_packet_from_chan failed")),
        Err(TryRecvError::Disconnected) => Err(anyhow::anyhow!("recv_packet_from_chan failed")),
    }
}

pub async fn recv_packet_from_chan(
    packet_recv_chan_receiver: &mut PacketRecvChanReceiver,
) -> Result<ZCPacket, anyhow::Error> {
    recv_packet_envelope_from_chan(packet_recv_chan_receiver)
        .await
        .map(|envelope| envelope.packet)
}

/// Returned packets continue at the next peer-packet filter.
#[derive(Debug)]
pub enum PeerPacketFilterResult {
    Consumed,
    Pass(ZCPacket),
    PassBatch(Vec<ZCPacket>),
}

impl From<Option<ZCPacket>> for PeerPacketFilterResult {
    fn from(packet: Option<ZCPacket>) -> Self {
        match packet {
            Some(packet) => Self::Pass(packet),
            None => Self::Consumed,
        }
    }
}

#[async_trait::async_trait]
#[auto_impl::auto_impl(Arc)]
pub trait PeerPacketFilter {
    async fn try_process_packet_from_peer(&self, zc_packet: ZCPacket) -> PeerPacketFilterResult {
        PeerPacketFilterResult::Pass(zc_packet)
    }
}

#[async_trait::async_trait]
#[auto_impl::auto_impl(Arc)]
pub trait NicPacketFilter {
    async fn try_process_packet_from_nic(&self, data: &mut ZCPacket) -> bool;

    fn id(&self) -> String {
        format!("{:p}", self)
    }
}

pub type BoxPeerPacketFilter = Box<dyn PeerPacketFilter + Send + Sync>;
pub type BoxNicPacketFilter = Box<dyn NicPacketFilter + Send + Sync>;
