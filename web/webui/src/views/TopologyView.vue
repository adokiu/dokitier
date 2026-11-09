<template>
  <div class="topology-page">
    <div class="d-flex align-center mb-3 flex-wrap topology-head">
      <div class="page-title">{{ t('nav.topology') }}</div>
      <v-spacer />
      <v-btn
        v-if="viewMode === 'detail'"
        size="small"
        variant="text"
        :color="routeMode ? 'primary' : undefined"
        prepend-icon="mdi-map-marker-path"
        class="mr-2"
        @click="toggleRoute"
      >路由</v-btn>
      <v-select
        v-model="networkFilter"
        :items="networkOptions"
        item-title="title"
        item-value="value"
        density="compact"
        variant="outlined"
        hide-details
        class="network-select mr-2"
      />
      <v-btn-toggle v-model="viewMode" mandatory density="compact" variant="text" divided class="mode-toggle">
        <v-btn value="detail" size="small">普通拓扑</v-btn>
        <v-btn value="geo" size="small">地理弧线</v-btn>
        <v-btn value="circle" size="small">环形视图</v-btn>
        <v-btn value="traffic" size="small">流量矩阵</v-btn>
        <v-btn value="latency" size="small">延迟矩阵</v-btn>
        <v-btn value="loss" size="small">丢包矩阵</v-btn>
      </v-btn-toggle>
    </div>
    <v-card ref="cardEl" class="pa-2 topology-card">
      <div v-show="viewMode !== 'traffic' && viewMode !== 'latency' && viewMode !== 'loss' && viewMode !== 'geo'" ref="chartEl" class="topology-chart"></div>
      <GeoGlobe v-if="viewMode === 'geo'" :geo="geoModel" />
      <div v-if="viewMode === 'traffic' || viewMode === 'latency' || viewMode === 'loss'" class="matrix-view">
        <div class="matrix-inner">
          <div class="matrix-scale">
            <span v-for="step in matrixModel.scale" :key="step.label" class="matrix-scale__item">
              <i :style="{ background: step.color, opacity: step.opacity }"></i>{{ step.label }}
            </span>
          </div>
          <div class="matrix-cols" :style="{ marginLeft: (matrixModel.labelW + 8) + 'px', gridTemplateColumns: `repeat(${matrixModel.n}, ${matrixModel.cell}px)` }">
            <div v-for="r in matrixModel.rows" :key="'cn' + r.i" class="matrix-num" :style="{ width: matrixModel.cell + 'px' }">{{ r.i + 1 }}</div>
          </div>
          <div class="matrix-gridwrap">
            <div class="matrix-rows" :style="{ width: matrixModel.labelW + 'px' }">
              <div v-for="r in matrixModel.rows" :key="'rn' + r.i" class="matrix-row" :class="{ 'matrix-row--offline': r.offline }" :style="{ height: matrixModel.cell + 'px' }">
                <span class="matrix-name">{{ r.name }}</span>
                <span class="matrix-strip" :style="{ background: r.color }"></span>
              </div>
            </div>
            <div
              class="matrix-grid"
              :style="{ gridTemplateColumns: `repeat(${matrixModel.n}, ${matrixModel.cell}px)`, gridAutoRows: matrixModel.cell + 'px' }"
            >
              <div
                v-for="c in matrixModel.cells"
                :key="c.key"
                class="matrix-cell"
                :style="{ background: c.color, opacity: c.opacity }"
                @mouseenter="onMatrixCell(c, $event)"
                @mousemove="onMatrixCell(c, $event)"
                @mouseleave="hover.visible = false"
                @click="onMatrixClick(c)"
              />
            </div>
          </div>
        </div>
      </div>
      <div v-if="viewMode === 'detail' && marqueeRect" class="marquee-box" :style="marqueeRect"></div>
      <div v-if="viewMode === 'detail' && isolated.length" class="isolated-panel">
        <div class="isolated-title">未连接 ({{ isolated.length }})</div>
        <div class="isolated-body">
          <div v-for="n in isolated" :key="n.key" class="isolated-item">
            <span class="isolated-name">{{ n.hostname }}</span>
            <span class="isolated-reason">{{ n.reason }}</span>
          </div>
        </div>
      </div>
      <div v-if="viewMode === 'detail' && routePath.length" class="route-panel">
        <div class="route-title">
          路由路径 ({{ routePath.length - 1 }} 跳 / {{ routeLatency != null ? routeLatency.toFixed(2) + ' ms' : '-' }})
        </div>
        <div class="route-path">{{ routePath.map(nodeLabel).join(' → ') }}</div>
      </div>
    </v-card>

    <DetailDrawer
      v-model="drawer"
      :title="drawerTitle"
      :subtitle="drawerSubtitle"
      :width="480"
    >
      <template v-if="selected && selected.kind === 'node'">
        <div class="node-grid">
          <div><div class="stat-label">Peer ID</div><div class="node-value mono">{{ selected.peer_id }}</div></div>
          <div><div class="stat-label">IPv4</div><div class="node-value mono">{{ selected.ipv4 || '-' }}</div></div>
          <div><div class="stat-label">IPv6</div><div class="node-value mono">{{ selected.ipv6 || '-' }}</div></div>
          <div><div class="stat-label">版本</div><div class="node-value">{{ selected.version || '-' }}</div></div>
          <div><div class="stat-label">角色</div><div class="node-value">{{ selected.local ? '本机节点' : '远端节点' }}</div></div>
          <div><div class="stat-label">实例</div><div class="node-value mono">{{ selected.instance_id || '-' }}</div></div>
        </div>

        <div class="stat-label mt-4 mb-2">邻居链路 ({{ nodeNeighbors.length }})</div>
        <div v-for="nb in nodeNeighbors" :key="nb.id" class="neighbor-block">
          <div class="neighbor-head">
            <span class="neighbor-name">{{ nb.hostname }}</span>
            <span class="neighbor-ip mono">{{ nb.ipv4 || nb.id }}</span>
            <v-spacer />
            <span class="neighbor-meta">{{ nb.latency != null ? nb.latency.toFixed(2) + ' ms' : '-' }} / {{ bps(nb.bw) }}</span>
          </div>
          <div v-for="t in uniqueTunnels(nb.tunnels)" :key="t.id" class="tunnel-row tunnel-row--nested">
            <div class="tunnel-row__top">
              <span
                class="tunnel-row__type proto-badge"
                :class="'proto-badge--' + protocolClass(t.tunnel_type)"
                :title="protocolRaw(t.tunnel_type)"
              >{{ protocolLabel(t.tunnel_type) }}</span>
              <span class="tunnel-row__addr">{{ tunnelPath(t) }}</span>
            </div>
            <div class="tunnel-row__stats">
              <span>延迟 {{ t.latency_ms != null ? Number(t.latency_ms).toFixed(2) + ' ms' : '-' }}</span>
              <span>Ping 丢包 {{ t.loss_pct != null ? Number(t.loss_pct).toFixed(2) + ' %' : '-' }}</span>
              <span>传输丢包 {{ t.transport_loss_pct != null ? Number(t.transport_loss_pct).toFixed(2) + ' %' : '-' }}</span>
              <span>容量 {{ t.capacity_mbps ? Number(t.capacity_mbps).toFixed(0) + ' Mbps' : '未测量' }}</span>
              <span>利用率 {{ t.utilization != null ? (Number(t.utilization) * 100).toFixed(1) + ' %' : '-' }}</span>
              <span v-if="t.transport_rtt_us">TRTT {{ (t.transport_rtt_us / 1000).toFixed(2) }} ms</span>
              <span>带宽 {{ bps((t.rx_bps || 0) + (t.tx_bps || 0)) }}</span>
            </div>
          </div>
        </div>
      </template>

      <template v-else-if="selected">
        <v-row class="mb-2">
          <v-col cols="6"><div class="stat-label">延迟</div><div class="stat-value" style="font-size:20px">{{ selected.latency != null ? selected.latency.toFixed(2) + ' ms' : '-' }}</div></v-col>
          <v-col cols="6"><div class="stat-label">丢包</div><div class="stat-value" style="font-size:20px">{{ selected.loss != null ? selected.loss.toFixed(2) + ' %' : '-' }}</div></v-col>
          <v-col cols="6"><div class="stat-label">接收</div><div class="stat-value" style="font-size:20px">{{ bps(selected.rx) }}</div></v-col>
          <v-col cols="6"><div class="stat-label">发送</div><div class="stat-value" style="font-size:20px">{{ bps(selected.tx) }}</div></v-col>
        </v-row>

        <div class="d-flex align-center mt-2 mb-1">
          <div class="stat-label">链路质量</div>
          <v-spacer />
          <v-btn-toggle v-model="historyRange" mandatory density="compact" variant="text" divided class="range-toggle">
            <v-btn value="live" size="x-small">实时</v-btn>
            <v-btn value="1h" size="x-small">1h</v-btn>
            <v-btn value="24h" size="x-small">24h</v-btn>
            <v-btn value="7d" size="x-small">7d</v-btn>
            <v-btn value="30d" size="x-small">30d</v-btn>
          </v-btn-toggle>
        </div>
        <div class="hist-wrap">
          <div ref="histEl" class="hist-chart" style="width: 100%; height: 180px"></div>
          <div v-if="histLine.visible" class="hist-pointer" :style="{ left: `${histLine.x}px` }"></div>
        </div>

        <div class="stat-label mt-3 mb-1">物理链路 ({{ uniqueTunnels(selected.tunnels).length || 1 }})</div>
        <div v-for="t in uniqueTunnels(selected.tunnels)" :key="t.id" class="tunnel-row">
          <div class="tunnel-row__top">
            <span
              class="tunnel-row__type proto-badge"
              :class="'proto-badge--' + protocolClass(t.tunnel_type)"
              :title="protocolRaw(t.tunnel_type)"
            >{{ protocolLabel(t.tunnel_type) }}</span>
            <span class="tunnel-row__addr">{{ tunnelPath(t) }}</span>
          </div>
          <div class="tunnel-row__stats">
            <span>延迟 {{ t.latency_ms != null ? Number(t.latency_ms).toFixed(2) + ' ms' : '-' }}</span>
            <span>Ping 丢包 {{ t.loss_pct != null ? Number(t.loss_pct).toFixed(2) + ' %' : '-' }}</span>
            <span>传输丢包 {{ t.transport_loss_pct != null ? Number(t.transport_loss_pct).toFixed(2) + ' %' : '-' }}</span>
            <span>容量 {{ t.capacity_mbps ? Number(t.capacity_mbps).toFixed(0) + ' Mbps' : '未测量' }}</span>
            <span>利用率 {{ t.utilization != null ? (Number(t.utilization) * 100).toFixed(1) + ' %' : '-' }}</span>
            <span v-if="t.transport_rtt_us">TRTT {{ (t.transport_rtt_us / 1000).toFixed(2) }} ms</span>
            <span>带宽 {{ bps((t.rx_bps || 0) + (t.tx_bps || 0)) }}</span>
          </div>
        </div>

        <div class="stat-label mt-3 mb-1">链路策略</div>
        <v-text-field v-model.number="form.bandwidth_mbps" label="带宽 (Mbps)" type="number" density="compact" variant="outlined" hide-details class="mb-4" />
        <v-text-field v-model="form.latency_weight" label="延迟权重（空=全局）" type="number" density="compact" variant="outlined" hide-details class="mb-4" />
        <v-text-field v-model="form.loss_weight" label="丢包权重（空=全局）" type="number" density="compact" variant="outlined" hide-details class="mb-4" />
        <v-text-field v-model="form.bandwidth_weight" label="带宽权重（空=全局）" type="number" density="compact" variant="outlined" hide-details class="mb-4" />
        <v-text-field v-model="form.note" label="备注" density="compact" variant="outlined" hide-details class="mb-4" />
        <v-switch v-model="form.disabled" label="禁用此链路" color="error" density="compact" />
        <v-btn color="primary" block :loading="saving" @click="save">保存</v-btn>
      </template>
    </DetailDrawer>

    <HoverCard :visible="hover.visible" :x="hover.x" :y="hover.y">
      <template #title>{{ hover.title }}</template>
      <div v-for="row in hover.rows" :key="row.label" class="dok-hover-card__row">
        <span>{{ row.label }}</span><b>{{ row.value }}</b>
      </div>
    </HoverCard>
  </div>
</template>

<script setup>
import { useI18n } from 'vue-i18n'

import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import * as echarts from 'echarts'
import { api } from '@/api'
import HoverCard from '@/components/HoverCard.vue'
import { protocolClass, protocolLabel, protocolRaw } from '@/utils/protocol'
import DetailDrawer from '@/components/DetailDrawer.vue'
import GeoGlobe from '../components/GeoGlobe.vue'

const { t } = useI18n()

const chartEl = ref(null)
let chart = null
let ws = null
let graph = { nodes: [], edges: [] }
let allGraph = { nodes: [], edges: [] }
const geoModel = ref({ cities: [], links: [] })
let metrics = {}
let layout = {}
let layoutTarget = null
let layoutMeta = null
let edgeHitCache = []
let width = 1200
let height = 640
const networkFilter = ref(null)
const networkOptions = ref([{ title: '全部网络', value: '' }])
const routeMode = ref(false)
const routeStart = ref(null)
const routeEnd = ref(null)
const routePath = ref([])
const routeLatency = ref(null)
const isolated = ref([])
let boardNodes = []
let networkPicked = false
let drag = null
let pan = null
let resizeObserver = null
let histChart = null
const histEl = ref(null)
const cardEl = ref(null)
let eventSource = null
let sseRetry = null
let pollTimer = null
let flowTimer = null
let flowOffset = 0
let nodesTimer = null
let refreshTick = 0
let lastSampleTs = 0
// peer_id -> last seen node object; offline nodes keep their matrix/ring slot
const knownNodes = new Map()
const prevTraffic = new Map()
const edgeSamples = {}
const historyRange = ref('live')
const historyPoints = ref([])
const histLine = ref({ visible: false, x: 0 })
const view = { scale: 1, tx: 0, ty: 0 }

function toScreen(p) {
  return { x: p.x * view.scale + view.tx, y: p.y * view.scale + view.ty }
}

function toWorld(x, y) {
  return { x: (x - view.tx) / view.scale, y: (y - view.ty) / view.scale }
}

function zoomBy(factor, cx, cy) {
  const px = cx ?? width / 2
  const py = cy ?? height / 2
  const next = Math.min(3, Math.max(0.35, view.scale * factor))
  const ratio = next / view.scale
  view.tx = px - (px - view.tx) * ratio
  view.ty = py - (py - view.ty) * ratio
  view.scale = next
  apply()
}

/** Reset the view transform to the default (compiled build behaviour). */
function resetView() {
  view.scale = 1
  view.tx = 0
  view.ty = 0
  apply()
}
const drawer = ref(false)
const saving = ref(false)
const selected = ref(null)
const form = ref({ bandwidth_mbps: null, latency_weight: '', loss_weight: '', bandwidth_weight: '', note: '', disabled: false })
const nodeSelected = ref(null)
const hover = ref({ visible: false, x: 0, y: 0, title: '', rows: [] })
const hoverFocus = ref(null)
const multiSelected = ref([])
const viewMode = ref('detail')
const drillGroup = ref('')
const groupLayout = new Map()
let groupBoxDrag = null
const hoverCircle = ref(null)
const matrixModel = ref({ cells: [], rows: [], scale: [], cell: 20, labelW: 180, n: 0 })
const marqueeRect = ref(null)
let marquee = null
let groupDrag = null
let suppressClick = false

const drawerTitle = computed(() => {
  if (!selected.value) return ''
  return selected.value.kind === 'node'
    ? selected.value.alias || selected.value.hostname || String(selected.value.peer_id)
    : selected.value.nameA + ' ↔ ' + selected.value.nameB
})

const drawerSubtitle = computed(() => {
  if (!selected.value) return ''
  return selected.value.kind === 'node'
    ? '节点 ' + selected.value.peer_id
    : selected.value.a + ' ↔ ' + selected.value.b
})

const nodeNeighbors = computed(() => {
  const node = selected.value
  if (!node || node.kind !== 'node') return []
  const out = []
  for (const e of graph.edges || []) {
    if (e.a !== node.peer_id && e.b !== node.peer_id) continue
    const other = e.a === node.peer_id ? e.b : e.a
    const peer = (graph.nodes || []).find((n) => n.peer_id === other)
    const m = metrics[keyOf(e.a, e.b)] || {}
    const bw = (m.rx_bps || 0) + (m.tx_bps || 0)
    out.push({
      id: other,
      hostname: peer?.alias || peer?.hostname || String(other),
      ipv4: peer?.ipv4,
      latency: m.latency_ms,
      bw,
      tunnels: m.tunnels || [],
      count: (m.tunnels || []).length || 1,
    })
  }
  return out
})

function openNode(node) {
  selected.value = { ...node, kind: 'node' }
  drawer.value = true
}

function nodeName(id) {
  const n = (graph.nodes || []).find((x) => x.peer_id === Number(id))
  return n?.alias || n?.hostname || String(id)
}

function bpsOf(v) {
  if (v == null) return '-'
  const units = ['B/s', 'KB/s', 'MB/s', 'GB/s']
  let n = Number(v), i = 0
  while (n >= 1024 && i < units.length - 1) { n /= 1024; i++ }
  return `${n.toFixed(1)} ${units[i]}`
}

function nodeLabel(pid) {
  const n = (graph.nodes || []).find((x) => x.peer_id === Number(pid))
  return n ? n.alias || n.hostname || String(pid) : String(pid)
}

function applyNetworkFilter() {
  // Scope filtering happens on the server (?network=&group=); the client only
  // renders the payload it received.
  graph = { nodes: allGraph.nodes || [], edges: allGraph.edges || [] }
  if (hoverFocus.value != null && !graph.nodes.some((n) => n.peer_id === hoverFocus.value)) {
    hoverFocus.value = null
  }
}

function nodeBox(n) {
  return n && n.local ? SIZE.local : SIZE.remote
}

function edgePerp(p0, p1) {
  const dx = p1.x - p0.x
  const dy = p1.y - p0.y
  const d = Math.hypot(dx, dy) || 1
  return { x: -dy / d, y: dx / d }
}

function curveHitsCard(p0, c1, c2, p1, rects, idA, idB) {
  for (let i = 1; i < 18; i++) {
    const pt = bezierPoint(p0, c1, c2, p1, i / 18)
    for (const r of rects) {
      if (r.id === idA || r.id === idB) continue
      if (pt.x >= r.x && pt.x <= r.x + r.w && pt.y >= r.y && pt.y <= r.y + r.h) return true
    }
  }
  return false
}

function pointInRects(pt, rects, idA, idB) {
  for (const r of rects) {
    if (r.id === idA || r.id === idB) continue
    if (pt.x > r.x && pt.x < r.x + r.w && pt.y > r.y && pt.y < r.y + r.h) return true
  }
  return false
}

function cubicBlocked(p0, c1, c2, p1, rects, idA, idB) {
  for (let i = 1; i < 32; i++) {
    if (pointInRects(bezierPoint(p0, c1, c2, p1, i / 32), rects, idA, idB)) return true
  }
  return false
}

function quadPoints(p0, ctrl, p1, count = 18) {
  const out = []
  for (let i = 0; i <= count; i++) {
    const t = i / count
    const mt = 1 - t
    out.push({
      x: mt * mt * p0.x + 2 * mt * t * ctrl.x + t * t * p1.x,
      y: mt * mt * p0.y + 2 * mt * t * ctrl.y + t * t * p1.y,
    })
  }
  return out
}

