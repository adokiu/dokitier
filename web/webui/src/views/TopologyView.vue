<template>
  <div class="topology-page">
    <div class="d-flex align-center mb-3">
      <div class="page-title">{{ t('nav.topology') }}</div>
      <v-spacer />
      <v-btn
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
        class="network-select"
      />
    </div>
    <v-card class="pa-2 topology-card">
      <div ref="chartEl" class="topology-chart"></div>
      <div v-if="isolated.length" class="isolated-panel">
        <div class="isolated-title">未连接 ({{ isolated.length }})</div>
        <div class="isolated-body">
          <div v-for="n in isolated" :key="n.key" class="isolated-item">
            <span class="isolated-name">{{ n.hostname }}</span>
            <span class="isolated-reason">{{ n.reason }}</span>
          </div>
        </div>
      </div>
      <div v-if="routePath.length" class="route-panel">
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
          <div v-for="t in nb.tunnels" :key="t.id" class="tunnel-row tunnel-row--nested">
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

        <div class="stat-label mt-3 mb-1">物理链路 ({{ (selected.tunnels || []).length || 1 }})</div>
        <div v-for="t in (selected.tunnels || [])" :key="t.id" class="tunnel-row">
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

const { t } = useI18n()

const chartEl = ref(null)
let chart = null
let ws = null
let graph = { nodes: [], edges: [] }
let allGraph = { nodes: [], edges: [] }
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
let eventSource = null
let sseRetry = null
let pollTimer = null
let flowTimer = null
let flowOffset = 0
let nodesTimer = null
let lastSampleTs = 0
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
  const net = networkFilter.value
  const nodes = (allGraph.nodes || []).filter((n) => !net || n.network_name === net)
  const ids = new Set(nodes.map((n) => n.peer_id))
  const edges = (allGraph.edges || []).filter((e) => ids.has(e.a) && ids.has(e.b) && (!net || !e.network_name || e.network_name === net))
  graph = { nodes, edges }
  if (hoverFocus.value != null && !ids.has(hoverFocus.value)) hoverFocus.value = null
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
    const mine = networkFilter.value ? insts.filter((i) => i.network_name === networkFilter.value) : insts
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
  return match ? match[1] : text
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
const layoutStart = {}

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
  const cols = [others.filter((_, i) => i % 2 === 0), relays, others.filter((_, i) => i % 2 === 1)].filter((c) => c.length)
  const COL_GAP = Math.round(Math.max(240, Math.min(330, width * 0.13)))
  const ROW_GAP = Math.round(Math.max(130, Math.min(180, height * 0.24)))
  const clusterW = COL_GAP + 170
  const nCols = cols.length
  const spacing = nCols <= 1 ? 0 : Math.min(clusterW / 2 + 85 + 150, Math.max(300, (width - clusterW - 80) / (nCols - 1)))
  const colXs = Array.from({ length: nCols }, (_, i) => width / 2 + (i - (nCols - 1) / 2) * spacing)
  const clusterSize = (c) => {
    const n = c.list.length
    const colsN = n <= 1 ? 1 : 2
    const rows = Math.ceil(n / colsN)
    return { colsN, rows, h: rows * ROW_GAP }
  }
  const cy = height / 2
  cols.forEach((col, ci) => {
    const sizes = col.map(clusterSize)
    const gap = 70
    const total = sizes.reduce((a, sz) => a + sz.h, 0) + gap * (col.length - 1)
    let y = cy - total / 2
    col.forEach((c, i) => {
      c.cx = colXs[ci]
      c.cy = y + sizes[i].h / 2
      y += sizes[i].h + gap
    })
  })

  const pos = {}
  const clusterOf = new Map()
  for (const c of clusters) for (const n of c.list) clusterOf.set(n.peer_id, c)
  const neighborInfo = (c) => {
    const info = new Map()
    for (const node of c.list) {
      let sx = 0
      let sy = 0
      let count = 0
      for (const e of edges) {
        let other = null
        if (e.a === node.peer_id) other = e.b
        else if (e.b === node.peer_id) other = e.a
        else continue
        if (clusterOf.get(other) === c) continue
        const p = pos[other]
        if (!p) continue
        sx += p.x
        sy += p.y
        count++
      }
      info.set(node.peer_id, count ? { x: sx / count, y: sy / count, ext: count } : { x: c.cx, y: c.cy, ext: 0 })
    }
    return info
  }
  const place = (c) => {
    const n = c.list.length
    if (!n) return
    if (n === 1) {
      pos[c.list[0].peer_id] = { x: c.cx, y: c.cy }
      return
    }
    const rows = Math.ceil(n / 2)
    const rowY = (r) => c.cy + (r - (rows - 1) / 2) * ROW_GAP
    const info = neighborInfo(c)
    const leftCol = []
    const rightCol = []
    for (const node of c.list) {
      const it = info.get(node.peer_id)
      if (!it.ext) continue
      if (it.x > c.cx) rightCol.push(node)
      else leftCol.push(node)
    }
    for (const node of c.list) {
      if (leftCol.includes(node) || rightCol.includes(node)) continue
      ;(leftCol.length <= rightCol.length ? leftCol : rightCol).push(node)
    }
    const placeCol = (list, colIndex) => {
      list.sort((a, b) => info.get(a.peer_id).y - info.get(b.peer_id).y || byName(a, b))
      const offset = (rows - list.length) / 2
      list.forEach((node, i) => {
        pos[node.peer_id] = { x: c.cx + (colIndex - 0.5) * COL_GAP, y: rowY(i + offset) }
      })
    }
    placeCol(leftCol, 0)
    placeCol(rightCol, 1)
  }
  clusters.forEach(place)
  for (let pass = 0; pass < 5; pass++) clusters.forEach(place)

  const CARD_W = 170
  const CARD_H = 56
  const PAD_X = 34
  const PAD_Y = 58
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
  for (let iter = 0; iter < 60; iter++) {
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
    return { x: from.x + (dx >= 0 ? 85 : -85), y: from.y, side: 'h', sign: dx >= 0 ? 1 : -1 }
  }
  return { x: from.x, y: from.y + (dy >= 0 ? 28 : -28), side: 'v', sign: dy >= 0 ? 1 : -1 }
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

