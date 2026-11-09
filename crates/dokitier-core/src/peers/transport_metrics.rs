//! Per-connection transport-level measurements (TCP retransmits, QUIC loss/RTT)
//! published by transport samplers and consumed by peer statistics/routing.

use std::collections::HashMap;
use std::sync::{OnceLock, RwLock};
use std::time::Instant;

#[derive(Debug, Clone, Default)]
pub struct TransportSample {
    pub proto: String,
    pub loss_pct: Option<f32>,
    pub rtt_us: Option<u32>,
    pub capacity_mbps: Option<f64>,
    pub utilization: Option<f64>,
    pub updated: Option<Instant>,
}

static SAMPLES: OnceLock<RwLock<HashMap<String, TransportSample>>> = OnceLock::new();

fn samples() -> &'static RwLock<HashMap<String, TransportSample>> {
    SAMPLES.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Publish/merge a sample. `key` is `"<ip>:<port>"` of the remote endpoint.
pub fn publish(key: impl Into<String>, sample: TransportSample) {
    let mut map = samples().write().unwrap();
    let entry = map.entry(key.into()).or_default();
    if !sample.proto.is_empty() {
        entry.proto = sample.proto;
    }
    if sample.loss_pct.is_some() {
        entry.loss_pct = sample.loss_pct;
    }
    if sample.rtt_us.is_some() {
        entry.rtt_us = sample.rtt_us;
    }
    if sample.capacity_mbps.is_some() {
        entry.capacity_mbps = sample.capacity_mbps;
    }
    if sample.utilization.is_some() {
        entry.utilization = sample.utilization;
    }
    entry.updated = Some(Instant::now());
}

pub fn get(key: &str) -> Option<TransportSample> {
    samples().read().unwrap().get(key).cloned()
}

pub fn remove(key: &str) {
    samples().write().unwrap().remove(key);
}

/// Capacity/utilization sampler based on cumulative byte counters. Keeps a
/// decaying peak delivery rate and an EWMA of the current rate.
#[derive(Debug, Clone, Copy, Default)]
pub struct RateSample {
    pub capacity_mbps: f64,
    pub utilization: f64,
}

pub fn update_rate(
    key: &str,
    tx_bytes: u64,
    rx_bytes: u64,
    elapsed_secs: f64,
    declared_capacity_mbps: Option<f64>,
) -> Option<RateSample> {
    use std::collections::HashMap as Map;
    use std::sync::Mutex;
    static RATE_STATE: OnceLock<Mutex<Map<String, (u64, u64, f64, f64, f64)>>> = OnceLock::new();
    // (tx, rx, peak_mbps, ewma_mbps, last_ts_secs)
    let state = RATE_STATE.get_or_init(|| Mutex::new(Map::new()));
    let mut map = state.lock().ok()?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs_f64();
    let entry = map
        .entry(key.to_string())
        .or_insert((tx_bytes, rx_bytes, 0.0, 0.0, now));
    let dt = if entry.4 > 0.0 {
        (now - entry.4).clamp(0.2, 10.0)
    } else {
        elapsed_secs.max(0.001)
    };
    let tx_delta = tx_bytes.saturating_sub(entry.0);
    let rx_delta = rx_bytes.saturating_sub(entry.1);
    let bytes_per_sec = (tx_delta.max(rx_delta)) as f64 / dt;
    let mbps = bytes_per_sec * 8.0 / 1_000_000.0;
    entry.0 = tx_bytes;
    entry.1 = rx_bytes;
    entry.2 = (entry.2 * 0.995).max(mbps);
    entry.3 = if entry.3 == 0.0 { mbps } else { entry.3 * 0.8 + mbps * 0.2 };
    entry.4 = now;
    let peak = entry.2.max(0.0);
    // Declared/advertised capacity is an upper bound; passive peak is only a
    // fallback when the peer does not advertise. Using peak.min(declared)
    // wrongly collapsed idle links to ~1 Mbps.
    // Capacity is known only when an active burst measurement supplied it;
    // otherwise it stays 0 (unknown) and the UI shows "未测量".
    let capacity = match declared_capacity_mbps.filter(|v| *v > 0.0) {
        Some(declared) => declared.max(1.0).round(),
        None => 0.0,
    };
    let utilization = (entry.3 / capacity).clamp(0.0, 1.0);
    Some(RateSample {
        capacity_mbps: capacity,
        utilization,
    })
}

/// Most recent sample older than `max_age` is treated as stale.
pub fn get_fresh(key: &str, max_age: std::time::Duration) -> Option<TransportSample> {
    get(key).filter(|sample| {
        sample
            .updated
            .map(|updated| updated.elapsed() <= max_age)
            .unwrap_or(false)
    })
}

/// Parse `/proc/net/tcp` for the socket matching `remote` and return the
/// cumulative retransmit counter (`retrnsmt`). Linux only; returns None for
/// IPv6 or when the socket is not found.
pub fn linux_tcp_retrans(remote: std::net::SocketAddr) -> Option<u64> {
    let std::net::SocketAddr::V4(remote) = remote else {
        return None;
    };
    let text = std::fs::read_to_string("/proc/net/tcp").ok()?;
    let want_ip = u32::from(*remote.ip());
    let want_port = remote.port();
    for line in text.lines().skip(1) {
        let mut fields = line.split_whitespace();
        let _sl = fields.next()?;
        let local = fields.next()?;
        let rem = fields.next()?;
        let _st = fields.next()?;
        let _queues = fields.next()?;
        let _tr = fields.next()?;
        let retrnsmt = fields.next()?;
        let (Some((rem_ip, rem_port)), Some((_local_ip, _local_port))) =
            (parse_proc_addr(rem), parse_proc_addr(local))
        else {
            continue;
        };
        if rem_ip == want_ip && rem_port == want_port {
            return retrnsmt.parse::<u64>().ok();
        }
    }
    None
}

fn parse_proc_addr(value: &str) -> Option<(u32, u16)> {
    let (ip_hex, port_hex) = value.split_once(':')?;
    if ip_hex.len() != 8 {
        return None;
    }
    let raw = u32::from_str_radix(ip_hex, 16).ok()?;
    let ip = u32::from_le_bytes(raw.to_le_bytes());
    let port = u16::from_str_radix(port_hex, 16).ok()?;
    Some((ip, port))
}

/// Compute a TCP retransmission ratio from cumulative counters. The result is
/// an EWMA so a single burst does not flip routing.
pub fn tcp_loss_tick(key: &str, retrans_total: u64, tx_packets: u64) -> Option<f32> {
    use std::collections::HashMap as Map;
    use std::sync::Mutex;
    static TCP_STATE: OnceLock<Mutex<Map<String, (u64, u64, f32)>>> = OnceLock::new();
    let state = TCP_STATE.get_or_init(|| Mutex::new(Map::new()));
    let mut map = state.lock().ok()?;
    let entry = map
        .entry(key.to_string())
        .or_insert((retrans_total, tx_packets, 0.0));
    let d_retrans = retrans_total.saturating_sub(entry.0);
    let d_packets = tx_packets.saturating_sub(entry.1);
    entry.0 = retrans_total;
    entry.1 = tx_packets;
    if d_retrans == 0 && d_packets == 0 {
        return Some(entry.2);
    }
    let ratio = (d_retrans as f32 / (d_packets + d_retrans).max(1) as f32) * 100.0;
    entry.2 = entry.2 * 0.7 + ratio * 0.3;
    Some(entry.2.clamp(0.0, 100.0))
}