// Route one link so it never crosses any card. First tries to bend the single
// cubic; if that is impossible it uses a smooth one-path detour around the
// blocking cards (still one continuous curve, no merging, same layer).
function routeLink(p0, c1, c2, p1, rects, idA, idB) {
  if (!cubicBlocked(p0, c1, c2, p1, rects, idA, idB)) {
    return {
      kind: 'cubic',
      d: `M ${p0.x.toFixed(2)} ${p0.y.toFixed(2)} C ${c1.x.toFixed(2)} ${c1.y.toFixed(2)} ${c2.x.toFixed(2)} ${c2.y.toFixed(2)} ${p1.x.toFixed(2)} ${p1.y.toFixed(2)}`,
      samples: [p0, ...Array.from({ length: 15 }, (_, i) => bezierPoint(p0, c1, c2, p1, (i + 1) / 16)), p1],
    }
  }
  const perp = edgePerp(p0, p1)
  for (const mag of [24, 40, 56, 74, 94, 118, 146, 176, 210, 250]) {
    for (const sa of [1, -1]) {
      for (const sb of [1, -1]) {
        const tc1 = { x: c1.x + perp.x * mag * sa, y: c1.y + perp.y * mag * sa }
        const tc2 = { x: c2.x + perp.x * mag * sb, y: c2.y + perp.y * mag * sb }
        if (!cubicBlocked(p0, tc1, tc2, p1, rects, idA, idB)) {
          return {
            kind: 'cubic',
            d: `M ${p0.x.toFixed(2)} ${p0.y.toFixed(2)} C ${tc1.x.toFixed(2)} ${tc1.y.toFixed(2)} ${tc2.x.toFixed(2)} ${tc2.y.toFixed(2)} ${p1.x.toFixed(2)} ${p1.y.toFixed(2)}`,
            samples: [p0, ...Array.from({ length: 15 }, (_, i) => bezierPoint(p0, tc1, tc2, p1, (i + 1) / 16)), p1],
          }
        }
      }
    }
  }
  const mid = { x: (p0.x + p1.x) / 2, y: (p0.y + p1.y) / 2 }
  for (const mag of [50, 80, 115, 150, 190, 235, 285, 340, 400, 470]) {
    for (const sign of [1, -1]) {
      const wp = { x: mid.x + perp.x * mag * sign, y: mid.y + perp.y * mag * sign }
      const ctrl = { x: 2 * wp.x - mid.x, y: 2 * wp.y - mid.y }
      const pts = quadPoints(p0, ctrl, p1, 26)
      if (pts.every((pt) => !pointInRects(pt, rects, idA, idB))) {
        return { kind: 'path', d: edgePathD(pts), samples: pts }
      }
    }
  }
  // route outside the whole card field (guarantees cards never cover a link)
  if (rects.length) {
    let minX = Infinity
    let maxX = -Infinity
    let minY = Infinity
    let maxY = -Infinity
    for (const r of rects) {
      minX = Math.min(minX, r.x)
      maxX = Math.max(maxX, r.x + r.w)
      minY = Math.min(minY, r.y)
      maxY = Math.max(maxY, r.y + r.h)
    }
    const pad = 40
    const outside = [
      { x: minX - pad, y: (p0.y + p1.y) / 2 },
      { x: maxX + pad, y: (p0.y + p1.y) / 2 },
      { x: (p0.x + p1.x) / 2, y: minY - pad },
      { x: (p0.x + p1.x) / 2, y: maxY + pad },
    ]
    for (const wp of outside) {
      const ctrl = { x: 2 * wp.x - mid.x, y: 2 * wp.y - mid.y }
      const pts = quadPoints(p0, ctrl, p1, 26)
      if (pts.every((pt) => !pointInRects(pt, rects, idA, idB))) {
        return { kind: 'path', d: edgePathD(pts), samples: pts }
      }
    }
  }
  // last resort: keep the plain cubic (extremely crowded view)
  return {
    kind: 'cubic',
    d: `M ${p0.x.toFixed(2)} ${p0.y.toFixed(2)} C ${c1.x.toFixed(2)} ${c1.y.toFixed(2)} ${c2.x.toFixed(2)} ${c2.y.toFixed(2)} ${p1.x.toFixed(2)} ${p1.y.toFixed(2)}`,
    samples: [p0, ...Array.from({ length: 15 }, (_, i) => bezierPoint(p0, c1, c2, p1, (i + 1) / 16)), p1],
  }
}

// ------------------------------------------------------------------ layout math
// Latency is used as physical distance: nodes with low RTT end up close, long
// hauls end up far apart. Missing links are estimated through the metric
// closure so the layout still makes sense for sparse graphs.
function latencyDistance(nodes, edges) {
  const ids = nodes.map((n) => n.peer_id)
  const index = new Map(ids.map((id, i) => [id, i]))
  const n = ids.length
  const adj = Array.from({ length: n }, () => [])
  for (const e of edges) {
    const i = index.get(e.a)
    const j = index.get(e.b)
    if (i == null || j == null || i === j) continue
    const m = metrics[keyOf(e.a, e.b)] || {}
    let lat = m.latency_ms
    if (lat == null) {
      const lats = (e.tunnels || []).map((t) => t.latency_ms).filter((v) => v != null)
      lat = lats.length ? Math.min(...lats) : null
    }
    const w = lat != null ? Math.max(1, Number(lat)) : 80
    adj[i].push({ to: j, w })
    adj[j].push({ to: i, w })
  }
  const D = Array.from({ length: n }, () => new Array(n).fill(Infinity))
  for (let src = 0; src < n; src++) {
    const dist = D[src]
    dist[src] = 0
    const used = new Array(n).fill(false)
    for (let step = 0; step < n; step++) {
      let u = -1
      for (let i = 0; i < n; i++) if (!used[i] && (u === -1 || dist[i] < dist[u])) u = i
      if (u === -1 || dist[u] === Infinity) break
      used[u] = true
      for (const { to, w } of adj[u]) {
        if (dist[u] + w < dist[to]) dist[to] = dist[u] + w
      }
    }
    for (let i = 0; i < n; i++) if (dist[i] === Infinity) dist[i] = 120
  }
  return { ids, index, D, n }
}

function spectralEmbedding(nodes, edges) {
  const { ids, D, n } = latencyDistance(nodes, edges)
  if (n <= 1) return ids.map((id) => ({ id, x: 0, y: 0 }))
  const W = Array.from({ length: n }, (_, i) => Array.from({ length: n }, (_, j) => (i === j ? 0 : 1 / (1 + Math.log1p(Math.max(1, D[i][j]))))))
  const deg = W.map((row) => row.reduce((a, b) => a + b, 0))
  const normalize = (v) => {
    const mean = v.reduce((a, b) => a + b, 0) / n
    const c = v.map((x) => x - mean)
    const norm = Math.hypot(...c) || 1
    return c.map((x) => x / norm)
  }
  const power = (deflate) => {
    let v = Array.from({ length: n }, (_, i) => Math.sin(i * 1.7 + 0.3) + (i % 2 ? -0.4 : 0.4))
    for (let iter = 0; iter < 150; iter++) {
      const centered = normalize(v)
      const next = centered.map((_, i) => {
        let s = centered[i] * 0.5
        for (let j = 0; j < n; j++) {
          if (i === j) continue
          s += centered[j] * (W[i][j] / (Math.sqrt(deg[i] * deg[j]) || 1)) * 0.5
        }
        return s
      })
      let res = normalize(next)
      for (const dvec of deflate) {
        const proj = res.reduce((acc, x, i) => acc + x * dvec[i], 0)
        res = res.map((x, i) => x - proj * dvec[i])
      }
      v = res
    }
    return normalize(v)
  }
  const v1 = power([])
  const v2 = power([v1])
  return ids.map((id, i) => ({ id, x: v1[i], y: v2[i] }))
}

// Stress majorization (gradient form of SMACOF): screen distances follow the
// latency matrix, so the picture itself is meaningful.
function stressLayout(nodes, edges, cx, cy, maxW, maxH) {
  const { ids, D, n } = latencyDistance(nodes, edges)
  if (!n) return new Map()
  if (n === 1) return new Map([[ids[0], { x: cx, y: cy }]])
  const embedding = spectralEmbedding(nodes, edges)
  const embById = new Map(embedding.map((e) => [e.id, e]))
  const pos = ids.map((id) => {
    const e = embById.get(id)
    return { x: (e ? e.x : 0) * 180, y: (e ? e.y : 0) * 180 }
  })
  const target = D.map((row) => row.map((v) => Math.max(1, Math.min(4000, v))))
  const weight = target.map((row) => row.map((v) => 1 / (v * v)))
  for (let iter = 0; iter < 140; iter++) {
    const disp = pos.map(() => ({ x: 0, y: 0 }))
    for (let i = 0; i < n; i++) {
      for (let j = i + 1; j < n; j++) {
        const dx = pos[i].x - pos[j].x
        const dy = pos[i].y - pos[j].y
        const dist = Math.hypot(dx, dy) || 0.01
        const f = weight[i][j] * (1 - target[i][j] / dist)
        disp[i].x += f * dx
        disp[i].y += f * dy
        disp[j].x -= f * dx
        disp[j].y -= f * dy
      }
    }
    const alpha = 0.09
    for (let i = 0; i < n; i++) {
      pos[i].x += alpha * disp[i].x
      pos[i].y += alpha * disp[i].y
    }
  }
  let minX = Infinity
  let maxX = -Infinity
  let minY = Infinity
  let maxY = -Infinity
  for (const p of pos) {
    minX = Math.min(minX, p.x)
    maxX = Math.max(maxX, p.x)
    minY = Math.min(minY, p.y)
    maxY = Math.max(maxY, p.y)
  }
  const spanX = maxX - minX || 1
  const spanY = maxY - minY || 1
  // avoid the "everything on one line" look: give the flatter axis a minimum
  // share of the area while keeping relative distances
  let stretchX = 1
  let stretchY = 1
  const ratio = spanX / spanY
  if (ratio > 2.3) stretchY = ratio / 2.3
  else if (ratio < 1 / 2.3) stretchX = 1 / (ratio * 2.3)
  const scale = Math.min(maxW / (spanX * stretchX), maxH / (spanY * stretchY))
  const out = new Map()
  pos.forEach((p, i) => {
    out.set(ids[i], {
      x: cx + (p.x - (minX + maxX) / 2) * scale * stretchX,
      y: cy + (p.y - (minY + maxY) / 2) * scale * stretchY,
    })
  })
  return out
}

// Spectral seriation (Fiedler vector): similar (low latency) nodes become
// neighbours, which makes matrices/rings readable.
function spectralOrder(nodes, edges) {
  const { ids, D, n } = latencyDistance(nodes, edges)
  if (n <= 1) return ids
  const W = Array.from({ length: n }, (_, i) => Array.from({ length: n }, (_, j) => (i === j ? 0 : 1 / (1 + Math.log1p(Math.max(1, D[i][j]))))))
  const deg = W.map((row) => row.reduce((a, b) => a + b, 0))
  let v = Array.from({ length: n }, (_, i) => (i % 2 ? -1 : 1))
  for (let iter = 0; iter < 120; iter++) {
    const mean = v.reduce((a, b) => a + b, 0) / n
    const centered = v.map((x) => x - mean)
    const next = centered.map((_, i) => {
      let s = centered[i] * 0.5
      for (let j = 0; j < n; j++) {
        if (i === j) continue
        s += centered[j] * (W[i][j] / (Math.sqrt(deg[i] * deg[j]) || 1)) * 0.5
      }
      return s
    })
    const norm = Math.hypot(...next) || 1
    v = next.map((x) => x / norm)
  }
  return ids.slice().sort((a, b) => v[ids.indexOf(a)] - v[ids.indexOf(b)])
}

function refreshNetworkOptions(nodes) {
  const names = [...new Set((nodes || []).map((n) => n.network_name).filter(Boolean))].sort()
  networkOptions.value = [{ title: '全部网络', value: '' }, ...names.map((name) => ({ title: name, value: name }))]
  if (networkPicked) return
  const counts = new Map()
  for (const n of nodes || []) {
    const name = n.network_name || ''
    counts.set(name, (counts.get(name) || 0) + 1)
  }
  let best = ''
  let bestCount = 0
  for (const [name, count] of counts) {
    if (count > bestCount) { best = name; bestCount = count }
  }
  networkPicked = true
  if (best) networkFilter.value = best
}

function toggleRoute() {
  routeMode.value = !routeMode.value
  routeStart.value = null
  routeEnd.value = null
  routePath.value = []
  routeLatency.value = null
  apply()
}

function pickRouteNode(n) {
  if (routeStart.value == null) {
    routeStart.value = n.peer_id
    routeEnd.value = null
    routePath.value = []
    routeLatency.value = null
  } else if (routeEnd.value == null) {
    routeEnd.value = n.peer_id
    computeRoute()
  } else {
    routeStart.value = n.peer_id
    routeEnd.value = null
    routePath.value = []
    routeLatency.value = null
  }
  apply()
}

function computeRoute() {
  const from = routeStart.value
  const to = routeEnd.value
  if (from == null || to == null) return
  const adj = new Map()
  for (const e of graph.edges || []) {
    if (!adj.has(e.a)) adj.set(e.a, [])
    if (!adj.has(e.b)) adj.set(e.b, [])
    adj.get(e.a).push(e.b)
    adj.get(e.b).push(e.a)
  }
  const prev = new Map([[from, null]])
  const queue = [from]
  while (queue.length) {
    const cur = queue.shift()
    if (cur === to) break
    for (const next of adj.get(cur) || []) {
      if (prev.has(next)) continue
      prev.set(next, cur)
      queue.push(next)
    }
  }
  if (!prev.has(to)) {
    routePath.value = []
    return
  }
  const path = []
  for (let p = to; p != null; p = prev.get(p)) path.push(p)
  path.reverse()
  routePath.value = path
  let total = 0
  let count = 0
  for (let i = 1; i < path.length; i++) {
    const m = metrics[keyOf(path[i - 1], path[i])]
    if (m && m.latency_ms != null) { total += Number(m.latency_ms); count++ }
  }
  routeLatency.value = count ? total : null
}

function refreshIsolated() {
  const present = new Set((graph.nodes || []).map((n) => n.peer_id))
  const degree = new Map()
  for (const e of graph.edges || []) {
    degree.set(e.a, (degree.get(e.a) || 0) + 1)
    degree.set(e.b, (degree.get(e.b) || 0) + 1)
  }
  const list = []
  for (const node of boardNodes) {
    if (node.status !== 'online') continue
    const insts = (node.snapshot && node.snapshot.instances) || []
    let mine = insts
    if (networkFilter.value) {
      mine = mine.filter((i) => i.network_name === networkFilter.value)
    }
    if (!mine.length) continue
    if (mine.some((i) => present.has(i.peer_id) && (degree.get(i.peer_id) || 0) > 0)) continue
    list.push({ key: node.node_id, node_id: node.node_id, hostname: node.hostname || node.node_id, reason: '无连接' })
  }
  isolated.value = list
}

async function loadBoardNodes() {
  try {
    boardNodes = (await api.boardNodes()).nodes || []
    refreshIsolated()
  } catch {}
}

const SIZE = { local: { w: 170, h: 56 }, remote: { w: 170, h: 56 } }

function keyOf(a, b) {
  return a <= b ? `${a}:${b}` : `${b}:${a}`
}

function endpoint(addr) {
  if (!addr) return '-'
  const text = String(addr)
  const match = text.match(/^[a-z0-9+.-]+:\/\/(.*)$/i)
  let value = match ? match[1] : text
  // collapse IPv4-mapped IPv6 ([::ffff:a.b.c.d]:port -> a.b.c.d:port): the same
  // physical connection must never be listed twice in two representations
  const mapped = value.match(/^\[::ffff:(\d+\.\d+\.\d+\.\d+)\]:(\d+)$/i)
  if (mapped) value = `${mapped[1]}:${mapped[2]}`
  const mappedBare = value.match(/^::ffff:(\d+\.\d+\.\d+\.\d+)$/i)
  if (mappedBare) value = mappedBare[1]
  return value
}

/**
 * One row per actual connection: collapse duplicate tunnel entries that only
 * differ by representation (shared-listener local port vs direct local port,
 * IPv4 vs IPv4-mapped IPv6). Identity = protocol + peer endpoint.
 */
function uniqueTunnels(tunnels) {
  const seen = new Set()
  const out = []
  for (const t of tunnels || []) {
    const key = `${protocolClass(t.tunnel_type)}|${endpoint(t.remote_addr || t.id)}`
    if (seen.has(key)) continue
    seen.add(key)
    out.push(t)
  }
  return out
}

function tunnelPath(t) {
  const local = t.local_addr ? endpoint(t.local_addr) : '-'
  const remote = endpoint(t.remote_addr || t.id)
  return `本机 ${local} → 对端 ${remote}`
}

function bps(v) {
  if (v == null) return '-'
  const units = ['B/s', 'KB/s', 'MB/s', 'GB/s']
  let n = Number(v), i = 0
  while (n >= 1024 && i < units.length - 1) { n /= 1024; i++ }
  return `${n.toFixed(1)} ${units[i]}`
}

function edgeMeta(e) {
  const m = metrics[keyOf(e.a, e.b)] || {}
  const latency = m.latency_ms
  const bw = (m.rx_bps || 0) + (m.tx_bps || 0)
  return { m, latency, bw }
}

/** Simple deterministic force layout in pixel space. */
const GROUP_ORDER = ['cn', 'ix-sh', 'ix-sz', 'overseas']

/** Cluster layout: one ring per p2p group, regions ordered by connectivity so
 *  bridge groups (e.g. the IX sites) end up between the groups they connect. */
let layoutSig = ''
let pathCache = new Map()
let pathVersion = 0

