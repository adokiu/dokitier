<template>
  <div class="topology-page">
    <div class="d-flex align-center mb-3">
      <div class="page-title">{{ t('nav.topology') }}</div>
      <v-spacer />
      <v-chip size="small" variant="text" class="mr-2">本机</v-chip>
      <v-chip size="small" variant="text" class="mr-3">远端</v-chip>
      <v-chip size="small" variant="text" class="mr-2">实时（1s）</v-chip>
      <v-btn size="small" variant="text" icon="mdi-magnify-plus-outline" @click="zoomBy(1.2)" />
      <v-btn size="small" variant="text" icon="mdi-magnify-minus-outline" @click="zoomBy(1 / 1.2)" />
      <v-btn size="small" variant="text" icon="mdi-fit-to-page-outline" @click="resetView" />
      <span class="text-caption text-medium-emphasis mr-2">点击节点或链路查看详情</span>
      <v-btn size="small" variant="text" prepend-icon="mdi-refresh" @click="load">刷新</v-btn>
    </div>
    <v-card class="pa-2 topology-card">
      <div ref="chartEl" class="topology-chart"></div>
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
              <span class="tunnel-row__type">{{ t.tunnel_type }}</span>
              <span class="tunnel-row__addr">{{ tunnelPath(t) }}</span>
            </div>
            <div class="tunnel-row__stats">
              <span>延迟 {{ t.latency_ms != null ? t.latency_ms.toFixed(2) + ' ms' : '-' }}</span>
              <span>丢包 {{ t.loss_pct != null ? t.loss_pct.toFixed(2) + ' %' : '-' }}</span>
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
          <div ref="histEl" style="width: 100%; height: 180px"></div>
          <div v-if="histLine.visible" class="hist-pointer" :style="{ left: `${histLine.x}px` }"></div>
        </div>

        <div class="stat-label mt-3 mb-1">物理链路 ({{ (selected.tunnels || []).length || 1 }})</div>
        <div v-for="t in (selected.tunnels || [])" :key="t.id" class="tunnel-row">
          <div class="tunnel-row__top">
            <span class="tunnel-row__type">{{ t.tunnel_type }}</span>
            <span class="tunnel-row__addr">{{ tunnelPath(t) }}</span>
          </div>
          <div class="tunnel-row__stats">
            <span>延迟 {{ t.latency_ms != null ? t.latency_ms.toFixed(2) + ' ms' : '-' }}</span>
            <span>丢包 {{ t.loss_pct != null ? t.loss_pct.toFixed(2) + ' %' : '-' }}</span>
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
import DetailDrawer from '@/components/DetailDrawer.vue'

const { t } = useI18n()

const chartEl = ref(null)
let chart = null
let ws = null
let graph = { nodes: [], edges: [] }
let metrics = {}
let layout = {}
let edgeHitCache = []
let width = 1200
let height = 640
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