function buildOption() {
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
    const p0 = anchor(a, b, na)
    const p1 = anchor(b, a, nb)
    const seg = Math.hypot(p1.x - p0.x, p1.y - p0.y)
    const off = Math.min(115, seg * (seg < 320 ? 0.18 : 0.34))
    let c1
    let c2
    if (p0.side === 'h') {
      c1 = { x: p0.x + p0.sign * off, y: p0.y }
      c2 = { x: p1.x + p1.sign * off, y: p1.y }
    } else {
      c1 = { x: p0.x, y: p0.y + p0.sign * off }
      c2 = { x: p1.x, y: p1.y + p1.sign * off }
    }
    if (curveHitsCard(p0, c1, c2, p1, rects, e.a, e.b)) {
      const perp = edgePerp(p0, p1)
      let cleared = false
      for (const mag of [24, 48, 72, 96, 120, 144, 168]) {
        for (const sign of [1, -1]) {
          const tc1 = { x: c1.x + perp.x * mag * sign, y: c1.y + perp.y * mag * sign }
          const tc2 = { x: c2.x + perp.x * mag * sign, y: c2.y + perp.y * mag * sign }
          if (!curveHitsCard(p0, tc1, tc2, p1, rects, e.a, e.b)) {
            c1 = tc1
            c2 = tc2
            cleared = true
            break
          }
        }
        if (cleared) break
      }
    }
    const mid = toScreen(bezierPoint(p0, c1, c2, p1, 0.5))
    const sp0 = toScreen(p0)
    const sc1 = toScreen(c1)
    const sc2 = toScreen(c2)
    const sp1 = toScreen(p1)
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
    const tl = toScreen({ x: x0 - 18, y: y0 - 36 })
    const br = toScreen({ x: x1 + 18, y: y1 + 14 })
    boxes.push({
      g,
      color: groupColor.get(g),
      x: tl.x,
      y: tl.y,
      w: Math.max(60, br.x - tl.x),
      h: Math.max(60, br.y - tl.y),
    })
  }
  const routeNodes = new Set(routePath.value)
  const focusId = hoverFocus.value
  const nodeData = nodes.map((n) => ({
    ...n,
    p: toScreen(layout[n.peer_id] || { x: width / 2, y: height / 2 }),
    dim: false,
    selected: focusId != null && n.peer_id === focusId,
    route: routeNodes.has(n.peer_id),
  }))

  const dark = document.querySelector('.v-application')?.classList.contains('v-theme--PurpleThemeDark')
  const axisLine = dark ? 'rgba(255,255,255,0.16)' : 'rgba(0,0,0,0.12)'
  return {
    animation: false,
    // compiled build renders its own hover card; the chart tooltip is disabled
    tooltip: { show: false },
    series: [
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
        z: 1,
        data: edgeData,
        renderItem: (params, api) => {
          const d = edgeData[params.dataIndex]
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
                style: { stroke: 'rgba(0,0,0,0.001)', lineWidth: 18, fill: 'none' },
              },
              {
                type: 'bezierCurve',
                shape: {
                  x1: d.p0.x, y1: d.p0.y,
                  cpx1: d.c1.x, cpy1: d.c1.y,
                  cpx2: d.c2.x, cpy2: d.c2.y,
                  x2: d.p1.x, y2: d.p1.y,
                },
                style: {
                  stroke: d.color,
                  lineWidth: d.lineWidth,
                  fill: 'none',
                  // only links that actually carry traffic get the moving dash
                  lineDash: d.active ? [8, 6] : undefined,
                  lineDashOffset: d.active ? -flowOffset : 0,
                  opacity: d.opacity ?? 0.9,
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
                fill: dark ? 'rgba(42,47,52,0.72)' : 'rgba(255,255,255,0.72)',
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
          return { type: 'group', children }
        },
      },
    ],
  }
}