/** Cluster layout: one column-pair per p2p_group, ordered by external neighbours. */
function computeLayout(force = false) {
  const nodes = graph.nodes || []
  const edges = graph.edges || []
  const sig = [
    networkFilter.value || '',
    Math.round(width),
    Math.round(height),
    nodes.map((n) => `${n.peer_id}:${groupOf(n)}`).sort().join(','),
    edges.map((e) => keyOf(e.a, e.b)).sort().join(','),
  ].join('|')
  if (!force && sig === layoutSig && Object.keys(layout).length) return false
  layoutSig = sig
  if (!nodes.length) {
    layoutTarget = {}
    layout = {}
    return true
  }
  const byName = (a, b) => String(a.hostname || a.peer_id).localeCompare(String(b.hostname || b.peer_id))
  const groupMap = new Map()
  for (const n of nodes) {
    const g = groupOf(n)
    if (!groupMap.has(g)) groupMap.set(g, [])
    groupMap.get(g).push(n)
  }
  const clusters = [...groupMap.entries()].map(([g, list]) => ({ g, list: list.slice().sort(byName), relay: isRelayGroup(g) }))
  const relays = clusters.filter((c) => c.relay).sort((a, b) => a.g.localeCompare(b.g))
  const others = clusters.filter((c) => !c.relay).sort((a, b) => b.list.length - a.list.length || a.g.localeCompare(b.g))
  // Preferred flat arrangement: cn on the left, IX groups in the middle
  // column, overseas on the right (other groups fill the middle).
  const rank = (g) => (g === 'cn' ? 0 : g === 'overseas' ? 2 : g.startsWith('ix') ? 1 : 1)
  // ---- group placement: the two largest groups anchor the left/right edges,
  // every bridging group stays in the middle, vertically centred and stacked.
  // Column positions are derived from the estimated width of each group's
  // degree-bucket grid, so a wide group can never push the middle column away.
  const ROW_GAP = Math.round(Math.max(130, Math.min(180, height * 0.24)))
  const COL_W = 300
  const estimateExtent = (c) => {
    const inGroup = new Set(c.list.map((n) => n.peer_id))
    const deg = new Map(c.list.map((n) => [n.peer_id, 0]))
    for (const e of edges) {
      if (!inGroup.has(e.a) || !inGroup.has(e.b)) continue
      deg.set(e.a, (deg.get(e.a) || 0) + 1)
      deg.set(e.b, (deg.get(e.b) || 0) + 1)
    }
    const buckets = [0, 0, 0, 0]
    for (const n of c.list) {
      const d = deg.get(n.peer_id) || 0
      buckets[d <= 1 ? 0 : d <= 3 ? 1 : d <= 5 ? 2 : 3]++
    }
    let colsN = 0
    let rows = 1
    for (const len of buckets) {
      if (!len) continue
      const sub = Math.ceil(len / 5)
      colsN += sub
      rows = Math.max(rows, Math.ceil(len / sub))
    }
    if (!colsN) colsN = 1
    return { w: Math.max(COL_W, (colsN - 1) * COL_W + 200), h: Math.max(ROW_GAP, rows * ROW_GAP) }
  }
  const extents = new Map(clusters.map((c) => [c, estimateExtent(c)]))
  let layoutCols
  if (relays.length) {
    layoutCols = [others.filter((_, i) => i % 2 === 0), relays, others.filter((_, i) => i % 2 === 1)].filter((c) => c.length)
  } else if (others.length >= 3) {
    const [left, right, ...mid] = others
    layoutCols = [[left], mid, [right]].filter((c) => c.length)
  } else if (others.length === 2) {
    layoutCols = [[others[0]], [others[1]]]
  } else {
    layoutCols = others.length ? [[others[0]]] : []
  }
  const margin = 70
  const gapX = 150
  const colWidth = (col) => Math.max(...col.map((c) => extents.get(c).w), 220)
  // lay the columns out sequentially in world space and centre the whole block;
  // if the groups need more width than the viewport they simply extend beyond it
  // (the canvas is pannable/zoomable) instead of being squashed together.
  const nCols = layoutCols.length
  const widths = layoutCols.map(colWidth)
  const contentW = widths.reduce((a, w) => a + w, 0) + gapX * Math.max(0, nCols - 1)
  const startX = (width - contentW) / 2
  const colCx = new Array(nCols).fill(width / 2)
  let cursor = startX
  for (let i = 0; i < nCols; i++) {
    colCx[i] = cursor + widths[i] / 2
    cursor += widths[i] + gapX
  }
  const cy = height / 2
  layoutCols.forEach((col, ci) => {
    const sizes = col.map((c) => extents.get(c))
    const vgap = 90
    const total = sizes.reduce((a, sz) => a + sz.h, 0) + vgap * (col.length - 1)
    let y = cy - total / 2
    col.forEach((c, i) => {
      c.cx = colCx[ci]
      c.cy = y + sizes[i].h / 2
      y += sizes[i].h + vgap
    })
  })

  const pos = {}
  const clusterOf = new Map()
  for (const c of clusters) for (const n of c.list) clusterOf.set(n.peer_id, c)

  // ---- tree layout per group: the gateway node (most cross-group links) is
  // placed on the region edge facing the groups it connects to, and the
  // internal spanning tree fans out away from it so links stay tidy.
  const placeTree = (c) => {
    const list = c.list
    const count = list.length
    if (!count) return
    if (count === 1) {
      pos[list[0].peer_id] = { x: c.cx, y: c.cy }
      return
    }
    const inGroup = new Set(list.map((n) => n.peer_id))
    const adj = new Map(list.map((n) => [n.peer_id, []]))
    for (const e of edges) {
      if (!inGroup.has(e.a) || !inGroup.has(e.b)) continue
      adj.get(e.a).push(e.b)
      adj.get(e.b).push(e.a)
    }
    const ext = new Map()
    let sx = 0
    let sy = 0
    let sw = 0
    for (const node of list) {
      let vx = 0
      let vy = 0
      let w = 0
      for (const e of edges) {
        let other = null
        if (e.a === node.peer_id) other = e.b
        else if (e.b === node.peer_id) other = e.a
        else continue
        const oc = clusterOf.get(other)
        if (!oc || oc === c) continue
        vx += oc.cx - c.cx
        vy += oc.cy - c.cy
        w++
      }
      ext.set(node.peer_id, { vx, vy, w })
      sx += vx
      sy += vy
      sw += w
    }
    const root = list.slice().sort((a, b) =>
      (ext.get(b.peer_id).w - ext.get(a.peer_id).w) ||
      (adj.get(b.peer_id).length - adj.get(a.peer_id).length) ||
      byName(a, b)
    )[0]
    let ux = 0
    let uy = -1
    const rootExt = ext.get(root.peer_id)
    if (rootExt && rootExt.w > 0) {
      const len = Math.hypot(rootExt.vx, rootExt.vy) || 1
      ux = rootExt.vx / len
      uy = rootExt.vy / len
    } else if (sw > 0) {
      const len = Math.hypot(sx, sy) || 1
      ux = sx / len
      uy = sy / len
    }
    const parent = new Map([[root.peer_id, null]])
    const children = new Map(list.map((n) => [n.peer_id, []]))
    const queue = [root.peer_id]
    const byId = new Map(list.map((n) => [n.peer_id, n]))
    const extWeight = (id) => ext.get(id)?.w || 0
    while (queue.length) {
      const cur = queue.shift()
      const ordered = adj.get(cur).slice().sort((a, b) =>
        (extWeight(b) - extWeight(a)) ||
        (adj.get(b).length - adj.get(a).length) ||
        byName(byId.get(a), byId.get(b))
      )
      for (const nb of ordered) {
        if (parent.has(nb)) continue
        parent.set(nb, cur)
        children.get(cur).push(nb)
        queue.push(nb)
      }
    }
    for (const node of list) {
      if (parent.has(node.peer_id)) continue
      parent.set(node.peer_id, root.peer_id)
      children.get(root.peer_id).push(node.peer_id)
    }
    for (const kids of children.values()) {
      kids.sort((a, b) => (extWeight(b) - extWeight(a)) || (adj.get(b).length - adj.get(a).length) || byName(byId.get(a), byId.get(b)))
    }
    const vx = -uy
    const vy = ux
    const stepDepth = 172
    const lateralGap = 232
    const edgeOffset = 196
    const placed = new Map()
    const placeSub = (id, depth, lateral) => {
      const depthOffset = edgeOffset - depth * stepDepth
      placed.set(id, {
        x: c.cx + ux * depthOffset + vx * lateral,
        y: c.cy + uy * depthOffset + vy * lateral,
      })
      const kids = children.get(id)
      if (!kids.length) return
      const rows = kids.length > 4 ? 2 : 1
      const perRow = Math.ceil(kids.length / rows)
      kids.forEach((kid, k) => {
        const row = Math.floor(k / perRow)
        const col = k % perRow
        const rowKids = Math.min(perRow, kids.length - row * perRow)
        const total = (rowKids - 1) * lateralGap
        placeSub(kid, depth + 1 + row, lateral + col * lateralGap - total / 2 + (row ? lateralGap * 0.22 : 0))
      })
    }
    placeSub(root.peer_id, 0, 0)
    for (const [id, p] of placed) pos[id] = p
  }
  // Perimeter ring layout: nodes are spread around the outer edge of the group
  // area instead of a dense tree, which keeps links long and readable (like the
  // manual arrangement) and avoids every card piling up in the centre.
  const placeRing = (c) => {
    const list = c.list
    const count = list.length
    if (!count) return
    if (count === 1) {
      pos[list[0].peer_id] = { x: c.cx, y: c.cy }
      return
    }
    const inGroup = new Set(list.map((n) => n.peer_id))
    const byId = new Map(list.map((n) => [n.peer_id, n]))
    const adj = new Map(list.map((n) => [n.peer_id, []]))
    for (const e of edges) {
      if (!inGroup.has(e.a) || !inGroup.has(e.b)) continue
      adj.get(e.a).push(e.b)
      adj.get(e.b).push(e.a)
    }
    // BFS order so neighbouring nodes stay next to each other on the ring
    const start = list.slice().sort((a, b) => (adj.get(b.peer_id).length - adj.get(a.peer_id).length) || byName(a, b))[0]
    const order = [start.peer_id]
    const seen = new Set(order)
    for (let i = 0; i < order.length; i++) {
      const nbs = adj.get(order[i]).slice().sort((x, y) => byName(byId.get(x), byId.get(y)))
      for (const nb of nbs) {
        if (seen.has(nb)) continue
        seen.add(nb)
        order.push(nb)
      }
    }
    for (const n of list) if (!seen.has(n.peer_id)) order.push(n.peer_id)
    let C = Math.max(3, Math.ceil(Math.sqrt(count * 1.6)))
    let R = Math.max(3, Math.ceil(count / C))
    const perimeter = (cols, rows) => 2 * cols + 2 * (rows - 2)
    while (perimeter(C, R) < count) {
      if (C <= R) C += 1
      else R += 1
    }
    const maxW = Math.max(420, width * 0.78)
    const maxH = Math.max(300, height * 0.62)
    const stepX = Math.min(330, maxW / Math.max(1, C - 1))
    const stepY = Math.min(210, maxH / Math.max(1, R - 1))
    const gridW = stepX * (C - 1)
    const gridH = stepY * (R - 1)
    const x0 = c.cx - gridW / 2
    const y0 = c.cy - gridH / 2
    const cells = []
    for (let j = 0; j < C; j++) cells.push({ x: x0 + j * stepX, y: y0 })
    for (let i = 1; i < R; i++) cells.push({ x: x0 + (C - 1) * stepX, y: y0 + i * stepY })
    for (let j = C - 2; j >= 0; j--) cells.push({ x: x0 + j * stepX, y: y0 + (R - 1) * stepY })
    for (let i = R - 2; i >= 1; i--) cells.push({ x: x0, y: y0 + i * stepY })
    const step = cells.length / count
    for (let k = 0; k < count; k++) {
      const cell = cells[Math.round(k * step) % cells.length]
      pos[order[k]] = { x: cell.x, y: cell.y }
    }
  }
  // Columns by peer degree: leaves on the left, hubs on the right. Rows inside
  // a column follow the spectral (latency similarity) order. Deterministic and
  // immune to the "everything in one line" problem of free stress layouts.
  const placeDegreeColumns = (c) => {
    const list = c.list
    if (!list.length) return
    const inGroup = new Set(list.map((n) => n.peer_id))
    const deg = new Map(list.map((n) => [n.peer_id, 0]))
    for (const e of edges) {
      if (!inGroup.has(e.a) || !inGroup.has(e.b)) continue
      deg.set(e.a, (deg.get(e.a) || 0) + 1)
      deg.set(e.b, (deg.get(e.b) || 0) + 1)
    }
    const bucketOf = (d) => (d <= 1 ? 0 : d <= 3 ? 1 : d <= 5 ? 2 : 3)
    const cols = [[], [], [], []]
    for (const n of list) cols[bucketOf(deg.get(n.peer_id) || 0)].push(n)
    const rank = new Map(spectralOrder(list, edges).map((id, i) => [id, i]))
    for (const col of cols) col.sort((a, b) => (rank.get(a.peer_id) ?? 0) - (rank.get(b.peer_id) ?? 0))
    // A bucket with many equal-degree peers must not become one long column,
    // otherwise all its links run along the same line and hide each other.
    const maxPerColumn = 5
    const finalCols = []
    for (const col of cols) {
      if (!col.length) continue
      if (col.length <= maxPerColumn) {
        finalCols.push(col)
        continue
      }
      const parts = Math.ceil(col.length / maxPerColumn)
      const per = Math.ceil(col.length / parts)
      for (let k = 0; k < parts; k++) {
        const chunk = col.slice(k * per, (k + 1) * per)
        if (chunk.length) finalCols.push(chunk)
      }
    }
    if (finalCols.length === 1 && finalCols[0].length > 3) {
      const col = finalCols.pop()
      const half = Math.ceil(col.length / 2)
      finalCols.push(col.slice(0, half), col.slice(half))
    }
    const colGap = 300
    const rowGap = 132
    const totalW = (finalCols.length - 1) * colGap
    let x = c.cx - totalW / 2
    finalCols.forEach((col, idx) => {
      const totalH = (col.length - 1) * rowGap
      // half-row stagger between neighbouring columns keeps links from
      // collapsing onto one straight line
      const stagger = idx % 2 ? rowGap * 0.45 : 0
      let y = c.cy - totalH / 2 + stagger
      for (const n of col) {
        pos[n.peer_id] = { x, y }
        y += rowGap
      }
      x += colGap
    })
  }
  clusters.forEach(placeDegreeColumns)

  // crossing reduction: barycenter ordering of rows inside every column
  // (3 left/right sweeps), then rows are re-spread evenly.
  for (let sweep = 0; sweep < 3; sweep++) {
    for (const c of clusters) {
      if (c.list.length < 2) continue
      const columns = new Map()
      for (const n of c.list) {
        const p0 = pos[n.peer_id]
        if (!p0) continue
        const key = Math.round(p0.x)
        if (!columns.has(key)) columns.set(key, [])
        columns.get(key).push(n)
      }
      const order = [...columns.keys()].sort((a, b) => a - b)
      for (let ci = 0; ci < order.length; ci++) {
        const key = order[ci]
        const col = columns.get(key)
        if (col.length < 2) continue
        const bary = new Map()
        for (const n of col) {
          let sum = 0
          let cnt = 0
          for (const e of edges) {
            const other = e.a === n.peer_id ? e.b : e.b === n.peer_id ? e.a : null
            if (other == null) continue
            const op = pos[other]
            if (!op) continue
            if (Math.round(op.x) === key) continue
            sum += op.y
            cnt++
          }
          bary.set(n.peer_id, cnt ? sum / cnt : pos[n.peer_id].y)
        }
        col.sort((a, b) => (bary.get(a.peer_id) ?? 0) - (bary.get(b.peer_id) ?? 0))
        const rowGap = 132
        const totalH = (col.length - 1) * rowGap
        let y = c.cy - totalH / 2
        for (const n of col) {
          pos[n.peer_id].y = y
          y += rowGap
        }
      }
    }
  }

  // keep cards inside a group from overlapping after the tree placement
  for (let iter = 0; iter < 160; iter++) {
    let moved = false
    for (let i = 0; i < nodes.length; i++) {
      for (let j = i + 1; j < nodes.length; j++) {
        const ci = clusterOf.get(nodes[i].peer_id)
        if (!ci || ci !== clusterOf.get(nodes[j].peer_id)) continue
        const A = pos[nodes[i].peer_id]
        const B = pos[nodes[j].peer_id]
        if (!A || !B) continue
        const ox = 216 - Math.abs(B.x - A.x)
        const oy = 92 - Math.abs(B.y - A.y)
        if (ox <= 0 || oy <= 0) continue
        moved = true
        if (ox < oy) {
          const push = (ox / 2 + 1) * (B.x >= A.x ? 1 : -1)
          A.x -= push
          B.x += push
        } else {
          const push = (oy / 2 + 1) * (B.y >= A.y ? 1 : -1)
          A.y -= push
          B.y += push
        }
      }
    }
    if (!moved) break
  }

  const CARD_W = 170
  const CARD_H = 56
  const PAD_X = 66
  const PAD_Y = 92
  const boundsOf = (c) => {
    let minX = Infinity
    let maxX = -Infinity
    let minY = Infinity
    let maxY = -Infinity
    for (const n of c.list) {
      const p = pos[n.peer_id]
      if (!p) continue
      minX = Math.min(minX, p.x)
      maxX = Math.max(maxX, p.x)
      minY = Math.min(minY, p.y)
      maxY = Math.max(maxY, p.y)
    }
    return { minX: minX - CARD_W / 2 - PAD_X, maxX: maxX + CARD_W / 2 + PAD_X, minY: minY - CARD_H / 2 - PAD_Y, maxY: maxY + CARD_H / 2 + PAD_Y }
  }
  const shiftCluster = (c, dx, dy) => {
    for (const n of c.list) {
      const p = pos[n.peer_id]
      if (!p) continue
      p.x += dx
      p.y += dy
    }
  }
  for (let iter = 0; iter < 200; iter++) {
    let moved = false
    for (let i = 0; i < clusters.length; i++) {
      for (let j = i + 1; j < clusters.length; j++) {
        const A = boundsOf(clusters[i])
        const B = boundsOf(clusters[j])
        const ox = Math.min(A.maxX, B.maxX) - Math.max(A.minX, B.minX)
        const oy = Math.min(A.maxY, B.maxY) - Math.max(A.minY, B.minY)
        if (ox <= 0 || oy <= 0) continue
        moved = true
        if (ox < oy) {
          const d = ox / 2 + 8
          const sign = A.minX <= B.minX ? -1 : 1
          shiftCluster(clusters[i], d * sign, 0)
          shiftCluster(clusters[j], -d * sign, 0)
        } else {
          const d = oy / 2 + 8
          const sign = A.minY <= B.minY ? -1 : 1
          shiftCluster(clusters[i], 0, d * sign)
          shiftCluster(clusters[j], 0, -d * sign)
        }
      }
    }
    if (!moved) break
  }

  pathVersion += 1
  pathCache = new Map()
  layoutTarget = pos
  const next = {}
  for (const n of nodes) {
    const old = layout[n.peer_id]
    const target = pos[n.peer_id]
    if (old) {
      next[n.peer_id] = { x: old.x, y: old.y }
      continue
    }
    let sx = 0
    let sy = 0
    let count = 0
    for (const e of edges) {
      const other = e.a === n.peer_id ? e.b : e.b === n.peer_id ? e.a : null
      if (other == null) continue
      const p = layout[other]
      if (!p) continue
      sx += p.x
      sy += p.y
      count++
    }
    next[n.peer_id] = count ? { x: sx / count, y: sy / count } : { x: target.x, y: target.y }
  }
  layout = next
  return true
}

/** Ease rendered positions toward the freshly computed layout. */
function tickLayoutAnim() {
  if (!layoutTarget) return false
  let moving = false
  for (const id in layoutTarget) {
    const a = layout[id]
    const t = layoutTarget[id]
    if (!a) { layout[id] = { x: t.x, y: t.y }; moving = true; continue }
    const dx = t.x - a.x
    const dy = t.y - a.y
    if (Math.abs(dx) > 0.2 || Math.abs(dy) > 0.2) {
      a.x += dx * 0.16
      a.y += dy * 0.16
      moving = true
    } else {
      a.x = t.x
      a.y = t.y
    }
  }
  return moving
}

/** Midpoint of the rectangle side facing the other node. *//** Midpoint of the rectangle side facing the other node. *//** Midpoint of the rectangle side facing the other node. */
function anchor(from, to) {
  const dx = to.x - from.x
  const dy = to.y - from.y
  if (Math.abs(dx) >= Math.abs(dy)) {
    return { x: from.x + (dx >= 0 ? 88 : -88), y: from.y, side: 'h', sign: dx >= 0 ? 1 : -1 }
  }
  return { x: from.x, y: from.y + (dy >= 0 ? 31 : -31), side: 'v', sign: dy >= 0 ? 1 : -1 }
}

// One continuous SVG bezier path through all waypoints (Catmull-Rom control
// points): a link is a single curve object, never a chain of separate elements.
// Catmull-Rom sample points for a path through `points` (used for hit testing
// and for checking that the curve never touches a card).
function catmullSamples(points, perSegment = 12) {
  const out = []
  for (let i = 0; i < points.length - 1; i++) {
    const p0 = points[Math.max(0, i - 1)]
    const p1 = points[i]
    const p2 = points[i + 1]
    const p3 = points[Math.min(points.length - 1, i + 2)]
    const c1 = { x: p1.x + (p2.x - p0.x) / 6, y: p1.y + (p2.y - p0.y) / 6 }
    const c2 = { x: p2.x - (p3.x - p1.x) / 6, y: p2.y - (p3.y - p1.y) / 6 }
    for (let k = 0; k < perSegment; k++) {
      const t = k / perSegment
      const mt = 1 - t
      out.push({
        x: mt * mt * mt * p1.x + 3 * mt * mt * t * c1.x + 3 * mt * t * t * c2.x + t * t * t * p2.x,
        y: mt * mt * mt * p1.y + 3 * mt * mt * t * c1.y + 3 * mt * t * t * c2.y + t * t * t * p2.y,
      })
    }
  }
  out.push(points[points.length - 1])
  return out
}

function pathTouchesCards(points, rects, idA, idB) {
  const samples = catmullSamples(points, 14)
  for (const pt of samples) {
    for (const r of rects) {
      if (r.id === idA || r.id === idB) continue
      if (pt.x > r.x && pt.x < r.x + r.w && pt.y > r.y && pt.y < r.y + r.h) return true
    }
  }
  return false
}