const drawerTitle = computed(() => {
  if (!selected.value) return ''
  return selected.value.kind === 'node'
    ? selected.value.hostname || String(selected.value.peer_id)
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
      hostname: peer?.hostname || String(other),
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
  return n?.hostname || String(id)
}

function bpsOf(v) {
  if (v == null) return '-'
  const units = ['B/s', 'KB/s', 'MB/s', 'GB/s']
  let n = Number(v), i = 0
  while (n >= 1024 && i < units.length - 1) { n /= 1024; i++ }
  return `${n.toFixed(1)} ${units[i]}`
}

const SIZE = { local: { w: 170, h: 56 }, remote: { w: 150, h: 48 } }

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
function computeLayout() {
  const nodes = graph.nodes || []
  const edges = graph.edges || []
  const cx = width / 2
  const cy = height / 2
  const pos = {}
  nodes.forEach((n, i) => {
    if (nodes.length === 1) {
      pos[n.peer_id] = { x: cx, y: cy }
    } else {
      const angle = (i / nodes.length) * Math.PI * 2 - Math.PI / 2
      const radius = Math.min(width, height) * 0.28
      pos[n.peer_id] = { x: cx + Math.cos(angle) * radius, y: cy + Math.sin(angle) * radius }
    }
  })

  const neighbor = {}
  for (const e of edges) {
    ;(neighbor[e.a] ||= []).push(e.b)
    ;(neighbor[e.b] ||= []).push(e.a)
  }

  const iterations = 220
  for (let it = 0; it < iterations; it++) {
    const force = {}
    for (const n of nodes) force[n.peer_id] = { x: 0, y: 0 }

    // Repulsion between all pairs.
    for (let i = 0; i < nodes.length; i++) {
      for (let j = i + 1; j < nodes.length; j++) {
        const a = nodes[i].peer_id
        const b = nodes[j].peer_id
        let dx = pos[a].x - pos[b].x
        let dy = pos[a].y - pos[b].y
        let dist2 = dx * dx + dy * dy
        if (dist2 < 1) { dx = 1; dy = 1; dist2 = 2 }
        const dist = Math.sqrt(dist2)
        const rep = 90000 / dist2
        force[a].x += (dx / dist) * rep
        force[a].y += (dy / dist) * rep
        force[b].x -= (dx / dist) * rep
        force[b].y -= (dy / dist) * rep
      }
    }

    // Springs along edges.
    for (const e of edges) {
      const a = pos[e.a]
      const b = pos[e.b]
      if (!a || !b) continue
      const dx = b.x - a.x
      const dy = b.y - a.y
      const dist = Math.sqrt(dx * dx + dy * dy) || 1
      const target = 240
      const k = (dist - target) * 0.02
      force[e.a].x += (dx / dist) * k
      force[e.a].y += (dy / dist) * k
      force[e.b].x -= (dx / dist) * k
      force[e.b].y -= (dy / dist) * k
    }

    for (const n of nodes) {
      const p = pos[n.peer_id]
      // Gravity towards the canvas center.
      force[n.peer_id].x += (cx - p.x) * 0.004
      force[n.peer_id].y += (cy - p.y) * 0.004
      p.x += force[n.peer_id].x * 0.85
      p.y += force[n.peer_id].y * 0.85
      p.x = Math.max(110, Math.min(width - 110, p.x))
      p.y = Math.max(70, Math.min(height - 70, p.y))
    }
  }
  layout = pos
  if (import.meta.env.DEV) {
    window.__dokLayout = layout
    window.__dokRect = () => chartEl.value.getBoundingClientRect()
  }
}

/** Midpoint of the rectangle side facing the other node. */
function anchor(from, to) {
  const dx = to.x - from.x
  const dy = to.y - from.y
  if (Math.abs(dx) >= Math.abs(dy)) {
    return { x: from.x + (dx >= 0 ? 70 : -70), y: from.y, side: 'h', sign: dx >= 0 ? 1 : -1 }
  }
  return { x: from.x, y: from.y + (dy >= 0 ? 22 : -22), side: 'v', sign: dy >= 0 ? 1 : -1 }
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

function buildOption() {
  const nodes = graph.nodes || []
  const edges = graph.edges || []

  const edgeData = []
  for (const e of edges) {
    const a = layout[e.a]
    const b = layout[e.b]
    if (!a || !b) continue
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
    const p0 = anchor(a, b)
    const p1 = anchor(b, a)
    const off = p0.side === 'h'
      ? Math.abs(p1.x - p0.x) * 0.45
      : Math.abs(p1.y - p0.y) * 0.45
    let c1, c2
    if (p0.side === 'h') {
      c1 = { x: p0.x + p0.sign * off, y: p0.y }
      c2 = { x: p1.x + p1.sign * off, y: p1.y }
    } else {
      c1 = { x: p0.x, y: p0.y + p0.sign * off }
      c2 = { x: p1.x, y: p1.y + p1.sign * off }
    }
    const midWorld = bezierPoint(p0, c1, c2, p1, 0.5)
    const sp0 = toScreen(p0)
    const sc1 = toScreen(c1)
    const sc2 = toScreen(c2)
    const sp1 = toScreen(p1)
    const mid = toScreen(midWorld)
    const latency = m.latency_ms
    const bw = (m.rx_bps || 0) + (m.tx_bps || 0)
    const active = bw >= 1024
    const lineWidth = active
      ? Math.min(5, 1 + Math.log10(1 + bw / 1024) * 1.4)
      : 1
    const label = `${latency != null ? latency.toFixed(1) + 'ms' : '--'} / ${bps(bw)}`
    edgeData.push({
      a: e.a,
      b: e.b,
      p0: sp0, c1: sc1, c2: sc2, p1: sp1, mid,
      tunnels,
      samples: m.samples || [],
      label,
      latency,
      loss: m.loss_pct,
      rx: m.rx_bps,
      tx: m.tx_bps,
      bw,
      color: '#94a3b8',
      network_name: e.network_name || '',
      active,
      lineWidth,
    })
  }

  edgeHitCache = edgeData
  const nodeData = nodes.map((n) => ({
    ...n,
    p: toScreen(layout[n.peer_id] || { x: width / 2, y: height / 2 }),
  }))

  const dark = document.querySelector('.v-application')?.classList.contains('v-theme--PurpleThemeDark')
  const axisLine = dark ? 'rgba(255,255,255,0.16)' : 'rgba(0,0,0,0.12)'
  return {
    animation: false,
    tooltip: {
      trigger: 'axis',
      confine: true,
      backgroundColor: dark ? 'rgba(36,36,36,0.96)' : 'rgba(255,255,255,0.98)',
      borderColor: dark ? 'rgba(255,255,255,0.14)' : 'rgba(0,0,0,0.1)',
      borderWidth: 1,
      padding: 0,
      textStyle: { color: dark ? '#e8eaed' : '#1f2a30', fontSize: 12 },
      extraCssText: 'border-radius:12px;box-shadow:0 10px 28px rgba(16,24,40,.16);padding:10px 12px;',
      axisPointer: { type: 'line', lineStyle: { color: axisLine } },
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
    series: [
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
                  lineDash: d.active ? [8, 6] : undefined,
                  lineDashOffset: d.active ? -flowOffset : 0,
                  opacity: .9,
                },
              },
              {
                type: 'text',
                style: (() => {
                  const dark = document.querySelector('.v-application')?.classList.contains('v-theme--PurpleThemeDark')
                  return {
                    text: d.label,
                    x: d.mid.x,
                    y: d.mid.y - 10 * Math.max(0.8, Math.min(1.6, view.scale)),
                    fill: dark ? '#e8eaed' : '#455a64',
                    fontSize: Math.max(9, 11 * Math.max(0.8, Math.min(1.6, view.scale))),
                    align: 'center',
                    backgroundColor: dark ? 'transparent' : 'rgba(255,255,255,0.92)',
                    padding: dark ? 0 : [2, 6],
                    borderRadius: dark ? 0 : 4,
                  }
                })(),
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
          const first = d.local
          const w = (first ? 170 : 150) * view.scale
          const h = (first ? 56 : 48) * view.scale
          const r = 12 * view.scale
          const dark = document.querySelector('.v-application')?.classList.contains('v-theme--PurpleThemeDark')
          return {
            type: 'group',
            children: [
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
              {
                type: 'text',
                style: {
                  text: d.hostname || String(d.peer_id),
                  x: d.p.x,
                  y: d.p.y,
                  fill: dark ? '#e8eaed' : '#1f2a30',
                  fontSize: Math.max(9, 12 * view.scale),
                  fontWeight: 500,
                  align: 'center',
                  verticalAlign: 'middle',
                },
              },
            ],
          }
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
  histLine.value = { visible: true, x: e.offsetX }
}

function onHistOut() {
  histLine.value.visible = false
}

function renderHistory() {
  if (!histEl.value) return
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
    axisPointer: { show: false, type: 'none' },
    grid: { left: 40, right: 40, top: 24, bottom: 22 },
    tooltip: {
      trigger: 'axis',
      confine: true,
      backgroundColor: dark ? 'rgba(36,36,36,0.96)' : 'rgba(255,255,255,0.98)',
      borderColor: dark ? 'rgba(255,255,255,0.14)' : 'rgba(0,0,0,0.1)',
      borderWidth: 1,
      padding: 10,
      textStyle: { color: dark ? '#e8eaed' : '#1f2a30', fontSize: 12 },
      extraCssText: 'border-radius:12px;box-shadow:0 10px 28px rgba(16,24,40,.16);',
      axisPointer: { show: false, type: 'none' },
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
  const points = (edgeSamples[keyOf(item.a, item.b)] || item.samples || []).slice(-60)
  historyPoints.value = points
  renderHistory()
}

watch(historyRange, (range) => {
  if (range === 'live') syncLive()
  else loadHistory()
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
        title: d.hostname || String(d.peer_id),
        rows: [
          { label: 'Peer ID', value: String(d.peer_id) },
          { label: 'IPv4', value: d.ipv4 || '-' },
          { label: '版本', value: d.version || '-' },
        ],
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
  if (!best) { hover.value.visible = false; return }
  const rect = chartEl.value.getBoundingClientRect()
  hover.value = {
    visible: true,
    x: rect.left + x,
    y: rect.top + y,
    title: `${nodeName(best.a)} ↔ ${nodeName(best.b)}`,
    rows: [
      { label: '延迟', value: best.latency != null ? `${best.latency.toFixed(2)} ms` : '-' },
      { label: '丢包', value: best.loss != null ? `${best.loss.toFixed(2)} %` : '-' },
      { label: '接收', value: bps(best.rx) },
      { label: '发送', value: bps(best.tx) },
      { label: '协议', value: [...new Set((best.tunnels || []).map((t) => t.tunnel_type))].join(', ') || '-' },
    ],
  }
}

function setupDrag() {
  const zr = chart.getZr()

  function endDrag() {
    drag = null
    pan = null
    hover.value.visible = false
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
      const { w, h } = nodeSize(drag.id)
      layout[drag.id] = {
        x: Math.max(w / 2 + 10, Math.min(width - w / 2 - 10, world.x - drag.dx)),
        y: Math.max(h / 2 + 10, Math.min(height - h / 2 - 10, world.y - drag.dy)),
      }
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
  graph = board.graph
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
    graph = data.graph || { nodes: [], edges: [] }
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
  const seriesOf = (params) => params.seriesId || (params.seriesIndex === 0 ? 'edges' : params.seriesIndex === 1 ? 'nodes' : null)
  chart.on('click', (params) => {
    const series = seriesOf(params)
    if (series === 'edges') openEdge(params.data)
    else if (series === 'nodes') openNode(params.data)
  })

  chartEl.value.addEventListener('mousemove', (e) => {
    const rect = chartEl.value.getBoundingClientRect()
    showHoverAt(e.clientX - rect.left, e.clientY - rect.top)
  })
  chartEl.value.addEventListener('mouseleave', () => { hover.value.visible = false })
  setupDrag()
  const ro = new ResizeObserver(() => resize())
  ro.observe(chartEl.value)
  resizeObserver = ro
  await load()
  connectBoardEvents()
  flowTimer = setInterval(() => {
    flowOffset = (flowOffset + 1.5) % 14
    if (chart) chart.setOption(buildOption(), true)
  }, 200)
  window.addEventListener('resize', resize)
})

onBeforeUnmount(() => {
  window.removeEventListener('resize', resize)
  if (resizeObserver) resizeObserver.disconnect()
  if (eventSource) eventSource.close()
  if (sseRetry) clearTimeout(sseRetry)
  stopPolling()
  if (flowTimer) clearInterval(flowTimer)
  if (ws) ws.close()
  if (chart) chart.dispose()
})
</script>

<style scoped>
.range-toggle {
  border: 1px solid rgba(var(--v-theme-on-surface), 0.14);
  border-radius: 8px;
  overflow: hidden;
  background: transparent;
}
.range-toggle :deep(.v-btn) {
  min-width: 46px;
  height: 28px;
  padding: 0 10px;
  font-size: 11px;
  letter-spacing: 0;
  text-transform: none;
  color: rgba(var(--v-theme-on-surface), 0.62);
  border-radius: 0 !important;
}
.range-toggle :deep(.v-btn.v-btn--active) {
  background: rgba(var(--v-theme-primary), 0.1) !important;
  color: rgb(var(--v-theme-primary)) !important;
}
.range-toggle :deep(.v-divider) { display: none; }
.hist-wrap { position: relative; width: 100%; height: 180px; }
.hist-pointer {
  position: absolute;
  top: 0;
  bottom: 0;
  width: 1px;
  background: rgba(var(--v-theme-on-surface), 0.22);
  pointer-events: none;
}
.tunnel-row { padding: 8px 0; }
.tunnel-row + .tunnel-row { border-top: 1px dashed var(--dok-border); }
.tunnel-row__top { display: flex; align-items: center; gap: 8px; }
.tunnel-row__type {
  flex: 0 0 auto; font-size: 10px; font-weight: 700; text-transform: uppercase; letter-spacing: .05em;
  color: rgb(var(--v-theme-primary)); background: rgba(60, 150, 202, .08);
  padding: 2px 6px; border-radius: 5px;
}
.tunnel-row__addr { flex: 1; min-width: 0; font-size: 12px; color: var(--dok-muted); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.tunnel-row__stats { display: flex; gap: 14px; margin-top: 6px; padding-left: 2px; font-size: 11px; color: var(--dok-subtle); }
</style>

<style scoped>
.topology-page { height: calc(100dvh - 40px - 6px - 16px); min-height: 560px; display: flex; flex-direction: column; }
.topology-card { flex: 1 1 auto; min-height: 0; overflow: hidden; }
.topology-chart { width: 100%; height: 100%; }
.node-grid { display: grid; grid-template-columns: 1fr 1fr; gap: 14px 18px; }
.node-value { margin-top: 3px; font-size: 12.5px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.neighbor-block { padding: 10px 0 4px; }
.neighbor-head { display: flex; align-items: baseline; gap: 8px; margin-bottom: 4px; }
.neighbor-name { font-size: 13px; font-weight: 600; }
.neighbor-ip { font-size: 11px; color: var(--dok-subtle); }
.neighbor-meta { font-size: 11px; color: var(--dok-muted); }
.tunnel-row--nested { margin-left: 8px; padding-left: 10px; border: 0; }
</style>