function apply() {
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
    for (let t = 0; t <= 1.001; t += 0.05) {
      const p = bezierPoint(e.p0, e.c1, e.c2, e.p1, t)
      const d = Math.hypot(p.x - x, p.y - y)
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

function setupDrag() {
  const zr = chart.getZr()

  function endDrag() {
    drag = null
    pan = null
    hover.value.visible = false
    hoverFocus.value = null
    zr.setCursorStyle('default')
  }

  zr.on('mousedown', (e) => {
    const id = hitNode(e.offsetX, e.offsetY)
    if (id != null) {
      const p = layout[id]
      const world = toWorld(e.offsetX, e.offsetY)
      drag = { id, dx: world.x - p.x, dy: world.y - p.y }
      zr.setCursorStyle('grabbing')
    } else {
      pan = { x: e.offsetX, y: e.offsetY, tx: view.tx, ty: view.ty }
      zr.setCursorStyle('grabbing')
    }
  })

  zr.on('mousemove', (e) => {
    if (drag) {
      const world = toWorld(e.offsetX, e.offsetY)
      const moved = { x: world.x - drag.dx, y: world.y - drag.dy }
      layout[drag.id] = moved
      if (layoutTarget) layoutTarget[drag.id] = { ...moved }
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

  zr.on('mouseup', endDrag)
  zr.on('globalout', endDrag)
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
  for (const e of graph.edges || []) {
    const key = keyOf(e.a, e.b)
    const tunnels = (e.tunnels || []).map((t) => ({
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
      rx_bps: 0,
      tx_bps: 0,
    }))
    const { rx, tx } = computeTunnelBps(`${key}:${e.network_name || ''}`, tunnels, now, dt)
    const lat = tunnels.map((t) => t.latency_ms).filter((v) => v != null)
    const loss = tunnels.length ? tunnels.reduce((sum, t) => sum + (t.loss_pct || 0), 0) / tunnels.length : null
    const sample = { ts: now, latency_ms: lat.length ? Math.min(...lat) : null, loss_pct: loss, bandwidth_bps: rx + tx }
    const arr = edgeSamples[key] || (edgeSamples[key] = [])
    arr.push(sample)
    if (arr.length > 60) arr.shift()
    metrics[key] = {
      latency_ms: sample.latency_ms,
      loss_pct: loss,
      tunnels,
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
    const board = await api.boardTopology()
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
  } catch {}
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
  eventSource = new EventSource('/api/board/events')
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
  if (!chart) return
  const rect = chartEl.value.getBoundingClientRect()
  width = rect.width
  height = rect.height
  chart.resize()
  computeLayout()
  apply()
}

onMounted(async () => {
  width = chartEl.value.clientWidth || width
  height = chartEl.value.clientHeight || height
  chart = echarts.init(chartEl.value)
  const seriesOf = (params) =>
    params.seriesId || (params.seriesIndex === 1 ? 'edges' : params.seriesIndex === 2 ? 'nodes' : null)
  chart.on('click', (params) => {
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
    showHoverAt(e.clientX - rect.left, e.clientY - rect.top)
  })
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
.topology-chart { width:100%;height:100% }
.route-panel { position:absolute;right:12px;top:12px;max-width:60%;padding:10px 12px;border:1px solid rgba(var(--v-theme-on-surface),.16);border-radius:12px;background:rgba(var(--v-theme-surface),.82);-webkit-backdrop-filter:blur(6px);backdrop-filter:blur(6px);z-index:4 }
.route-title { font-size:11px;font-weight:600;color:rgba(var(--v-theme-on-surface),.55);margin-bottom:4px }
.route-path { font-size:12.5px;color:rgba(var(--v-theme-on-surface),.86) }
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