// Route one intra-group link: endpoints follow the port-normal rule, the middle
// may use up to two extra waypoints (≤5 curve points) so the line bends around
// cards instead of touching them.
function routeIntraLink(p0, n0, p1, n1, off, rects, idA, idB) {
  const h0 = { x: p0.x + n0.x * off, y: p0.y + n0.y * off }
  const h1 = { x: p1.x + n1.x * off, y: p1.y + n1.y * off }
  const simple = [p0, h0, h1, p1]
  if (!pathTouchesCards(simple, rects, idA, idB)) return simple
  const mid = { x: (p0.x + p1.x) / 2, y: (p0.y + p1.y) / 2 }
  const dx = p1.x - p0.x
  const dy = p1.y - p0.y
  const len = Math.hypot(dx, dy) || 1
  const perp = { x: -dy / len, y: dx / len }
  // one extra waypoint
  for (const mag of [70, 110, 160, 220, 300, 400, 520]) {
    for (const side of [1, -1]) {
      const wp = { x: mid.x + perp.x * mag * side, y: mid.y + perp.y * mag * side }
      const pts = [p0, h0, wp, h1, p1]
      if (!pathTouchesCards(pts, rects, idA, idB)) return pts
    }
  }
  // two extra waypoints around the blocking field
  for (const mag of [110, 170, 240, 330, 440, 560]) {
    for (const side of [1, -1]) {
      const wp1 = { x: p0.x + dx * 0.33 + perp.x * mag * side, y: p0.y + dy * 0.33 + perp.y * mag * side }
      const wp2 = { x: p0.x + dx * 0.67 + perp.x * mag * side, y: p0.y + dy * 0.67 + perp.y * mag * side }
      const pts = [p0, h0, wp1, wp2, h1, p1]
      if (!pathTouchesCards(pts, rects, idA, idB)) return pts
    }
  }
  return simple
}

function cubicHitCount(p0, c1, c2, p1, rects, idA, idB) {
  let hits = 0
  for (let i = 1; i < 26; i++) {
    const pt = bezierPoint(p0, c1, c2, p1, i / 26)
    for (const r of rects) {
      if (r.id === idA || r.id === idB) continue
      if (pt.x > r.x && pt.x < r.x + r.w && pt.y > r.y && pt.y < r.y + r.h) {
        hits++
        break
      }
    }
  }
  return hits
}

// Exactly two control points, both on the port normals. If the direct cubic
// would touch a card, the port position along the card edge and the normal
// extensions are adjusted until the curve clears every card.
function pickCubic(a, b, na, nb, rects, idA, idB) {
  const baseAxis = Math.abs(b.x - a.x) >= Math.abs(b.y - a.y) ? 'h' : 'v'
  const seg = Math.hypot(b.x - a.x, b.y - a.y)
  const off = Math.min(160, seg * 0.36)
  // each end may use its own axis, i.e. the link is allowed to switch card
  // sides when that makes a cleaner curve
  const make = (axis0, axis1, s0, s1, fa, fb) => {
    const p0 = anchorAxis(a, b, axis0)
    const p1 = anchorAxis(b, a, axis1)
    const t0 = axis0 === 'h' ? { x: 0, y: 1 } : { x: 1, y: 0 }
    const t1 = axis1 === 'h' ? { x: 0, y: 1 } : { x: 1, y: 0 }
    p0.x += t0.x * s0
    p0.y += t0.y * s0
    p1.x += t1.x * s1
    p1.y += t1.y * s1
    const n0 = p0.side === 'h' ? { x: p0.sign, y: 0 } : { x: 0, y: p0.sign }
    const n1 = p1.side === 'h' ? { x: p1.sign, y: 0 } : { x: 0, y: p1.sign }
    return {
      p0,
      p1,
      c1: { x: p0.x + n0.x * off * fa, y: p0.y + n0.y * off * fa },
      c2: { x: p1.x + n1.x * off * fb, y: p1.y + n1.y * off * fb },
    }
  }
  // fast path: natural anchors on the chord's dominant axis
  let best = make(baseAxis, baseAxis, 0, 0, 1, 1)
  best.hits = cubicHitCount(best.p0, best.c1, best.c2, best.p1, rects, idA, idB)
  best.cost = best.hits * 100
  if (best.hits === 0) return best
  for (const axis0 of [baseAxis, baseAxis === 'h' ? 'v' : 'h']) {
    for (const axis1 of [baseAxis, baseAxis === 'h' ? 'v' : 'h']) {
      for (const s0 of [0, -20, 20]) {
        for (const s1 of [0, -20, 20]) {
          for (const fa of [1, 1.4, 0.65]) {
            for (const fb of [1, 1.4, 0.65]) {
              const cand = make(axis0, axis1, s0, s1, fa, fb)
              cand.hits = cubicHitCount(cand.p0, cand.c1, cand.c2, cand.p1, rects, idA, idB)
              const axisPenalty = (axis0 === baseAxis ? 0 : 0.15) + (axis1 === baseAxis ? 0 : 0.15)
              const cost = cand.hits * 100 + axisPenalty + Math.abs(fa - 1) + Math.abs(fb - 1) + (Math.abs(s0) + Math.abs(s1)) / 80
              if (cost < best.cost) {
                best = cand
                best.cost = cost
              }
            }
          }
        }
      }
    }
  }
  return best
}

function edgePathD(points) {
  if (!points || points.length < 2) return ''
  let d = `M ${points[0].x.toFixed(2)} ${points[0].y.toFixed(2)}`
  for (let i = 0; i < points.length - 1; i++) {
    const p0 = points[Math.max(0, i - 1)]
    const p1 = points[i]
    const p2 = points[i + 1]
    const p3 = points[Math.min(points.length - 1, i + 2)]
    const c1x = p1.x + (p2.x - p0.x) / 6
    const c1y = p1.y + (p2.y - p0.y) / 6
    const c2x = p2.x - (p3.x - p1.x) / 6
    const c2y = p2.y - (p3.y - p1.y) / 6
    d += ` C ${c1x.toFixed(2)} ${c1y.toFixed(2)} ${c2x.toFixed(2)} ${c2y.toFixed(2)} ${p2.x.toFixed(2)} ${p2.y.toFixed(2)}`
  }
  return d
}

// Route between two cards: sample the natural curve, push samples out of every
// other card, then smooth — the result feeds a single bezier path.
function routeEdgePoints(p0, c1, c2, p1, idA, idB, rects) {
  const pts = []
  for (let i = 0; i <= 40; i++) pts.push(bezierPoint(p0, c1, c2, p1, i / 40))
  const margin = 14
  const pushOut = (pt, gap) => {
    for (const r of rects) {
      if (r.id === idA || r.id === idB) continue
      const x0 = r.x - gap
      const x1 = r.x + r.w + gap
      const y0 = r.y - gap
      const y1 = r.y + r.h + gap
      if (pt.x <= x0 || pt.x >= x1 || pt.y <= y0 || pt.y >= y1) continue
      const dl = pt.x - x0
      const dr = x1 - pt.x
      const du = pt.y - y0
      const dd = y1 - pt.y
      const min = Math.min(dl, dr, du, dd)
      if (min === dl) pt.x = x0
      else if (min === dr) pt.x = x1
      else if (min === du) pt.y = y0
      else pt.y = y1
    }
  }
  for (let pass = 0; pass < 6; pass++) {
    for (let i = 1; i < pts.length - 1; i++) pushOut(pts[i], margin)
    for (let i = 1; i < pts.length - 1; i++) {
      const a = pts[i - 1]
      const b = pts[i]
      const c = pts[i + 1]
      pts[i] = { x: a.x * 0.24 + b.x * 0.52 + c.x * 0.24, y: a.y * 0.24 + b.y * 0.52 + c.y * 0.24 }
    }
  }
  for (let i = 1; i < pts.length - 1; i++) pushOut(pts[i], margin - 2)
  const step = Math.max(1, Math.ceil(pts.length / 12))
  const out = pts.filter((_, i) => i === 0 || i === pts.length - 1 || i % step === 0)
  if (out[out.length - 1] !== pts[pts.length - 1]) out.push(pts[pts.length - 1])
  return out
}

function bezierPoint(p0, c1, c2, p1, t) {
  const mt = 1 - t
  const x = mt * mt * mt * p0.x + 3 * mt * mt * t * c1.x + 3 * mt * t * t * c2.x + t * t * t * p1.x
  const y = mt * mt * mt * p0.y + 3 * mt * mt * t * c1.y + 3 * mt * t * t * c2.y + t * t * t * p1.y
  return { x, y }
}

let dotPatternLight = null
let dotPatternDark = null
function dotPattern(dark) {
  if (dark && dotPatternDark) return dotPatternDark
  if (!dark && dotPatternLight) return dotPatternLight
  const canvas = document.createElement('canvas')
  canvas.width = 6
  canvas.height = 6
  const ctx = canvas.getContext('2d')
  ctx.fillStyle = dark ? 'rgba(255,255,255,0.14)' : 'rgba(15,35,50,0.12)'
  ctx.beginPath()
  ctx.arc(3, 3, 0.7, 0, Math.PI * 2)
  ctx.fill()
  const pattern = ctx.createPattern(canvas, 'repeat')
  if (dark) dotPatternDark = pattern
  else dotPatternLight = pattern
  return pattern
}

const CIRCLE_ACTIVE_BPS = 15 * 1024 // 15 kb/s
const GROUP_PALETTE = ['#3c96ca', '#26a69a', '#f59e0b', '#ab47bc', '#ef5350', '#7e57c2']

function groupOf(n) {
  const g = n?.p2p_group
  if (g != null && String(g).trim() !== '') return String(g)
  return n?.network_name || '默认'
}

function flagOf(node) {
  const code = (node?.country_code || '').toLowerCase()
  return `https://flagcdn.com/${code.length === 2 ? code : 'un'}.svg`
}

function isRelayGroup(g) {
  return /relay/i.test(String(g))
}

/** Loss -> colour ramp: green (healthy) .. yellow .. orange .. red. */
function lossColor(lossPct) {
  const loss = Math.max(0, Math.min(100, Number(lossPct) || 0))
  const stops = [
    { at: 0.0, rgb: [134, 217, 147] },
    { at: 0.5, rgb: [180, 226, 130] },
    { at: 1.0, rgb: [255, 214, 102] },
    { at: 3.0, rgb: [255, 152, 67] },
    { at: 8.0, rgb: [239, 83, 80] },
    { at: 30.0, rgb: [198, 40, 40] },
  ]
  let lo = stops[0]
  let hi = stops[stops.length - 1]
  for (let i = 0; i < stops.length - 1; i++) {
    if (loss >= stops[i].at && loss <= stops[i + 1].at) {
      lo = stops[i]
      hi = stops[i + 1]
      break
    }
  }
  const span = hi.at - lo.at || 1
  const t = Math.max(0, Math.min(1, (loss - lo.at) / span))
  const rgb = lo.rgb.map((c, i) => Math.round(c + (hi.rgb[i] - c) * t))
  return `rgb(${rgb[0]},${rgb[1]},${rgb[2]})`
}

// ---------------------------------------------------------------- group level view
// "普通拓扑" shows only the groups (collapsed cards); clicking one drills into
// the intra-group topology. Cards can be dragged around freely and links are
// bezier curves, same interaction freedom as the node view.
const GROUP_BOX_W = 152
const GROUP_BOX_H = 68

function ensureGroupLayout(groups) {
  const count = groups.length || 1
  const preset = {
    cn: [0.17, 0.5],
    'ix-sh': [0.5, 0.28],
    'ix-sz': [0.5, 0.72],
    overseas: [0.83, 0.5],
  }
  const spread = Math.max(130, Math.min(width, height) * 0.28)
  groups.forEach((g, i) => {
    if (groupLayout.has(g)) return
    const p = preset[g]
    if (p) {
      groupLayout.set(g, { x: width * p[0], y: height * p[1] })
      return
    }
    const a = -Math.PI / 2 + (Math.PI * 2 * i) / count
    groupLayout.set(g, { x: width / 2 + spread * Math.cos(a), y: height / 2 + spread * Math.sin(a) })
  })
  for (const key of [...groupLayout.keys()]) {
    if (!groups.includes(key)) groupLayout.delete(key)
  }
}

function groupLinkStats() {
  const { groups, byGroup, pairs } = overviewData()
  const nodeGroup = new Map()
  for (const g of groups) for (const n of byGroup.get(g) || []) nodeGroup.set(n.peer_id, g)
  const agg = new Map()
  for (const pr of pairs) {
    const ga = nodeGroup.get(pr.a)
    const gb = nodeGroup.get(pr.b)
    if (!ga || !gb || ga === gb) continue
    const key = ga < gb ? `${ga}|${gb}` : `${gb}|${ga}`
    const rec = agg.get(key) || { a: ga < gb ? ga : gb, b: ga < gb ? gb : ga, traffic: 0, latencies: [] }
    rec.traffic += pr.traffic || 0
    if (pr.latency != null) rec.latencies.push(pr.latency)
    agg.set(key, rec)
  }
  return { groups, byGroup, nodeGroup, agg }
}

// Anchor a link on the card side of a FIXED orientation axis. Using the same
// axis at both ends keeps the two outward normals anti-parallel, which is what
// makes the port-local curve monotone (no belly / no S bend).
function anchorAxis(from, to, axis) {
  if (axis === 'h') {
    const sign = to.x >= from.x ? 1 : -1
    return { x: from.x + sign * 88, y: from.y, side: 'h', sign }
  }
  const sign = to.y >= from.y ? 1 : -1
  return { x: from.x, y: from.y + sign * 31, side: 'v', sign }
}

function groupAnchorAxis(from, to, axis) {
  if (axis === 'h') {
    const sign = to.x >= from.x ? 1 : -1
    return { x: from.x + sign * (GROUP_BOX_W / 2 + 3), y: from.y, nx: sign, ny: 0 }
  }
  const sign = to.y >= from.y ? 1 : -1
  return { x: from.x, y: from.y + sign * (GROUP_BOX_H / 2 + 3), nx: 0, ny: sign }
}

function groupAnchor(from, to) {
  const dx = to.x - from.x
  const dy = to.y - from.y
  // anchor on the card side facing the other card + outward normal of that side
  if (Math.abs(dx) * GROUP_BOX_H > Math.abs(dy) * GROUP_BOX_W) {
    const sign = dx >= 0 ? 1 : -1
    return { x: from.x + sign * (GROUP_BOX_W / 2 + 3), y: from.y, nx: sign, ny: 0 }
  }
  const sign = dy >= 0 ? 1 : -1
  return { x: from.x, y: from.y + sign * (GROUP_BOX_H / 2 + 3), nx: 0, ny: sign }
}

function buildGroupLevelOption() {
  const { groups, byGroup, agg } = groupLinkStats()
  ensureGroupLayout(groups)
  const dark = document.querySelector('.v-application')?.classList.contains('v-theme--PurpleThemeDark')
  const gScale = view.scale
  const boxes = groups.map((g) => {
    const world = groupLayout.get(g) || { x: width / 2, y: height / 2 }
    const p = toScreen(world)
    return {
      group: g,
      x: p.x, y: p.y,
      w: GROUP_BOX_W * gScale,
      h: GROUP_BOX_H * gScale,
      nodes: (byGroup.get(g) || []).length,
      color: groupColor(g),
      scale: gScale,
    }
  })
  const groupRects = boxes.map((b) => ({
    id: b.group,
    x: b.x - b.w / 2 - 6,
    y: b.y - b.h / 2 - 6,
    w: b.w + 12,
    h: b.h + 12,
  }))
  const boxData = [...agg.values()].map((rec) => {
    const wa = groupLayout.get(rec.a)
    const wb = groupLayout.get(rec.b)
    if (!wa || !wb) return null
    const pa = toScreen(wa)
    const pb = toScreen(wb)
    const axis = Math.abs(pb.x - pa.x) >= Math.abs(pb.y - pa.y) ? 'h' : 'v'
    const p0 = groupAnchorAxis(pa, pb, axis)
    const p1 = groupAnchorAxis(pb, pa, axis)
    const dx = p1.x - p0.x
    const dy = p1.y - p0.y
    const seg = Math.hypot(dx, dy) || 1
    const hash = (rec.a + '|' + rec.b).split('').reduce((acc, ch) => acc + ch.charCodeAt(0), 0)
    // the link leaves (and enters) perpendicular to the card side, like an
    // elastic band pulled away from the edge mid-line
    const off = Math.min(160, seg * 0.36)
    const c1 = { x: p0.x + p0.nx * off, y: p0.y + p0.ny * off }
    const c2 = { x: p1.x + p1.nx * off, y: p1.y + p1.ny * off }
    const labelT = 0.38 + (hash % 3) * 0.12
    const labelPoint = bezierPoint(p0, c1, c2, p1, labelT)
    const active = rec.traffic >= 102400
    const lat = rec.latencies.length ? rec.latencies.reduce((a, b) => a + b, 0) / rec.latencies.length : null
    const label = bps(rec.traffic) + (lat != null ? ' · ' + lat.toFixed(0) + ' ms' : '')
    return {
      p0, p1, c1, c2,
      width: 2 + Math.min(10, Math.max(0, Math.log10(Math.max(10, rec.traffic)) - 4) * 3),
      color: active ? '#1D9E75' : 'rgba(127,127,127,0.9)',
      opacity: active ? 0.85 : 0.3,
      label,
      labelW: label.length * 6.2 + 12,
      mx: labelPoint.x,
      my: labelPoint.y,
    }
  }).filter(Boolean)
  return {
    animation: false,
    tooltip: { show: false },
    series: [
      {
        id: 'group-links',
        type: 'custom',
        coordinateSystem: 'none',
        z: 1,
        data: boxData,
        renderItem: (params) => {
          const d = boxData[params.dataIndex]
          return {
            type: 'group',
            children: [
              {
                type: 'bezierCurve',
                shape: {
                  x1: d.p0.x, y1: d.p0.y,
                  cpx1: d.c1.x, cpy1: d.c1.y,
                  cpx2: d.c2.x, cpy2: d.c2.y,
                  x2: d.p1.x, y2: d.p1.y,
                },
                style: { stroke: d.color, lineWidth: d.width, fill: 'none', opacity: d.opacity, lineCap: 'round' },
              },
              {
                type: 'rect',
                shape: { x: d.mx - d.labelW / 2, y: d.my - 8, width: d.labelW, height: 16, r: 8 },
                style: { fill: dark ? 'rgba(42,47,52,0.92)' : 'rgba(255,255,255,0.92)' },
              },
              {
                type: 'text',
                style: {
                  text: d.label, x: d.mx, y: d.my, align: 'center', verticalAlign: 'middle',
                  fill: dark ? '#c9d1d6' : '#33414a', font: '10px "Noto Sans", sans-serif',
                },
              },
            ],
          }
        },
      },
      {
        id: 'groups',
        type: 'custom',
        coordinateSystem: 'none',
        z: 3,
        data: boxes,
        renderItem: (params) => {
          const d = boxes[params.dataIndex]
          return {
            type: 'group',
            children: [
              {
                type: 'rect',
                shape: { x: d.x - d.w / 2, y: d.y - d.h / 2, width: d.w, height: d.h, r: 10 * d.scale },
                style: { fill: dark ? 'rgba(42,47,52,0.96)' : 'rgba(255,255,255,0.97)', stroke: d.color, lineWidth: 1.5 },
              },
              {
                type: 'text',
                style: {
                  text: d.group, x: d.x, y: d.y - 10 * d.scale, align: 'center', verticalAlign: 'middle',
                  fill: dark ? '#e8eaed' : '#1f2a30', fontSize: 14 * d.scale, fontWeight: 500,
                },
              },
              {
                type: 'text',
                style: {
                  text: d.nodes + ' 节点', x: d.x, y: d.y + 12 * d.scale, align: 'center', verticalAlign: 'middle',
                  fill: dark ? '#9aa4ab' : '#5f6b73', fontSize: 11 * d.scale,
                },
              },
            ],
          }
        },
      },
    ],
  }
}

function hitGroup(x, y) {
  const world = toWorld(x, y)
  const { groups } = groupLinkStats()
  for (const g of groups) {
    const p = groupLayout.get(g)
    if (!p) continue
    if (Math.abs(world.x - p.x) <= GROUP_BOX_W / 2 && Math.abs(world.y - p.y) <= GROUP_BOX_H / 2) return g
  }
  return null
}

