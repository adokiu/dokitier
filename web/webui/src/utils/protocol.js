// Shared mapping for EasyTier link/tunnel protocol types.
//
// The core reports the transport as a raw scheme string (sometimes composed by
// manual-endpoint resolution, e.g. "https-ws", or aggregated across tunnels,
// e.g. "udp/tcp"). Views should never print those raw strings directly: use
// protocolLabel() for the human readable text and protocolClass() for the CSS
// color badge class (global `.proto-badge--<class>` rules in styles/main.scss).

const LABELS = {
  tcp: 'TCP',
  udp: 'UDP',
  quic: 'QUIC',
  ws: 'WS',
  wss: 'WSS',
  faketcp: 'FAKE-TCP',
  wg: 'WG',
  ring: 'RING',
  unix: 'UNIX',
  http: 'HTTP',
  https: 'HTTPS',
  other: '其他',
}

// Extra spellings emitted by older cores / proto enums / UI selections.
const ALIASES = {
  websocket: 'ws',
  'websocket-secure': 'wss',
  'websocket_secure': 'wss',
  'websocket-tls': 'wss',
  wireguard: 'wg',
  'fake-tcp': 'faketcp',
}

// Aggregates are joined by "/", "+" or ","; the console also uses the Chinese
// placeholder "汇总" when a link only carries an aggregate type list.
const AGGREGATE_SPLIT = /[\/+，,、]/

function normalizePart(part) {
  const raw = String(part ?? '').trim()
  if (!raw) return null
  // Already-human placeholder (e.g. 汇总) — keep it instead of mapping to 其他.
  if (/[^\x00-\x7f]/.test(raw)) return { key: 'other', label: raw, cls: 'other', raw }
  const key = raw.toLowerCase()
  if (LABELS[key]) return { key, label: LABELS[key], cls: key, raw }
  if (ALIASES[key]) {
    const canonical = ALIASES[key]
    return { key: canonical, label: LABELS[canonical], cls: canonical, raw }
  }
  // Composed manual-endpoint types such as "https-ws" / "ring-tcp-udp": every
  // segment is a known transport, color by the innermost (actual) transport.
  if (key.includes('-')) {
    const tokens = key.split('-').filter(Boolean)
    const resolved = tokens.map((token) => ALIASES[token] || (LABELS[token] ? token : ''))
    if (resolved.length > 1 && resolved.every(Boolean)) {
      return {
        key: resolved.join('-'),
        label: resolved.map((token) => LABELS[token]).join('-'),
        cls: resolved[resolved.length - 1],
        raw,
      }
    }
  }
  return { key: 'other', label: LABELS.other, cls: 'other', raw }
}

/** Split any tunnel_type value (string, aggregate or array) into parts. */
export function protocolParts(value) {
  if (value == null || value === '') return []
  if (Array.isArray(value)) {
    return value.flatMap((entry) => protocolParts(entry))
  }
  return String(value)
    .split(AGGREGATE_SPLIT)
    .map(normalizePart)
    .filter(Boolean)
}

/** Human readable label, e.g. "udp/tcp" -> "UDP / TCP", "websocket" -> "WS". */
export function protocolLabel(value) {
  const parts = protocolParts(value)
  if (!parts.length) return '-'
  return parts.map((part) => part.label).join(' / ')
}

/** CSS badge color class suffix, e.g. "mixed", "ws", "faketcp". */
export function protocolClass(value) {
  const parts = protocolParts(value)
  if (!parts.length) return 'other'
  const classes = [...new Set(parts.map((part) => part.cls))]
  return classes.length === 1 ? classes[0] : 'mixed'
}

/** Raw value kept for title/tooltip debugging, never rendered as the label. */
export function protocolRaw(value) {
  if (value == null) return ''
  return Array.isArray(value) ? value.filter(Boolean).join('/') : String(value)
}