function groupHoverAt(x, y) {
  const g = hitGroup(x, y)
  if (!g) {
    hover.value.visible = false
    return
  }
  const { groups, byGroup, agg } = groupLinkStats()
  let traffic = 0
  for (const rec of agg.values()) {
    if (rec.a === g || rec.b === g) traffic += rec.traffic
  }
  const rect = chartEl.value.getBoundingClientRect()
  hover.value = {
    visible: true,
    x: rect.left + x,
    y: rect.top + y,
    title: g,
    rows: [
      { label: '节点数', value: String((byGroup.get(g) || []).length) },
      { label: '跨组流量', value: bps(traffic) },
      { label: '提示', value: '点击查看组内拓扑' },
    ],
  }
}

function buildDetailOption() {
  const nodes = graph.nodes || []
  const edges = graph.edges || []

  const routeEdges = new Set()
  for (let i = 1; i < routePath.value.length; i++) {
    routeEdges.add(keyOf(routePath.value[i - 1], routePath.value[i]))
  }
  const rects = nodes
    .filter((n) => layout[n.peer_id])
    .map((n) => {
      const p = layout[n.peer_id]
      const box = nodeBox(n)
      return { id: n.peer_id, x: p.x - box.w / 2 - 8, y: p.y - box.h / 2 - 8, w: box.w + 16, h: box.h + 16 }
    })
  const edgeData = []
  for (const e of edges) {
    const a = layout[e.a]
    const b = layout[e.b]
    if (!a || !b) continue
    const na = (graph.nodes || []).find((n) => n.peer_id === e.a)
    const nb = (graph.nodes || []).find((n) => n.peer_id === e.b)
    const ga = na ? groupOf(na) : ''
    const gb = nb ? groupOf(nb) : ''
    const cross = ga !== gb
    const relay = isRelayGroup(ga) || isRelayGroup(gb)
    const m = metrics[keyOf(e.a, e.b)] || {}
    const tunnels = m.tunnels && m.tunnels.length
      ? m.tunnels
      : [{
          id: 'aggregate',
          tunnel_type: (e.tunnel_types || []).join('/') || '汇总',
          latency_ms: m.latency_ms,
          loss_pct: m.loss_pct,
          rx_bps: m.rx_bps,
          tx_bps: m.tx_bps,
        }]
    // exactly two control points on the port normals; the port position along
    // the card edge may shift so the curve never touches another card
    const chosen = pickCubic(a, b, na, nb, rects, e.a, e.b)
    const p0 = chosen.p0
    const p1 = chosen.p1
    const c1 = chosen.c1
    const c2 = chosen.c2
    const mid = toScreen(bezierPoint(p0, c1, c2, p1, 0.5))
    const sp0 = toScreen(p0)
    const sc1 = toScreen(c1)
    const sc2 = toScreen(c2)
    const sp1 = toScreen(p1)
    const hitPts = []
    for (let i = 0; i <= 20; i++) {
      const pt = toScreen(bezierPoint(p0, c1, c2, p1, i / 20))
      hitPts.push([pt.x, pt.y])
    }
    const bw = (m.rx_bps || 0) + (m.tx_bps || 0)
    const active = bw >= 1024
    const onRoute = routeEdges.has(keyOf(e.a, e.b))
    const lineWidth = (relay ? 1.5 : 1) + (active ? Math.min(3.5, Math.log10(1 + bw / 1024) * 1.4) : 0) + (onRoute ? 1.8 : 0)
    const opacity = onRoute ? 1 : relay ? 0.95 : cross ? 0.6 : 0.36
    edgeData.push({
      a: e.a,
      b: e.b,
      p0: sp0,
      c1: sc1,
      c2: sc2,
      p1: sp1,
      points: hitPts,
      mid,
      tunnels,
      samples: m.samples || [],
      latency: m.latency_ms,
      loss: m.loss_pct,
      rx: m.rx_bps,
      tx: m.tx_bps,
      bw,
      color: lossColor(m.loss_pct),
      network_name: e.network_name || '',
      active,
      lineWidth,
      opacity,
    })
  }

  edgeHitCache = edgeData
  const groupNames = [...new Set(nodes.map(groupOf))].sort()
  const groupColor = new Map(groupNames.map((g, i) => [g, GROUP_PALETTE[i % GROUP_PALETTE.length]]))
  const boxes = []
  for (const g of groupNames) {
    const list = nodes.filter((n) => groupOf(n) === g && layout[n.peer_id])
    if (!list.length) continue
    let x0 = Infinity
    let y0 = Infinity
    let x1 = -Infinity
    let y1 = -Infinity
    for (const n of list) {
      const p = layout[n.peer_id]
      const w = SIZE.local.w
      const h = SIZE.local.h
      x0 = Math.min(x0, p.x - w / 2)
      x1 = Math.max(x1, p.x + w / 2)
      y0 = Math.min(y0, p.y - h / 2)
      y1 = Math.max(y1, p.y + h / 2)
    }
    const tl = toScreen({ x: x0 - 46, y: y0 - 66 })
    const br = toScreen({ x: x1 + 46, y: y1 + 42 })
    boxes.push({
      g,
      color: groupColor.get(g),
      x: tl.x,
      y: tl.y,
      w: Math.max(60, br.x - tl.x),
      h: Math.max(60, br.y - tl.y),
    })
  }
  // cross-group stubs: when drilled into one group, links leaving the group are
  // drawn as curves to the *nearest* point of the group border, labelled with
  // the destination group name
  const stubs = []
  if (drillGroup.value) {
    const localIds = new Set(nodes.map((n) => n.peer_id))
    let rx0 = Infinity
    let ry0 = Infinity
    let rx1 = -Infinity
    let ry1 = -Infinity
    for (const n of nodes) {
      const p = layout[n.peer_id]
      if (!p) continue
      rx0 = Math.min(rx0, p.x - SIZE.local.w / 2)
      rx1 = Math.max(rx1, p.x + SIZE.local.w / 2)
      ry0 = Math.min(ry0, p.y - SIZE.local.h / 2)
      ry1 = Math.max(ry1, p.y + SIZE.local.h / 2)
    }
    const padX = 46
    const padTop = 66
    const padBottom = 42
    const bx0 = rx0 - padX
    const bx1 = rx1 + padX
    const by0 = ry0 - padTop
    const by1 = ry1 + padBottom
    const seenStub = new Set()
    for (const e of allGraph.edges || []) {
      const inA = localIds.has(e.a)
      const inB = localIds.has(e.b)
      if (inA === inB) continue
      const inner = inA ? e.a : e.b
      const outer = inA ? e.b : e.a
      const outerNode = (allGraph.nodes || []).find((n) => n.peer_id === outer)
      const targetGroup = outerNode ? groupOf(outerNode) : 'external'
      const key = `${inner}:${targetGroup}`
      if (seenStub.has(key)) continue
      seenStub.add(key)
      const p = layout[inner]
      if (!p) continue
      // nearest point on the region border + its outward normal
      const clampedX = Math.max(rx0, Math.min(rx1, p.x))
      const clampedY = Math.max(ry0, Math.min(ry1, p.y))
      const clampedX2 = Math.max(bx0, Math.min(bx1, p.x))
      const clampedY2 = Math.max(by0, Math.min(by1, p.y))
      const dists = [
        { d: Math.abs(p.x - bx0), side: 'left' },
        { d: Math.abs(bx1 - p.x), side: 'right' },
        { d: Math.abs(p.y - by0), side: 'top' },
        { d: Math.abs(by1 - p.y), side: 'bottom' },
      ].sort((a, b) => a.d - b.d)
      const side = dists[0].side
      let endWorld
      let out
      if (side === 'left') { endWorld = { x: bx0, y: clampedY2 }; out = { x: -1, y: 0 } }
      else if (side === 'right') { endWorld = { x: bx1, y: clampedY2 }; out = { x: 1, y: 0 } }
      else if (side === 'top') { endWorld = { x: clampedX2, y: by0 }; out = { x: 0, y: -1 } }
      else { endWorld = { x: clampedX2, y: by1 }; out = { x: 0, y: 1 } }
      const a0 = anchor(p, endWorld, nodes.find((n) => n.peer_id === inner))
      const s0 = toScreen(a0)
      const s3 = toScreen(endWorld)
      const labelWorld = { x: endWorld.x + out.x * 16, y: endWorld.y + out.y * 16 }
      const label = toScreen(labelWorld)
      stubs.push({
        x1: s0.x, y1: s0.y,
        x2: s3.x, y2: s3.y,
        lx: label.x, ly: label.y,
        label: targetGroup,
      })
    }
  }

  const routeNodes = new Set(routePath.value)
  const focusId = hoverFocus.value
  const nodeData = nodes.map((n) => ({
    ...n,
    p: toScreen(layout[n.peer_id] || { x: width / 2, y: height / 2 }),
    dim: false,
    selected: focusId != null && n.peer_id === focusId,
    route: routeNodes.has(n.peer_id),
    multi: multiSelected.value.includes(n.peer_id),
  }))

  const dark = document.querySelector('.v-application')?.classList.contains('v-theme--PurpleThemeDark')
  const axisLine = dark ? 'rgba(255,255,255,0.16)' : 'rgba(0,0,0,0.12)'
  return {
    animation: false,
    // compiled build renders its own hover card; the chart tooltip is disabled
    tooltip: { show: false },
    series: [
      {
        id: 'stubs',
        type: 'custom',
        coordinateSystem: 'none',
        z: 2,
        silent: true,
        data: stubs,
        renderItem: (params) => {
          const d = stubs[params.dataIndex]
          const text = d.label
          const w = text.length * 7 + 14
          return {
            type: 'group',
            children: [
              {
                type: 'line',
                shape: { x1: d.x1, y1: d.y1, x2: d.x2, y2: d.y2 },
                style: { stroke: dark ? 'rgba(200,210,220,0.55)' : 'rgba(70,85,100,0.55)', lineWidth: 1.4, lineDash: [6, 4] },
              },
              {
                type: 'rect',
                shape: { x: d.lx - w / 2, y: d.ly - 9, width: w, height: 18, r: 9 },
                style: { fill: dark ? 'rgba(42,47,52,0.95)' : 'rgba(255,255,255,0.95)', stroke: dark ? 'rgba(255,255,255,0.16)' : 'rgba(15,35,50,0.14)', lineWidth: 1 },
              },
              {
                type: 'text',
                style: {
                  text, x: d.lx, y: d.ly, align: 'center', verticalAlign: 'middle',
                  fill: dark ? '#c9d1d6' : '#33414a', font: '10px "Noto Sans", sans-serif',
                },
              },
            ],
          }
        },
      },
      {
        id: 'groups',
        type: 'custom',
        coordinateSystem: 'none',
        z: 0,
        silent: true,
        data: boxes,
        renderItem: (params) => {
          const d = boxes[params.dataIndex]
          return {
            type: 'group',
            children: [
              {
                type: 'rect',
                shape: { x: d.x, y: d.y, width: d.w, height: d.h, r: 18 },
                style: {
                  fill: dark ? 'rgba(255,255,255,0.02)' : 'rgba(15,35,50,0.02)',
                  stroke: d.color,
                  lineWidth: 1,
                  lineDash: [6, 6],
                  opacity: dark ? 0.34 : 0.26,
                },
              },
              {
                type: 'text',
                style: {
                  text: d.g,
                  x: d.x + 14 * view.scale,
                  y: d.y - 6 * view.scale,
                  fill: d.color,
                  fontSize: Math.max(10, 13 * view.scale),
                  fontWeight: 600,
                  opacity: dark ? 0.9 : 0.85,
                },
              },
            ],
          }
        },
      },
      {
        id: 'edges',
        type: 'custom',
        coordinateSystem: 'none',
        // links are routed around cards; below the card layer as a safety net
        z: 1,
        data: edgeData,
        renderItem: (params, api) => {
          const d = edgeData[params.dataIndex]
          const shape = {
            x1: d.p0.x, y1: d.p0.y,
            cpx1: d.c1.x, cpy1: d.c1.y,
            cpx2: d.c2.x, cpy2: d.c2.y,
            x2: d.p1.x, y2: d.p1.y,
          }
          return {
            type: 'group',
            children: [
              {
                type: 'bezierCurve',
                shape,
                style: { stroke: 'rgba(0,0,0,0.001)', lineWidth: 18, fill: 'none' },
              },
              {
                type: 'bezierCurve',
                shape,
                style: {
                  stroke: d.color,
                  lineWidth: d.lineWidth,
                  fill: 'none',
                  lineDash: d.active ? [8, 6] : undefined,
                  lineDashOffset: d.active ? -flowOffset : 0,
                  opacity: d.opacity ?? 0.9,
                  lineCap: 'round',
                },
              },
            ],
          }
        },
      },
      {
        id: 'nodes',
        type: 'custom',
        coordinateSystem: 'none',
        z: 3,
        data: nodeData,
        renderItem: (params) => {
          const d = nodeData[params.dataIndex]
          const box = nodeBox(d)
          const w = box.w * view.scale
          const h = box.h * view.scale
          const r = 12 * view.scale
          const dark = document.querySelector('.v-application')?.classList.contains('v-theme--PurpleThemeDark')
          const children = [
            {
              type: 'rect',
              shape: { x: d.p.x - w / 2, y: d.p.y - h / 2, width: w, height: h, r },
              style: {
                fill: dark ? 'rgba(42,47,52,0.96)' : 'rgba(255,255,255,0.96)',
                stroke: dark ? 'rgba(255,255,255,0.18)' : 'rgba(15,35,50,0.16)',
                lineWidth: 1,
                shadowBlur: 18,
                shadowColor: dark ? 'rgba(0,0,0,0.35)' : 'rgba(15,35,50,0.10)',
              },
            },
            {
              type: 'rect',
              shape: { x: d.p.x - w / 2, y: d.p.y - h / 2, width: w, height: h, r },
              style: { fill: dotPattern(dark), stroke: 'none' },
            },
            (() => {
              const label = d.alias || d.hostname || String(d.peer_id)
              const flagW = 18 * view.scale
              const flagH = 12 * view.scale
              const gap = 5 * view.scale
              const measure = (text, fontSize) =>
                [...text].reduce((sum, ch) => sum + (/[\u2e80-\u9fff\uf900-\ufaff]/.test(ch) ? fontSize : fontSize * 0.62), 0)
              let fontSize = Math.max(9, 12 * view.scale)
              const maxW = Math.max(30 * view.scale, 170 * view.scale - 24 * view.scale - flagW - gap)
              let textW = measure(label, fontSize)
              if (textW > maxW) {
                fontSize = Math.max(8 * view.scale, (fontSize * maxW) / textW)
                textW = measure(label, fontSize)
              }
              const blockW = flagW + gap + textW
              const x0 = d.p.x - blockW / 2
              return {
                type: 'group',
                children: [
                  {
                    type: 'image',
                    style: {
                      image: flagOf(d),
                      x: x0,
                      y: d.p.y - 9 * view.scale - flagH / 2,
                      width: flagW,
                      height: flagH,
                    },
                  },
                  {
                    type: 'text',
                    style: {
                      text: label,
                      x: x0 + flagW + gap,
                      y: d.p.y - 9 * view.scale,
                      fill: dark ? '#e8eaed' : '#1f2a30',
                      fontSize,
                      fontWeight: 500,
                      align: 'left',
                      verticalAlign: 'middle',
                    },
                  },
                ],
              }
            })(),
            {
              type: 'text',
              style: {
                text: d.ipv4 || d.ipv6 || '无 IP',
                x: d.p.x,
                y: d.p.y + 10 * view.scale,
                fill: dark ? 'rgba(232,234,237,0.58)' : 'rgba(31,42,48,0.55)',
                fontSize: Math.max(8, 10 * view.scale),
                align: 'center',
                verticalAlign: 'middle',
              },
            },
          ]
          if (d.route) {
            children.push({
              type: 'rect',
              shape: { x: d.p.x - w / 2 - 6, y: d.p.y - h / 2 - 6, width: w + 12, height: h + 12, r: r + 5 },
              style: { fill: 'none', stroke: '#3c96ca', lineWidth: 2, opacity: 0.95 },
            })
          }
          if (d.selected) {
            children.push({
              type: 'rect',
              shape: { x: d.p.x - w / 2 - 4, y: d.p.y - h / 2 - 4, width: w + 8, height: h + 8, r: r + 3 },
              style: { fill: 'none', stroke: '#3c96ca', lineWidth: 1.5, opacity: 0.9 },
            })
          }
          if (d.multi) {
            children.push({
              type: 'rect',
              shape: { x: d.p.x - w / 2 - 7, y: d.p.y - h / 2 - 7, width: w + 14, height: h + 14, r: r + 6 },
              style: { fill: 'none', stroke: '#26a69a', lineWidth: 2, lineDash: [5, 3], opacity: 0.95 },
            })
          }
          return { type: 'group', children }
        },
      },
    ],
  }
}


// ---------------------------------------------------------------- overview modes
const OVERVIEW_PALETTE = ['#1D9E75', '#BA7517', '#BA7517', '#7F77DD', '#4F8AC9', '#C0605A', '#5F8FA8', '#8A5CD6']
const PREFERRED_GROUP_ORDER = ['cn', 'ix-sh', 'ix-sz', 'overseas']
let circleGeom = null
let matrixGeom = null

function overviewData() {
  const nodes = [...knownNodes.values()]
  const pairs = (allGraph.edges || []).map((e) => ({
    a: e.a,
    b: e.b,
    traffic: Number(e.rx_bps || 0) + Number(e.tx_bps || 0),
    latency: e.latency_ms != null ? Number(e.latency_ms) : null,
    loss: e.loss_pct != null ? Number(e.loss_pct) : null,
    network_name: e.network_name || '',
  }))
  const byGroup = new Map()
  for (const n of nodes) {
    const g = groupOf(n) || 'default'
    if (!byGroup.has(g)) byGroup.set(g, [])
    byGroup.get(g).push(n)
  }
  const groups = [...byGroup.keys()].sort((a, b) => {
    const ia = PREFERRED_GROUP_ORDER.indexOf(a)
    const ib = PREFERRED_GROUP_ORDER.indexOf(b)
    if (ia !== -1 || ib !== -1) return (ia === -1 ? 99 : ia) - (ib === -1 ? 99 : ib)
    return a.localeCompare(b)
  })
  return { nodes, pairs, groups, byGroup }
}

function groupColor(name) {
  const idx = PREFERRED_GROUP_ORDER.indexOf(name)
  return OVERVIEW_PALETTE[(idx === -1 ? 4 : idx) % OVERVIEW_PALETTE.length]
}

function shortName(n) {
  return n.alias || n.hostname || String(n.peer_id)
}

// Ring/matrix order is permanent and data-independent: group order first, then
// hostname/peer id. It never reshuffles when metrics update and it does not
// depend on which nodes are online right now.
function orderedNodes(byGroup, groups) {
  const out = []
  for (const g of groups) {
    const list = (byGroup.get(g) || []).slice()
    list.sort((a, b) => {
      const na = a.alias || a.hostname || String(a.peer_id)
      const nb = b.alias || b.hostname || String(b.peer_id)
      return na.localeCompare(nb) || a.peer_id - b.peer_id
    })
    for (const node of list) out.push({ node, group: g })
  }
  return out
}


function buildCircleOption() {
  const { pairs, groups, byGroup } = overviewData()
  const dark = document.querySelector('.v-application')?.classList.contains('v-theme--PurpleThemeDark')
  const ordered = orderedNodes(byGroup, groups)
  const cx = width / 2
  const cy = height / 2
  const R = Math.max(120, Math.min(width, height) * 0.34)
  const count = ordered.length || 1
  const gaps = Math.max(0, groups.length - 1)
  const step = (Math.PI * 2 - gaps * 0.16) / count
  const P = new Map()
  const anchorSum = new Map()
  let angle = -Math.PI / 2
  let prevGroup = null
  for (const item of ordered) {
    if (prevGroup != null && item.group !== prevGroup) angle += 0.16
    prevGroup = item.group
    const x = cx + R * Math.cos(angle)
    const y = cy + R * Math.sin(angle)
    P.set(item.node.peer_id, { x, y, angle, group: item.group, node: item.node })
    const sum = anchorSum.get(item.group) || { x: 0, y: 0 }
    sum.x += Math.cos(angle)
    sum.y += Math.sin(angle)
    anchorSum.set(item.group, sum)
    angle += step
  }
  const anchors = new Map()
  for (const [g, sum] of anchorSum) {
    const a = Math.atan2(sum.y, sum.x)
    anchors.set(g, { x: cx + R * 0.3 * Math.cos(a), y: cy + R * 0.3 * Math.sin(a) })
  }
  circleGeom = { P, hit: [] }
  const sorted = pairs
    .filter((pr) => P.has(pr.a) && P.has(pr.b))
    .slice()
    .sort((a, b) => a.traffic - b.traffic)
  const hoverId = hoverCircle.value
  const edgeData = sorted.map((pr) => {
    const pa = P.get(pr.a)
    const pb = P.get(pr.b)
    const cross = pa.group !== pb.group
    let d = ''
    let labelX = (pa.x + pb.x) / 2
    let labelY = (pa.y + pb.y) / 2
    if (cross) {
      const A = anchors.get(pa.group)
      const B = anchors.get(pb.group)
      d = `M ${pa.x.toFixed(1)} ${pa.y.toFixed(1)} C ${A.x.toFixed(1)} ${A.y.toFixed(1)} ${B.x.toFixed(1)} ${B.y.toFixed(1)} ${pb.x.toFixed(1)} ${pb.y.toFixed(1)}`
      labelX = 0.125 * pa.x + 0.375 * A.x + 0.375 * B.x + 0.125 * pb.x
      labelY = 0.125 * pa.y + 0.375 * A.y + 0.375 * B.y + 0.125 * pb.y
    } else {
      const A = anchors.get(pa.group)
      const mx = (pa.x + pb.x) / 2
      const my = (pa.y + pb.y) / 2
      const cx2 = mx + (A.x - mx) * 0.6
      const cy2 = my + (A.y - my) * 0.6
      d = `M ${pa.x.toFixed(1)} ${pa.y.toFixed(1)} Q ${cx2.toFixed(1)} ${cy2.toFixed(1)} ${pb.x.toFixed(1)} ${pb.y.toFixed(1)}`
      labelX = 0.25 * pa.x + 0.5 * cx2 + 0.25 * pb.x
      labelY = 0.25 * pa.y + 0.5 * cy2 + 0.25 * pb.y
    }
    const active = pr.traffic >= CIRCLE_ACTIVE_BPS
    const isolated = hoverId != null && pr.a !== hoverId && pr.b !== hoverId
    const widthPx = active
      ? 1.6 + Math.min(5, Math.max(0, (Math.log10(Math.max(1, pr.traffic)) - Math.log10(CIRCLE_ACTIVE_BPS)) * 2.2))
      : 0.6
    return {
      d,
      color: active ? '#1D9E75' : 'rgba(127,127,127,0.9)',
      width: isolated ? widthPx : (hoverId != null ? Math.max(widthPx, 1.6) : widthPx),
      opacity: isolated ? (hoverId != null ? 0.03 : 0.09) : (active ? 0.8 : 0.09),
      active: active && !isolated,
      label: active && !isolated ? bps(pr.traffic) : '',
      labelX,
      labelY,
    }
  })
  const nodeData = ordered.map((item) => {
    const p = P.get(item.node.peer_id)
    const c = Math.cos(p.angle)
    const s = Math.sin(p.angle)
    const align = c > 0.15 ? 'left' : c < -0.15 ? 'right' : 'center'
    const lx = p.x + c * 13
    const ly = p.y + s * 13 + (Math.abs(c) <= 0.15 ? (s > 0 ? 8 : -8) : 0)
    const dim = hoverId != null && item.node.peer_id !== hoverId
    circleGeom.hit.push({ id: item.node.peer_id, x: p.x, y: p.y })
    return {
      x: p.x, y: p.y, lx, ly, align,
      label: shortName(item.node),
      color: item.node.offline ? 'rgba(127,127,127,0.55)' : groupColor(item.group),
      dim: dim || !!item.node.offline,
      offline: !!item.node.offline,
      node: item.node,
    }
  })
  // group legend along the bottom (like the reference example)
  const legendGroups = groups.slice(0, 8)
  const legendSpan = 96
  const legendStart = width / 2 - (legendGroups.length * legendSpan) / 2 + legendSpan / 2
  const legendData = legendGroups.map((g, i) => ({
    x: legendStart + i * legendSpan,
    y: height - 22,
    label: g,
    color: groupColor(g),
  }))

  return {
    animation: false,
    tooltip: { show: false },
    series: [
      {
        id: 'circle-edges',
        type: 'custom',
        coordinateSystem: 'none',
        z: 1,
        data: edgeData,
        renderItem: (params) => {
          const d = edgeData[params.dataIndex]
          const children = [{
            type: 'path',
            shape: { d: d.d },
            style: {
              stroke: d.color,
              lineWidth: d.width,
              fill: 'none',
              opacity: d.opacity,
              lineCap: 'round',
              // thick links need a proportionally larger gap so the round dash
              // caps never touch each other under heavy traffic
              lineDash: d.active ? [6 + d.width * 0.9, 5 + d.width * 1.6] : undefined,
              lineDashOffset: d.active
                ? -(((performance.now() % 900) / 900) * (11 + d.width * 2.5))
                : 0,
            },
          }]
          if (d.label) {
            children.push({
              type: 'text',
              style: {
                text: d.label,
                x: d.labelX,
                y: d.labelY - 6,
                align: 'center',
                verticalAlign: 'middle',
                fill: '#1D9E75',
                font: '10px "Noto Sans", sans-serif',
                opacity: 0.95,
              },
            })
          }
          return { type: 'group', children }
        },
      },
      {
        id: 'circle-nodes',
        type: 'custom',
        coordinateSystem: 'none',
        z: 3,
        data: nodeData,
        renderItem: (params) => {
          const d = nodeData[params.dataIndex]
          return {
            type: 'group',
            children: [
              { type: 'circle', shape: { cx: d.x, cy: d.y, r: d.offline ? 5 : 6 }, style: { fill: d.color, stroke: 'rgba(255,255,255,0.85)', lineWidth: 1.5, opacity: d.dim ? 0.35 : 1 } },
              {
                type: 'text',
                style: {
                  text: d.label, x: d.lx, y: d.ly, align: d.align, verticalAlign: 'middle',
                  fill: dark ? '#e8eaed' : '#1f2a30', font: '12px "Noto Sans", sans-serif', opacity: d.dim ? 0.3 : 1,
                },
              },
            ],
          }
        },
      },
      {
        id: 'circle-legend',
        type: 'custom',
        coordinateSystem: 'none',
        z: 4,
        data: legendData,
        silent: true,
        renderItem: (params) => {
          const d = legendData[params.dataIndex]
          return {
            type: 'group',
            children: [
              { type: 'circle', shape: { cx: d.x - 8, cy: d.y, r: 5 }, style: { fill: d.color } },
              {
                type: 'text',
                style: {
                  text: d.label, x: d.x + 2, y: d.y, align: 'left', verticalAlign: 'middle',
                  fill: dark ? '#9aa4ab' : '#5f6b73', font: '11px "Noto Sans", sans-serif',
                },
              },
            ],
          }
        },
      },
    ],
  }
}

function gridH0(gridW) {
  return gridW + 40
}

function buildMatrixOption(kind) {
  const { pairs, groups, byGroup } = overviewData()
  const ordered = orderedNodes(byGroup, groups)
  const n = ordered.length
  const indexOf = new Map()
  ordered.forEach((item, i) => indexOf.set(item.node.peer_id, i))
  const fontPx = n > 20 ? 9 : 10
  let labelW = 0
  for (const item of ordered) {
    const name = shortName(item.node)
    // rough CJK=1em / ASCII=0.55em text metrics for the label column
    let w = 0
    for (const ch of name) w += /[\u4e00-\u9fff]/.test(ch) ? fontPx : fontPx * 0.56
    labelW = Math.max(labelW, w + 42)
  }
  labelW = Math.min(labelW, Math.max(120, width * 0.45))
  const cell = Math.max(16, Math.min(46, Math.min((width - labelW - 24) / Math.max(1, n), (height - 64) / Math.max(1, n))))
  const gridW = cell * n
  const x0 = Math.round((width - (labelW + gridW)) / 2 + labelW)
  const y0 = Math.round((height - gridH0(gridW)) / 2 + 14)
  matrixGeom = { x0, y0, cell, n, ordered }
  const val = (pr) => {
    if (kind === 'traffic') return Math.max(0, Math.min(1, (Math.log10(Math.max(1, pr.traffic)) - 2) / 4.8))
    const lat = pr.latency == null ? 60 : pr.latency
    return Math.max(0, Math.min(1, (lat - 3) / 120))
  }
  const darkBg = document.querySelector('.v-application')?.classList.contains('v-theme--PurpleThemeDark')
  const baseColor = darkBg ? 'rgba(255,255,255,0.07)' : 'rgba(0,0,0,0.05)'
  const baseData = []
  for (let i = 0; i < n; i++) {
    for (let j = i + 1; j < n; j++) {
      baseData.push({ i, j })
      baseData.push({ i: j, j: i })
    }
  }
  const cellData = []
  for (const pr of pairs) {
    const i = indexOf.get(pr.a)
    const j = indexOf.get(pr.b)
    if (i == null || j == null) continue
    const v = val(pr)
    cellData.push({
      i, j, v, a: pr.a, b: pr.b, traffic: pr.traffic, latency: pr.latency,
      color: kind === 'traffic'
        ? '#1D9E75'
        : (pr.latency == null ? '#4F8AC9' : `hsl(${Math.round(150 - 150 * v)},55%,50%)`),
      opacity: kind === 'traffic'
        ? 0.06 + 0.94 * v
        : (pr.latency == null ? 0.22 : 0.35 + 0.65 * (0.35 + 0.65 * v)),
    })
  }
  const labelData = ordered.map((item, i) => ({
    i,
    group: item.group,
    name: `${i + 1} ${shortName(item.node)}`,
    number: String(i + 1),
    color: groupColor(item.group),
  }))
  const dark = document.querySelector('.v-application')?.classList.contains('v-theme--PurpleThemeDark')
  return {
    animation: false,
    tooltip: { show: false },
    series: [
      {
        id: 'matrix-base',
        type: 'custom',
        coordinateSystem: 'none',
        z: 1,
        data: baseData,
        renderItem: (params) => {
          const d = baseData[params.dataIndex]
          return {
            type: 'rect',
            shape: { x: x0 + d.j * cell + 1, y: y0 + d.i * cell + 1, width: cell - 2, height: cell - 2, r: 3 },
            style: { fill: baseColor },
          }
        },
      },
      {
        id: 'matrix-cells',
        type: 'custom',
        coordinateSystem: 'none',
        z: 2,
        data: cellData,
        renderItem: (params) => {
          const d = cellData[params.dataIndex]
          return {
            type: 'rect',
            shape: { x: x0 + d.j * cell + 1, y: y0 + d.i * cell + 1, width: cell - 2, height: cell - 2, r: 3 },
            style: { fill: d.color, opacity: d.opacity },
          }
        },
      },
      {
        id: 'matrix-labels',
        type: 'custom',
        coordinateSystem: 'none',
        z: 3,
        data: labelData,
        renderItem: (params) => {
          const d = labelData[params.dataIndex]
          return {
            type: 'group',
            children: [
              { type: 'rect', shape: { x: x0 + d.i * cell + 2, y: y0 - 13, width: Math.max(2, cell - 4), height: 5, r: 2 }, style: { fill: d.color } },
              { type: 'rect', shape: { x: x0 - 13, y: y0 + d.i * cell + 2, width: 5, height: Math.max(2, cell - 4), r: 2 }, style: { fill: d.color } },
              {
                type: 'text',
                style: { text: d.number, x: x0 + d.i * cell + cell / 2, y: y0 - 24, align: 'center', verticalAlign: 'middle', fill: dark ? '#9aa4ab' : '#5f6b73', font: '10px "Noto Sans", sans-serif' },
              },
              {
                type: 'text',
                style: { text: d.name, x: x0 - 18, y: y0 + d.i * cell + cell / 2, align: 'right', verticalAlign: 'middle', fill: dark ? '#c9d1d6' : '#33414a', font: fontPx + 'px "Noto Sans", sans-serif' },
              },
            ],
          }
        },
      },
    ],
  }
}

function latencyHue(ms) {
  const stops = [[0, 150], [50, 120], [100, 85], [200, 50], [250, 25], [320, 0]]
  if (ms <= stops[0][0]) return stops[0][1]
  for (let i = 0; i < stops.length - 1; i++) {
    const [x0, h0] = stops[i]
    const [x1, h1] = stops[i + 1]
    if (ms <= x1) {
      const t = (ms - x0) / (x1 - x0)
      return h0 + (h1 - h0) * t
    }
  }
  return 0
}

function latencyScore(pr) {
  if (pr.latency == null) return null
  const ms = Number(pr.latency)
  const hue = latencyHue(ms)
  const opacity = 0.62 + 0.38 * Math.max(0, Math.min(1, ms / 320))
  return { v: Math.max(0, Math.min(1, ms / 320)), color: `hsl(${Math.round(hue)},58%,48%)`, opacity }
}

function lossScore(pr) {
  if (pr.loss == null) return null
  const v = Math.max(0, Math.min(1, Number(pr.loss) / 5))
  return { v, color: `hsl(${Math.round(150 - 150 * v)},55%,50%)`, opacity: 0.6 + 0.4 * v }
}

function buildMatrixModel() {
  const { pairs, groups, byGroup } = overviewData()
  const ordered = orderedNodes(byGroup, groups)
  const n = ordered.length
  const dark = document.querySelector('.v-application')?.classList.contains('v-theme--PurpleThemeDark')
  if (!n) {
    matrixModel.value = { cells: [], rows: [], scale: [], cell: 20, labelW: 180, n: 0 }
    return
  }
  const kind = viewMode.value === 'traffic' ? 'traffic' : viewMode.value
  const fontPx = n > 24 ? 13 : 14
  let labelW = 0
  for (const item of ordered) {
    let w = 0
    for (const ch of shortName(item.node)) w += /[\u4e00-\u9fff]/.test(ch) ? fontPx : fontPx * 0.56
    labelW = Math.max(labelW, w + 46)
  }
  labelW = Math.min(labelW, Math.max(140, width * 0.42))
  const availW = Math.max(220, width - labelW - 32)
  const availH = Math.max(220, height - 100)
  const gapTotal = (n - 1) * 2
  const cell = Math.max(16, Math.min(48, Math.floor(Math.min((availW - gapTotal) / n, (availH - gapTotal) / n))))
  const idx = new Map()
  const nodeById = new Map()
  ordered.forEach((item, i) => {
    idx.set(item.node.peer_id, i)
    nodeById.set(item.node.peer_id, item.node)
  })
  const pairMap = new Map()
  for (const pr of pairs) {
    const i = idx.get(pr.a)
    const j = idx.get(pr.b)
    if (i == null || j == null) continue
    pairMap.set(`${Math.min(i, j)}:${Math.max(i, j)}`, pr)
  }
  const rows = ordered.map((item, i) => ({ i, name: `${i + 1} ${shortName(item.node)}`, color: groupColor(item.group), offline: !!item.node.offline }))
  let maxTraffic = 0
  for (const pr of pairMap.values()) maxTraffic = Math.max(maxTraffic, pr.traffic || 0)
  if (maxTraffic <= 0) maxTraffic = 1024 * 1024
  const trafficLogHi = Math.max(3, Math.log10(maxTraffic))
  const emptyColor = dark ? 'rgba(255,255,255,0.06)' : 'rgba(0,0,0,0.045)'
  const idleColor = dark ? 'rgba(255,255,255,0.12)' : 'rgba(0,0,0,0.09)'
  const cells = []
  for (let i = 0; i < n; i++) {
    for (let j = 0; j < n; j++) {
      const key = `${i}:${j}`
      if (i === j) {
        cells.push({ key, color: emptyColor, opacity: 0.5 })
        continue
      }
      const pr = pairMap.get(`${Math.min(i, j)}:${Math.max(i, j)}`)
      if (!pr) {
        cells.push({ key, color: emptyColor, opacity: 1 })
        continue
      }
      let color
      let opacity
      if (kind === 'traffic') {
        if (pr.traffic <= 1024) {
          color = idleColor
          opacity = 1
        } else {
          const v = Math.max(0, Math.min(1, (Math.log10(pr.traffic) - 2) / (trafficLogHi - 2)))
          color = '#1D9E75'
          opacity = 0.25 + 0.75 * v
        }
      } else {
        const scored = kind === 'loss' ? lossScore(pr) : latencyScore(pr)
        if (!scored) {
          color = '#4F8AC9'
          opacity = 0.22
        } else {
          color = scored.color
          opacity = scored.opacity
        }
        cells.push({ key, color, opacity, pair: pr, metric: kind })
        continue
      }
      cells.push({ key, color, opacity, pair: pr })
    }
  }
  const scale = []
  if (kind === 'traffic') {
    const steps = 5
    for (let k = 0; k <= steps; k++) {
      const logValue = 2 + (trafficLogHi - 2) * (k / steps)
      const v = k / steps
      scale.push({ label: bps(Math.pow(10, logValue)), color: '#1D9E75', opacity: 0.25 + 0.75 * v })
    }
    scale.push({ label: '无流量', color: idleColor, opacity: 1 })
  } else if (kind === 'latency') {
    const bands = [[25, '<50 ms'], [75, '50–100'], [150, '100–200'], [225, '200–250'], [320, '≥250 ms']]
    for (const [ms, label] of bands) {
      const hue = latencyHue(ms)
      scale.push({ label, color: `hsl(${Math.round(hue)},58%,48%)`, opacity: 0.62 + 0.38 * Math.min(1, ms / 320) })
    }
    scale.push({ label: '未测得', color: '#4F8AC9', opacity: 0.28 })
  } else {
    const bands = [[0, '0%'], [1, '1%'], [2.5, '2.5%'], [5, '5%+']]
    for (const [pct, label] of bands) {
      const v = Math.max(0, Math.min(1, pct / 5))
      scale.push({ label, color: `hsl(${Math.round(150 - 150 * v)},55%,50%)`, opacity: 0.6 + 0.4 * v })
    }
    scale.push({ label: '未测得', color: '#4F8AC9', opacity: 0.28 })
  }
  scale.push({ label: '无连接', color: emptyColor, opacity: 1 })

  matrixModel.value = { cells, rows, scale, cell, labelW, n }
}

function onMatrixClick(cell) {
  if (!cell.pair) return
  const pr = cell.pair
  const m = metrics[keyOf(pr.a, pr.b)] || {}
  hover.value.visible = false
  openEdge({
    a: pr.a,
    b: pr.b,
    network_name: pr.network_name || m.network_name || '',
    tunnels: m.tunnels || [],
    samples: m.samples || [],
    latency: m.latency_ms != null ? m.latency_ms : pr.latency,
    loss: m.loss_pct != null ? m.loss_pct : pr.loss,
    rx: m.rx_bps,
    tx: m.tx_bps,
  })
}

function onMatrixCell(cell, ev) {
  if (!cell.pair) {
    hover.value.visible = false
    return
  }
  const pr = cell.pair
  const a = (allGraph.nodes || []).find((x) => x.peer_id === pr.a)
  const b = (allGraph.nodes || []).find((x) => x.peer_id === pr.b)
  const rows = cell.metric === 'loss'
    ? [
        { label: '丢包', value: pr.loss != null ? Number(pr.loss).toFixed(2) + ' %' : '-' },
        { label: '延迟', value: pr.latency != null ? Number(pr.latency).toFixed(2) + ' ms' : '-' },
        { label: '带宽', value: bps(pr.traffic) },
      ]
    : [
        { label: '延迟', value: pr.latency != null ? Number(pr.latency).toFixed(2) + ' ms' : '-' },
        { label: '丢包', value: pr.loss != null ? Number(pr.loss).toFixed(2) + ' %' : '-' },
        { label: '带宽', value: bps(pr.traffic) },
      ]
  hover.value = {
    visible: true,
    x: ev.clientX,
    y: ev.clientY,
    title: (a ? shortName(a) : pr.a) + ' ↔ ' + (b ? shortName(b) : pr.b),
    rows,
  }
}

function buildOption() {
  if (viewMode.value === 'geo') return { animation: false, tooltip: { show: false }, series: [] }
  if (viewMode.value === 'circle') return buildCircleOption()
  if (viewMode.value === 'traffic' || viewMode.value === 'latency' || viewMode.value === 'loss') return { animation: false, tooltip: { show: false }, series: [] }
  return buildDetailOption()
}

function snapLayout() {
  if (!layoutTarget) return
  const snapped = {}
  for (const [id, p] of Object.entries(layoutTarget)) snapped[id] = { x: p.x, y: p.y }
  layout = snapped
}

function enterGroup(name) {
  drillGroup.value = name
  viewMode.value = 'detail'
  applyNetworkFilter()
  computeLayout(true)
  snapLayout()
  apply()
}

function leaveGroupView() {
  drillGroup.value = ''
  apply()
}

// hover support for the overview modes
function circleHoverAt(x, y) {
  if (!circleGeom) return
  let best = null
  let bestDist = 14
  for (const h of circleGeom.hit) {
    const d = Math.hypot(h.x - x, h.y - y)
    if (d < bestDist) { bestDist = d; best = h }
  }
  const next = best ? best.id : null
  if (next !== hoverCircle.value) {
    hoverCircle.value = next
    apply()
  }
  if (best) {
    const node = (allGraph.nodes || []).find((n) => n.peer_id === best.id)
    const rect = chartEl.value.getBoundingClientRect()
    hover.value = {
      visible: true,
      x: rect.left + x,
      y: rect.top + y,
      title: node ? shortName(node) : String(best.id),
      rows: [
        { label: '分组', value: node ? groupOf(node) || '-' : '-' },
        { label: 'Peer ID', value: String(best.id) },
        { label: 'IPv4', value: node?.ipv4 || '-' },
      ],
    }
  } else {
    hover.value.visible = false
  }
}

function matrixHoverAt(x, y) {
  if (!matrixGeom) return
  const { x0, y0, cell, n } = matrixGeom
  const j = Math.floor((x - x0) / cell)
  const i = Math.floor((y - y0) / cell)
  if (i < 0 || j < 0 || i >= n || j >= n) {
    hover.value.visible = false
    return
  }
  const a = matrixGeom.ordered[i].node
  const b = matrixGeom.ordered[j].node
  const key = keyOf(a.peer_id, b.peer_id)
  const m = metrics[key] || {}
  const lat = m.latency_ms != null ? m.latency_ms : null
  const rect = chartEl.value.getBoundingClientRect()
  hover.value = {
    visible: true,
    x: rect.left + x,
    y: rect.top + y,
    title: shortName(a) + ' ↔ ' + shortName(b),
    rows: [
      { label: '延迟', value: lat != null ? Number(lat).toFixed(2) + ' ms' : '-' },
      { label: '丢包', value: m.loss_pct != null ? Number(m.loss_pct).toFixed(2) + ' %' : '-' },
      { label: '带宽', value: bps((m.rx_bps || 0) + (m.tx_bps || 0)) },
    ],
  }
}

function apply() {
  if (viewMode.value === 'traffic' || viewMode.value === 'latency' || viewMode.value === 'loss') {
    buildMatrixModel()
    return
  }
  if (chart) chart.setOption(buildOption(), true)
}

function nodeSize(id) {
  const node = (graph.nodes || []).find((n) => n.peer_id === Number(id))
  return node?.local ? SIZE.local : SIZE.remote
}

function hitNode(x, y) {
  const world = toWorld(x, y)
  for (const n of graph.nodes || []) {
    const p = layout[n.peer_id]
    if (!p) continue
    const { w, h } = n.local ? SIZE.local : SIZE.remote
    if (Math.abs(world.x - p.x) <= w / 2 && Math.abs(world.y - p.y) <= h / 2) return n.peer_id
  }
  return null
}

function onHistMove(e) {
  if (!histEl.value) return
  const rect = histEl.value.getBoundingClientRect()
  const x = Math.min(Math.max(e.offsetX, 40), rect.width - 40)
  histLine.value = { visible: true, x }
  // compiled build shows the same hover card over the history chart
  updateHistHover(rect.left + x, rect.top + rect.height / 2)
}

function onHistOut() {
  histLine.value.visible = false
  hover.value.visible = false
}

function renderHistory() {
  if (!histEl.value) return
  if (histChart && histChart.getDom() !== histEl.value) {
    histChart.dispose()
    histChart = null
  }
  if (!histChart) histChart = echarts.init(histEl.value, null, { renderer: 'canvas' })
  const points = historyPoints.value
  const ts = points.map((p) => p.ts * 1000)
  const rangeSeconds = { live: 60, '1h': 3600, '24h': 86400, '7d': 7 * 86400, '30d': 30 * 86400 }
  const span = rangeSeconds[historyRange.value] || 60
  const end = Date.now() / 1000
  const start = end - span
  const dark = document.querySelector('.v-application')?.classList.contains('v-theme--PurpleThemeDark')
  const axisLine = dark ? 'rgba(255,255,255,0.16)' : 'rgba(0,0,0,0.12)'
  const labelColor = dark ? 'rgba(255,255,255,0.55)' : 'rgba(0,0,0,0.45)'
  const splitLine = dark ? 'rgba(255,255,255,0.06)' : 'rgba(0,0,0,0.05)'
  histChart.setOption({
    animation: false,
    axisPointer: { show: true, type: 'line', lineStyle: { color: axisLine, width: 1 } },
    grid: { left: 40, right: 40, top: 24, bottom: 22 },
    tooltip: {
      show: true,
      trigger: 'axis',
      confine: true,
      appendTo: 'body',
      backgroundColor: dark ? 'rgba(36,36,36,0.96)' : 'rgba(255,255,255,0.98)',
      borderColor: dark ? 'rgba(255,255,255,0.14)' : 'rgba(0,0,0,0.1)',
      borderWidth: 1,
      padding: 10,
      textStyle: { color: dark ? '#e8eaed' : '#1f2a30', fontSize: 12 },
      extraCssText: 'border-radius:12px;box-shadow:0 10px 28px rgba(16,24,40,.16);',
      formatter: (params) => {
        const p = params?.[0]
        if (!p) return ''
        const item = historyPoints.value[p.dataIndex]
        if (!item) return ''
        const time = new Date(item.ts * 1000).toLocaleString()
        const lat = item.latency_ms != null ? Number(item.latency_ms).toFixed(2) + ' ms' : '-'
        const loss = item.loss_pct != null ? Number(item.loss_pct).toFixed(2) + ' %' : '-'
        return `<div style="font-weight:600;margin-bottom:5px">${time}</div><div>延迟&nbsp;&nbsp;${lat}</div><div>丢包&nbsp;&nbsp;${loss}</div>`
      },
    },
    xAxis: {
      type: 'time',
      min: start * 1000,
      max: end * 1000,
      axisLabel: { fontSize: 9, color: labelColor, hideOverlap: true },
      axisLine: { lineStyle: { color: axisLine } },
      axisTick: { lineStyle: { color: axisLine } },
    },
    yAxis: [
      { type: 'value', name: 'ms', nameTextStyle: { fontSize: 9, color: labelColor }, axisLabel: { fontSize: 9, color: labelColor }, axisLine: { lineStyle: { color: axisLine } }, splitLine: { lineStyle: { color: splitLine } } },
      { type: 'value', name: '%', position: 'right', nameTextStyle: { fontSize: 9, color: labelColor }, axisLabel: { fontSize: 9, color: labelColor }, axisLine: { lineStyle: { color: axisLine } }, splitLine: { show: false } },
    ],
    series: [
      { name: '延迟', type: 'line', smooth: 0.8, showSymbol: false, data: ts.map((t, i) => [t, points[i].latency_ms]), lineStyle: { width: 1.5 }, color: '#3c96ca' },
      { name: '丢包', type: 'line', smooth: 0.8, showSymbol: false, yAxisIndex: 1, data: ts.map((t, i) => [t, points[i].loss_pct]), lineStyle: { width: 1 }, color: '#fb8c00' },
    ],
  }, { notMerge: false, replaceMerge: ['series'] })
  histChart.resize()
  const zr = histChart.getZr()
  zr.off('mousemove', onHistMove)
  zr.off('globalout', onHistOut)
  zr.on('mousemove', onHistMove)
  zr.on('globalout', onHistOut)
}

function updateHistHover(clientX, clientY) {
  const points = historyPoints.value
  if (!points.length || !histEl.value) { hover.value.visible = false; return }
  const rect = histEl.value.getBoundingClientRect()
  const x = clientX == null ? histLine.value.x : clientX - rect.left
  const y = clientY == null ? rect.height / 2 : clientY - rect.top
  const [time] = histChart.convertFromPixel({ xAxisIndex: 0 }, [x, y])
  let best = points[0]
  let bestDiff = Infinity
  for (const p of points) {
    const diff = Math.abs(p.ts * 1000 - time)
    if (diff < bestDiff) { bestDiff = diff; best = p }
  }
  hover.value = {
    visible: true,
    x: rect.left + x,
    y: rect.top + y,
    title: new Date(best.ts * 1000).toLocaleString(),
    rows: [
      { label: '延迟', value: best.latency_ms != null ? `${Number(best.latency_ms).toFixed(2)} ms` : '-' },
      { label: '丢包', value: best.loss_pct != null ? `${Number(best.loss_pct).toFixed(2)} %` : '-' },
    ],
  }
}

function mergePoints(base, extra) {
  const map = new Map()
  for (const p of base || []) map.set(p.ts, p)
  for (const p of extra || []) map.set(p.ts, p)
  return [...map.values()].sort((a, b) => a.ts - b.ts)
}

async function loadLiveBackfill() {
  const item = selected.value
  if (!item || item.kind !== 'edge') return
  try {
    const network = item.network_name || (graph.edges || []).find((e) => keyOf(e.a, e.b) === keyOf(item.a, item.b))?.network_name || ''
    const data = await api.boardHistory(network, item.a, item.b, '60s')
    const key = keyOf(item.a, item.b)
    const map = new Map()
    for (const p of edgeSamples[key] || []) map.set(p.ts, p)
    for (const p of data.points || []) map.set(p.ts, p)
    edgeSamples[key] = [...map.values()].sort((a, b) => a.ts - b.ts).slice(-60)
    if (historyRange.value === 'live') syncLive()
  } catch {}
}

async function loadHistory() {
  const item = selected.value
  if (!item || item.kind !== 'edge') return
  if (historyRange.value === 'live') { syncLive(); return }
  try {
    const network = item.network_name || (graph.edges || []).find((e) => keyOf(e.a, e.b) === keyOf(item.a, item.b))?.network_name || ''
    const data = await api.boardHistory(network, item.a, item.b, historyRange.value)
    historyPoints.value = mergePoints(data.points || [], item.samples || [])
    renderHistory()
  } catch {
    historyPoints.value = mergePoints([], item.samples || [])
    renderHistory()
  }
}

function syncLive() {
  const item = selected.value
  if (!item || item.kind !== 'edge') return
  const now = Date.now() / 1000
  const points = (edgeSamples[keyOf(item.a, item.b)] || item.samples || [])
    .filter((p) => p.ts >= now - 60)
    .slice(-120)
  historyPoints.value = points
  renderHistory()
}

watch(historyRange, (range) => {
  if (range === 'live') syncLive()
  else loadHistory()
})

watch(histEl, (el) => {
  if (!el && histChart) {
    histChart.dispose()
    histChart = null
  }
})

async function openEdge(item) {
  selected.value = {
    ...item,
    kind: 'edge',
    nameA: nodeName(item.a),
    nameB: nodeName(item.b),
  }
  try {
    const data = await api.boardLinks()
    const pair = data.links || []
    const found = pair.find((l) => {
      const m = l.metrics
      return m && keyOf(m.a_peer_id, m.b_peer_id) === keyOf(item.a, item.b)
    })
    const c = found?.config
    form.value = {
      bandwidth_mbps: c?.bandwidth_mbps ?? null,
      latency_weight: c?.latency_weight ?? '',
      loss_weight: c?.loss_weight ?? '',
      bandwidth_weight: c?.bandwidth_weight ?? '',
      note: c?.note ?? '',
      disabled: !!c?.disabled,
    }
  } catch {
    form.value = { bandwidth_mbps: null, latency_weight: '', loss_weight: '', bandwidth_weight: '', note: '', disabled: false }
  }
  drawer.value = true
  historyRange.value = 'live'
  historyPoints.value = (edgeSamples[keyOf(item.a, item.b)] || item.samples || []).slice(-60)
  await nextTick()
  renderHistory()
  loadLiveBackfill()
  setTimeout(() => {
    if (selected.value?.kind === 'edge') {
      renderHistory()
      histChart?.resize()
    }
  }, 80)
  setTimeout(() => {
    if (selected.value?.kind === 'edge') {
      renderHistory()
      histChart?.resize()
    }
  }, 260)
}

async function save() {
  if (!selected.value) return
  saving.value = true
  try {
    const f = form.value
    await api.updateLink(selected.value.a, selected.value.b, {
      bandwidth_mbps: f.bandwidth_mbps === '' ? null : f.bandwidth_mbps,
      latency_weight: f.latency_weight === '' ? null : f.latency_weight,
      loss_weight: f.loss_weight === '' ? null : f.loss_weight,
      bandwidth_weight: f.bandwidth_weight === '' ? null : f.bandwidth_weight,
      note: f.note,
      disabled: f.disabled,
    })
    await load()
  } finally {
    saving.value = false
  }
}

function showHoverAt(x, y) {
  const nodeId = hitNode(x, y)
  if (nodeId != null) {
    const d = (graph.nodes || []).find((n) => n.peer_id === nodeId)
    if (d) {
      const rect = chartEl.value.getBoundingClientRect()
      hover.value = {
        visible: true,
        x: rect.left + x,
        y: rect.top + y,
        title: d.alias || d.hostname || String(d.peer_id),
        rows: [
          { label: 'Peer ID', value: String(d.peer_id) },
          { label: 'IPv4', value: d.ipv4 || '-' },
          { label: '版本', value: d.version || '-' },
        ],
      }
      if (hoverFocus.value !== d.peer_id) {
        hoverFocus.value = d.peer_id
        apply()
      }
      return
    }
  }
  let best = null
  let bestDist = 14
  for (const e of edgeHitCache) {
    const labelDist = Math.hypot(e.mid.x - x, e.mid.y - y)
    if (labelDist < 20) { best = e; bestDist = labelDist; continue }
    for (const p of e.points || []) {
      const d = Math.hypot(p[0] - x, p[1] - y)
      if (d < bestDist) { bestDist = d; best = e }
    }
  }
  if (!best) {
    hover.value.visible = false
    if (hoverFocus.value != null) {
      hoverFocus.value = null
      apply()
    }
    return
  }
  if (hoverFocus.value != null) {
    hoverFocus.value = null
    apply()
  }
  const rect = chartEl.value.getBoundingClientRect()
  hover.value = {
    visible: true,
    x: rect.left + x,
    y: rect.top + y,
    title: `${nodeName(best.a)} ↔ ${nodeName(best.b)}`,
    rows: [
      { label: '延迟', value: best.latency != null ? `${best.latency.toFixed(2)} ms` : '-' },
      { label: '丢包', value: best.loss != null ? `${best.loss.toFixed(2)} %` : '-' },
      {
        label: '容量',
        value: best.tunnels?.[0]?.capacity_mbps ? `${Math.round(best.tunnels[0].capacity_mbps)} Mbps` : '未测量',
      },
      {
        label: '利用率',
        value: best.tunnels?.[0]?.utilization != null ? `${(Number(best.tunnels[0].utilization) * 100).toFixed(1)} %` : '-',
      },
      { label: '接收', value: bps(best.rx) },
      { label: '发送', value: bps(best.tx) },
      { label: '协议', value: [...new Set((best.tunnels || []).map((t) => protocolLabel(t.tunnel_type)))].join(', ') || '-' },
    ],
  }
}

function marqueeScreenRect() {
  if (!marquee) {
    marqueeRect.value = null
    return
  }
  const a = toScreen({ x: marquee.x0, y: marquee.y0 })
  const b = toScreen({ x: marquee.x1, y: marquee.y1 })
  marqueeRect.value = {
    left: `${Math.min(a.x, b.x)}px`,
    top: `${Math.min(a.y, b.y)}px`,
    width: `${Math.abs(b.x - a.x)}px`,
    height: `${Math.abs(b.y - a.y)}px`,
  }
}

function finishMarquee() {
  if (!marquee) return
  const x0 = Math.min(marquee.x0, marquee.x1)
  const x1 = Math.max(marquee.x0, marquee.x1)
  const y0 = Math.min(marquee.y0, marquee.y1)
  const y1 = Math.max(marquee.y0, marquee.y1)
  const picked = marquee.add ? multiSelected.value.slice() : []
  for (const n of graph.nodes || []) {
    const p = layout[n.peer_id]
    if (!p) continue
    if (p.x >= x0 && p.x <= x1 && p.y >= y0 && p.y <= y1 && !picked.includes(n.peer_id)) {
      picked.push(n.peer_id)
    }
  }
  multiSelected.value = picked
  marquee = null
  marqueeRect.value = null
  suppressClick = true
  apply()
}

function setupDrag() {
  const zr = chart.getZr()

  function endDrag() {
    drag = null
    pan = null
    groupDrag = null
    hover.value.visible = false
    hoverFocus.value = null
    zr.setCursorStyle('default')
  }

  zr.on('mousedown', (e) => {
    if (viewMode.value !== 'detail') return
    const native = e.event || {}
    const id = hitNode(e.offsetX, e.offsetY)
    const world = toWorld(e.offsetX, e.offsetY)
    // right button: rubber-band selection of all nodes inside the area
    if (native.button === 2) {
      marquee = { x0: world.x, y0: world.y, x1: world.x, y1: world.y, add: !!native.ctrlKey }
      if (!native.ctrlKey) multiSelected.value = []
      marqueeScreenRect()
      suppressClick = true
      zr.setCursorStyle('crosshair')
      return
    }
    if (id != null) {
      // ctrl/cmd + click toggles one node in the multi selection
      if (native.ctrlKey || native.metaKey) {
        const list = multiSelected.value.slice()
        const at = list.indexOf(id)
        if (at >= 0) list.splice(at, 1)
        else list.push(id)
        multiSelected.value = list
        suppressClick = true
        apply()
        return
      }
      // dragging any selected node moves the whole selection
      if (multiSelected.value.includes(id)) {
        const offsets = {}
        for (const sid of multiSelected.value) {
          const p = layout[sid]
          if (p) offsets[sid] = { dx: world.x - p.x, dy: world.y - p.y }
        }
        groupDrag = { offsets }
        suppressClick = true
        zr.setCursorStyle('grabbing')
        return
      }
      multiSelected.value = []
      const p = layout[id]
      drag = { id, dx: world.x - p.x, dy: world.y - p.y }
      zr.setCursorStyle('grabbing')
    } else {
      pan = { x: e.offsetX, y: e.offsetY, tx: view.tx, ty: view.ty }
      zr.setCursorStyle('grabbing')
    }
  })

  zr.on('mousemove', (e) => {
    if (viewMode.value !== 'detail') return
    if (marquee) {
      const world = toWorld(e.offsetX, e.offsetY)
      marquee.x1 = world.x
      marquee.y1 = world.y
      marqueeScreenRect()
      return
    }
    if (groupDrag) {
      const world = toWorld(e.offsetX, e.offsetY)
      for (const [sid, off] of Object.entries(groupDrag.offsets)) {
        const next = { x: world.x - off.dx, y: world.y - off.dy }
        layout[sid] = next
        if (layoutTarget) layoutTarget[sid] = { ...next }
      }
      pathVersion += 1
      pathCache = new Map()
      apply()
      return
    }
    if (drag) {
      const world = toWorld(e.offsetX, e.offsetY)
      const moved = { x: world.x - drag.dx, y: world.y - drag.dy }
      layout[drag.id] = moved
      if (layoutTarget) layoutTarget[drag.id] = { ...moved }
      pathVersion += 1
      pathCache = new Map()
      apply()
      return
    }
    if (pan) {
      view.tx = pan.tx + (e.offsetX - pan.x)
      view.ty = pan.ty + (e.offsetY - pan.y)
      apply()
      return
    }
    showHoverAt(e.offsetX, e.offsetY)
    zr.setCursorStyle('grab')
  })

  zr.on('mouseup', () => {
    if (groupBoxDrag) {
      const name = groupBoxDrag.group
      const moved = groupBoxDrag.moved
      groupBoxDrag = null
      if (!moved) enterGroup(name)
      return
    }
    if (marquee) {
      finishMarquee()
      return
    }
    endDrag()
  })
  zr.on('globalout', () => {
    groupBoxDrag = null
    if (marquee) finishMarquee()
    else endDrag()
  })
  zr.on('mousewheel', (e) => {
    e.event?.preventDefault?.()
    const delta = e.event?.deltaY ?? e.wheelDelta ?? 0
    zoomBy(delta < 0 ? 1.12 : 1 / 1.12, e.offsetX, e.offsetY)
  })
  zr.on('dblclick', resetView)
}

function connectWs() {
  const proto = location.protocol === 'https:' ? 'wss' : 'ws'
  ws = new WebSocket(`${proto}://${location.host}/api/events`)
  ws.onmessage = (ev) => {
    try {
      const msg = JSON.parse(ev.data)
      if (msg.type === 'metrics') {
        metrics = {}
        for (const m of msg.links || []) metrics[keyOf(m.a_peer_id, m.b_peer_id)] = m
        apply()
        if (drawer.value && selected.value && selected.value.kind === 'edge') {
          const m = metrics[keyOf(selected.value.a, selected.value.b)]
          if (m && m.samples) {
            selected.value.samples = m.samples
            historyPoints.value = mergePoints(historyPoints.value, m.samples)
            renderHistory()
          }
        }
      }
    } catch {}
  }
  ws.onclose = () => setTimeout(connectWs, 3000)
}

function computeTunnelBps(key, tunnels, now, dt) {
  let rx = 0, tx = 0
  for (const t of tunnels) {
    const id = `${key}:${t.remote_addr || t.type || t.id || 't'}`
    const prev = prevTraffic.get(id)
    const r = Number(t.rx_bytes || 0)
    const x = Number(t.tx_bytes || 0)
    const tRx = prev ? Math.max(0, (r - prev.rx) / dt) : 0
    const tTx = prev ? Math.max(0, (x - prev.tx) / dt) : 0
    prevTraffic.set(id, { rx: r, tx: x, ts: now })
    t.rx_bps = tRx
    t.tx_bps = tTx
    rx += tRx
    tx += tTx
  }
  return { rx, tx }
}

function ingestBoard(board) {
  const now = Math.floor(Date.now() / 1000)
  const dt = lastSampleTs ? Math.max(0.5, now - lastSampleTs) : 1
  const raw = board.graph || { nodes: [], edges: [] }
  allGraph = raw
  geoModel.value = board.geo || { cities: [], links: [] }
  const present = new Set()
  for (const n of raw.nodes || []) {
    present.add(n.peer_id)
    const prev = knownNodes.get(n.peer_id)
    knownNodes.set(n.peer_id, { ...n, offline: false })
    if (prev && prev.group !== undefined) knownNodes.get(n.peer_id).group = prev.group
  }
  for (const [pid, rec] of knownNodes) {
    if (!present.has(pid)) rec.offline = true
  }
  // latency/loss history is sampled from the raw board data (like the compiled build)
  for (const e of raw.edges || []) {
    const key = keyOf(e.a, e.b)
    const tunnels = e.tunnels || []
    const lat = tunnels.map((t) => t.latency_ms).filter((v) => v != null)
    const loss = tunnels.length ? tunnels.reduce((sum, t) => sum + (t.loss_pct || 0), 0) / tunnels.length : null
    const arr = edgeSamples[key] || (edgeSamples[key] = [])
    arr.push({ ts: now, latency_ms: lat.length ? Math.min(...lat) : null, loss_pct: loss })
    if (arr.length > 240) arr.splice(0, arr.length - 240)
  }
  refreshNetworkOptions(raw.nodes)
  applyNetworkFilter()
  if (selected.value && selected.value.kind === 'edge' && historyRange.value === 'live') syncLive()
  metrics = {}
  for (const e of raw.edges || []) {
    const key = keyOf(e.a, e.b)
    const latency = e.latency_ms != null ? Number(e.latency_ms) : null
    const loss = e.loss_pct != null ? Number(e.loss_pct) : null
    const rx = Number(e.rx_bps || 0)
    const tx = Number(e.tx_bps || 0)
    const sample = { ts: now, latency_ms: latency, loss_pct: loss, bandwidth_bps: rx + tx }
    const arr = edgeSamples[key] || (edgeSamples[key] = [])
    arr.push(sample)
    if (arr.length > 60) arr.shift()
    metrics[key] = {
      latency_ms: latency,
      loss_pct: loss,
      tunnels: (e.tunnels || []).map((t) => ({
        id: t.remote_addr || t.type,
        tunnel_type: t.type || 'tcp',
        local_addr: t.local_addr,
        remote_addr: t.remote_addr,
        latency_ms: t.latency_ms,
        loss_pct: t.loss_pct,
        rx_bytes: t.rx_bytes,
        tx_bytes: t.tx_bytes,
        transport_loss_pct: t.transport_loss_pct,
        capacity_mbps: t.capacity_mbps,
        utilization: t.utilization,
        transport_rtt_us: t.transport_rtt_us,
        rx_bps: Number(t.rx_bps || 0),
        tx_bps: Number(t.tx_bps || 0),
      })),
      rx_bps: rx,
      tx_bps: tx,
      samples: arr.slice(),
      network_name: e.network_name || '',
    }
  }
  lastSampleTs = now
  refreshIsolated()
}

async function load(silent = false) {
  try {
    const board = await api.boardTopology(networkFilter.value || '', drillGroup.value || '')
    if (board && board.graph && (board.graph.nodes || []).length) {
      const wasEmpty = !(graph.nodes || []).length
      ingestBoard(board)
      if (!silent || wasEmpty) computeLayout()
      apply()
      if (selected.value && selected.value.kind === 'edge') {
        const m = metrics[keyOf(selected.value.a, selected.value.b)]
        if (m) {
          selected.value = { ...selected.value, ...m }
          if (historyRange.value === 'live') syncLive()
        }
      }
      return
    }
  } catch (e) {
    console.error('topology load failed', e)
  }
  if (silent) return
  try {
    const data = await api.topology()
    allGraph = data.graph || { nodes: [], edges: [] }
    refreshNetworkOptions(allGraph.nodes)
    applyNetworkFilter()
    metrics = {}
    for (const m of data.metrics || []) metrics[keyOf(m.a_peer_id, m.b_peer_id)] = m
    computeLayout()
    apply()
  } catch {}
}

function stopPolling() {
  if (pollTimer) clearInterval(pollTimer)
  pollTimer = null
}

function startPolling() {
  if (pollTimer) return
  pollTimer = setInterval(() => load(true), 2000)
}

function connectBoardEvents() {
  if (eventSource) return
  if (typeof EventSource === 'undefined') {
    startPolling()
    return
  }
  const qs = new URLSearchParams()
  if (networkFilter.value) qs.set('network', networkFilter.value)
  if (drillGroup.value) qs.set('group', drillGroup.value)
  eventSource = new EventSource('/api/board/events' + (qs.toString() ? `?${qs}` : ''))
  eventSource.addEventListener('topology', (ev) => {
    try {
      stopPolling()
      const data = JSON.parse(ev.data)
      if (!data.graph || !(data.graph.nodes || []).length) return
      const wasEmpty = !(graph.nodes || []).length
      ingestBoard({ graph: data.graph })
      if (wasEmpty) computeLayout()
      apply()
      if (selected.value && selected.value.kind === 'edge') {
        const m = metrics[keyOf(selected.value.a, selected.value.b)]
        if (m) {
          selected.value = { ...selected.value, ...m }
          if (historyRange.value === 'live') syncLive()
        }
      }
    } catch {}
  })
  eventSource.onopen = () => stopPolling()
  eventSource.onerror = () => {
    startPolling()
    eventSource?.close()
    eventSource = null
    if (sseRetry) clearTimeout(sseRetry)
    sseRetry = setTimeout(connectBoardEvents, 5000)
  }
}

function resize() {
  if (histChart) histChart.resize()
  const box = cardEl.value?.$el || cardEl.value
  const rect = (box || chartEl.value).getBoundingClientRect()
  width = rect.width
  height = rect.height
  if (viewMode.value === 'traffic' || viewMode.value === 'latency' || viewMode.value === 'loss') {
    buildMatrixModel()
    return
  }
  if (!chart) return
  chart.resize()
  if (viewMode.value === 'detail') computeLayout()
  apply()
}

onMounted(async () => {
  width = chartEl.value.clientWidth || width
  height = chartEl.value.clientHeight || height
  chart = echarts.init(chartEl.value)
  const seriesOf = (params) =>
    params.seriesId || (params.seriesIndex === 1 ? 'edges' : params.seriesIndex === 2 ? 'nodes' : null)
  chart.on('click', (params) => {
    if (suppressClick) { suppressClick = false; return }
    if (viewMode.value !== 'detail') {
      if (viewMode.value === 'circle' && params.seriesId === 'circle-nodes' && params.data) openNode(params.data.node)
      return
    }

    const series = seriesOf(params)
    const data = params?.data
    if (!data) return
    if (series === 'edges') {
      hoverFocus.value = null
      openEdge(data)
    } else if (series === 'nodes') {
      if (routeMode.value) {
        pickRouteNode(data)
        return
      }
      hoverFocus.value = data.peer_id
      openNode(data)
    } else {
      hoverFocus.value = null
      drawer.value = false
    }
    apply()
  })

  chartEl.value.addEventListener('mousemove', (e) => {
    const rect = chartEl.value.getBoundingClientRect()
    const localX = e.clientX - rect.left
    const localY = e.clientY - rect.top
    if (viewMode.value === 'circle') circleHoverAt(localX, localY)
    else if (viewMode.value === 'traffic' || viewMode.value === 'latency') matrixHoverAt(localX, localY)
    else if (viewMode.value === 'detail') showHoverAt(localX, localY)
    else hover.value.visible = false
  })
  chartEl.value.addEventListener('contextmenu', (e) => e.preventDefault())
  chartEl.value.addEventListener('mouseleave', () => {
    hover.value.visible = false
    if (!(selected.value && selected.value.kind === 'node')) hoverFocus.value = null
  })
  setupDrag()
  const ro = new ResizeObserver(() => resize())
  ro.observe(chartEl.value)
  resizeObserver = ro
  await load()
  await loadBoardNodes()
  nodesTimer = setInterval(loadBoardNodes, 5000)
  connectBoardEvents()
  let animTick = 0
  flowTimer = setInterval(() => {
    if (viewMode.value === 'circle') {
      flowOffset = (flowOffset + 1.5) % 12
      if (chart) chart.setOption(buildOption(), true)
      return
    }
    if (viewMode.value === 'traffic' || viewMode.value === 'latency' || viewMode.value === 'loss') {
      refreshTick += 1
      if (refreshTick % 15 === 0) buildMatrixModel()
      return
    }
    if (viewMode.value !== 'detail') return
    animTick += 1
    const moving = tickLayoutAnim()
    if (animTick % 6 === 0) flowOffset = (flowOffset + 1.5) % 14
    if ((moving || animTick % 6 === 0) && chart) chart.setOption(buildOption(), true)
  }, 33)
  window.addEventListener('resize', resize)
})

onBeforeUnmount(() => {
  window.removeEventListener('resize', resize)
  if (resizeObserver) resizeObserver.disconnect()
  if (eventSource) eventSource.close()
  if (sseRetry) clearTimeout(sseRetry)
  stopPolling()
  if (nodesTimer) clearInterval(nodesTimer)
  if (flowTimer) clearInterval(flowTimer)
  if (ws) ws.close()
  if (chart) chart.dispose()
})
</script>

<style scoped>
.range-toggle { border:1px solid rgba(var(--v-theme-on-surface),.14);border-radius:8px;overflow:hidden;background:transparent }
.range-toggle :deep(.v-btn) { min-width:46px;height:28px;padding:0 10px;font-size:11px;letter-spacing:0;text-transform:none;color:rgba(var(--v-theme-on-surface),.62);border-radius:0!important }
.range-toggle :deep(.v-btn).v-btn--active { background:rgba(var(--v-theme-primary),.1)!important;color:rgb(var(--v-theme-primary))!important }
.range-toggle :deep(.v-divider) { display:none }
.hist-wrap { position:relative;width:100%;height:180px }
.hist-pointer { position:absolute;top:0;bottom:0;width:1px;background:rgba(var(--v-theme-on-surface),.22);pointer-events:none }
.tunnel-row { padding:8px 0 }
.tunnel-row+.tunnel-row { border-top:1px dashed var(--dok-border) }
.tunnel-row__top { display:flex;align-items:center;gap:8px }
.tunnel-row__type { flex:0 0 auto }
.tunnel-row__addr { flex:1;min-width:0;font-size:12px;color:var(--dok-muted);overflow:hidden;text-overflow:ellipsis;white-space:nowrap }
.tunnel-row__stats { display:flex;gap:14px;margin-top:6px;padding-left:2px;font-size:11px;color:var(--dok-subtle) }
.topology-page { height:calc(100dvh - 62px);min-height:560px;display:flex;flex-direction:column }
.network-select { flex:0 0 200px;width:200px;max-width:200px }
.network-select :deep(.v-field) { border-radius:8px }
.topology-card { position:relative;flex:1 1 auto;min-height:0;overflow:hidden }
.geo-globe-host {
  position: absolute;
  inset: 0;
}

.topology-chart { width:100%;height:100% }
.route-panel { position:absolute;right:12px;top:12px;max-width:60%;padding:10px 12px;border:1px solid rgba(var(--v-theme-on-surface),.16);border-radius:12px;background:rgba(var(--v-theme-surface),.82);-webkit-backdrop-filter:blur(6px);backdrop-filter:blur(6px);z-index:4 }
.route-title { font-size:11px;font-weight:600;color:rgba(var(--v-theme-on-surface),.55);margin-bottom:4px }
.route-path { font-size:12.5px;color:rgba(var(--v-theme-on-surface),.86) }
.matrix-view {
  position: absolute;
  inset: 0;
  display: flex;
  align-items: center;
  justify-content: center;
  overflow: hidden;
  padding: 8px 12px;
}
.matrix-inner {
  display: flex;
  flex-direction: column;
  align-items: center;
  max-width: 100%;
  max-height: 100%;
}
.matrix-caption {
  font-size: 11px;
  color: rgba(var(--v-theme-on-surface), 0.55);
  margin-bottom: 6px;
}
.matrix-cols {
  display: grid;
  gap: 2px;
}
.matrix-num {
  font-size: 11px;
  line-height: 1.2;
  text-align: center;
  color: rgba(var(--v-theme-on-surface), 0.5);
  overflow: hidden;
}
.matrix-gridwrap {
  display: flex;
  align-items: flex-start;
  gap: 8px;
}
.matrix-rows {
  display: flex;
  flex-direction: column;
  gap: 2px;
  overflow: hidden;
}
.matrix-row {
  display: flex;
  align-items: center;
  gap: 6px;
  overflow: hidden;
}
.matrix-strip {
  width: 5px;
  height: 72%;
  border-radius: 2px;
  flex: 0 0 auto;
}
.matrix-row--offline .matrix-name { opacity: 0.45; }
.matrix-row--offline .matrix-strip { opacity: 0.45; }
.matrix-name {
  flex: 1 1 auto;
  min-width: 0;
  font-size: 14px;
  line-height: 1.2;
  text-align: right;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
  color: rgba(var(--v-theme-on-surface), 0.85);
}
.matrix-grid {
  display: grid;
  gap: 2px;
}
.matrix-cell {
  border-radius: 3px;
  min-width: 0;
  min-height: 0;
  cursor: pointer;
  transform-origin: center;
  transition:
    background-color 0.55s cubic-bezier(0.22, 0.61, 0.36, 1),
    opacity 0.55s cubic-bezier(0.22, 0.61, 0.36, 1),
    transform 0.16s ease;
}
.matrix-cell:hover {
  transform: scale(1.16);
  z-index: 5;
}
.matrix-scale {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  justify-content: center;
  gap: 12px;
  margin-bottom: 8px;
  font-size: 11px;
  color: rgba(var(--v-theme-on-surface), 0.6);
}
.matrix-scale__item {
  display: inline-flex;
  align-items: center;
  gap: 5px;
}
.matrix-scale__item i {
  width: 12px;
  height: 12px;
  border-radius: 3px;
  display: inline-block;
}
.matrix-legend {
  display: flex;
  flex-wrap: wrap;
  gap: 14px;
  margin-top: 8px;
  font-size: 11px;
  color: rgba(var(--v-theme-on-surface), 0.55);
}
.matrix-legend__item {
  display: inline-flex;
  align-items: center;
  gap: 5px;
}
.matrix-legend__item i {
  width: 8px;
  height: 8px;
  border-radius: 2px;
  display: inline-block;
}
.mode-toggle {
  border: 1px solid rgba(var(--v-theme-on-surface), 0.14);
  border-radius: 8px;
  overflow: hidden;
}
.mode-toggle :deep(.v-btn) {
  min-width: 84px;
  height: 32px;
  padding: 0 12px;
  font-size: 12px;
  letter-spacing: 0;
  text-transform: none;
  color: rgba(var(--v-theme-on-surface), 0.62);
  border-radius: 0 !important;
}
.mode-toggle :deep(.v-btn.v-btn--active) {
  background: rgba(var(--v-theme-primary), 0.1) !important;
  color: rgb(var(--v-theme-primary)) !important;
}
.mode-toggle :deep(.v-divider) { display: none; }
.marquee-box {
  position: absolute;
  border: 1px dashed rgba(60, 150, 202, 0.9);
  background: rgba(60, 150, 202, 0.08);
  border-radius: 4px;
  pointer-events: none;
}
.isolated-panel { position:absolute;left:12px;bottom:12px;width:230px;max-height:46%;display:flex;flex-direction:column;padding:10px 12px;border:1px dashed rgba(var(--v-theme-on-surface),.22);border-radius:12px;background:rgba(var(--v-theme-surface),.72);-webkit-backdrop-filter:blur(6px);backdrop-filter:blur(6px);z-index:4 }
.isolated-title { font-size:11px;font-weight:600;letter-spacing:.04em;color:rgba(var(--v-theme-on-surface),.55);margin-bottom:6px }
.isolated-body { overflow:auto;display:flex;flex-direction:column;gap:6px }
.isolated-item { display:flex;align-items:baseline;justify-content:space-between;gap:8px;font-size:12px }
.isolated-name { color:rgba(var(--v-theme-on-surface),.82);overflow:hidden;text-overflow:ellipsis;white-space:nowrap }
.isolated-reason { flex:0 0 auto;font-size:10px;color:rgba(var(--v-theme-on-surface),.45) }
.node-grid { display:grid;grid-template-columns:1fr 1fr;gap:14px 18px }
.node-value { margin-top:3px;font-size:12.5px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap }
.neighbor-block { padding:10px 0 4px }
.neighbor-head { display:flex;align-items:baseline;gap:8px;margin-bottom:4px }
.neighbor-name { font-size:13px;font-weight:600 }
.neighbor-ip { font-size:11px;color:var(--dok-subtle) }
.neighbor-meta { font-size:11px;color:var(--dok-muted) }
.tunnel-row--nested { margin-left:8px;padding-left:10px;border:0 }
.node-value,.stat-value,.tunnel-row__stats,.tunnel-row__type,.tunnel-row__addr { font-variant-numeric:tabular-nums }
.tunnel-row__stats { display:grid;grid-template-columns:repeat(3,minmax(0,1fr));gap:6px 10px;font-size:12px }
.tunnel-row__stats span { display:block;min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap }</style>
