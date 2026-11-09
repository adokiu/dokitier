<template>
  <div class="dashboard-page">
    <div class="dash-head">
      <div>
        <div class="page-title">{{ t('nav.dashboard') }}</div>
        <div class="page-sub">全网实时转发、流量与节点健康</div>
      </div>
    </div>

    <div class="dash-cards">
      <v-card class="dash-stat">
        <div class="dash-stat__icon"><v-icon icon="mdi-swap-horizontal" size="20" /></div>
        <div class="stat-label">实时转发带宽</div>
        <div class="stat-value">{{ fmtBps(totalRx + totalTx) }}</div>
        <div class="dash-stat__delta">↑ {{ fmtBps(totalTx) }} / ↓ {{ fmtBps(totalRx) }}</div>
      </v-card>
      <v-card class="dash-stat">
        <div class="dash-stat__icon"><v-icon icon="mdi-chart-areaspline" size="20" /></div>
        <div class="stat-label">累计总流量</div>
        <div class="stat-value">{{ fmtBytes(totalTraffic) }}</div>
        <div class="dash-stat__delta">所有网络实例累计</div>
      </v-card>
      <v-card class="dash-stat">
        <div class="dash-stat__icon"><v-icon icon="mdi-lan" size="20" /></div>
        <div class="stat-label">网络数量</div>
        <div class="stat-value">{{ networks.length }}</div>
        <div class="dash-stat__delta">{{ p2pGroups }} 个 P2P 分组</div>
      </v-card>
      <v-card class="dash-stat">
        <div class="dash-stat__icon"><v-icon icon="mdi-server-network" size="20" /></div>
        <div class="stat-label">节点在线</div>
        <div class="stat-value">{{ online }}<span class="dash-stat__sub"> / {{ nodes.length }}</span></div>
        <div class="dash-stat__delta">离线 {{ nodes.length - online }}</div>
      </v-card>
    </div>

    <div class="dash-grid">
      <v-card class="dash-panel dash-span-8">
        <div class="dash-panel__head">
          <span>实时带宽</span>
          <v-btn-toggle v-model="bwRange" mandatory density="compact" variant="text" divided class="bw-toggle">
            <v-btn value="live" size="x-small">实时</v-btn>
            <v-btn value="1h" size="x-small">1h</v-btn>
            <v-btn value="24h" size="x-small">24h</v-btn>
            <v-btn value="7d" size="x-small">7d</v-btn>
            <v-btn value="30d" size="x-small">30d</v-btn>
          </v-btn-toggle>
        </div>
        <div ref="chartEl" class="dash-chart"></div>
      </v-card>

      <v-card class="dash-panel dash-span-4">
        <div class="dash-panel__head"><span>网络流量</span></div>
        <div class="dash-list">
          <div v-for="net in networkTraffic" :key="net.name" class="dash-list__row">
            <div class="dash-list__name">{{ net.name }}</div>
            <div class="dash-list__bar"><span :style="{ width: net.percent + '%' }"></span></div>
            <div class="dash-list__value">{{ fmtBytes(net.traffic) }}</div>
          </div>
          <div v-if="!networkTraffic.length" class="dash-empty">{{ dataReady ? '暂无网络' : '加载中…' }}</div>
        </div>
      </v-card>

      <v-card class="dash-panel dash-span-4">
        <div class="dash-panel__head"><span>链路质量</span></div>
        <div class="dash-quality">
          <div class="dash-quality__item"><div class="stat-label">平均延迟</div><div class="stat-value">{{ avgLatency }}</div></div>
          <div class="dash-quality__item"><div class="stat-label">平均丢包</div><div class="stat-value">{{ avgLoss }}</div></div>
          <div class="dash-quality__item"><div class="stat-label">链路总数</div><div class="stat-value">{{ linkCount }}</div></div>
          <div class="dash-quality__item"><div class="stat-label">实测链路</div><div class="stat-value">{{ measuredLinks }}</div></div>
        </div>
      </v-card>

      <v-card class="dash-panel dash-span-4">
        <div class="dash-panel__head"><span>节点负载 Top</span></div>
        <div class="dash-list">
          <div v-for="node in topNodes" :key="node.node_id" class="dash-list__row">
            <div class="dash-list__name">
              <img v-if="flagUrl(node.country_code)" class="node-flag" :src="flagUrl(node.country_code)" alt="" loading="lazy" />
              {{ node.alias || node.hostname }}
            </div>
            <div class="dash-list__bar dash-list__bar--cpu"><span :style="{ width: Math.min(100, node.cpu_pct || 0) + '%' }"></span></div>
            <div class="dash-list__value">{{ (node.cpu_pct || 0).toFixed(0) }}%</div>
          </div>
          <div v-if="!topNodes.length" class="dash-empty">{{ dataReady ? '暂无节点' : '加载中…' }}</div>
        </div>
      </v-card>

      <v-card class="dash-panel dash-span-4">
        <div class="dash-panel__head"><span>网络节点分布</span></div>
        <div class="dash-list">
          <div v-for="net in networkNodes" :key="net.name" class="dash-list__row">
            <div class="dash-list__name">{{ net.name }}</div>
            <div class="dash-list__bar"><span :style="{ width: Math.max(6, (net.count / Math.max(1, online)) * 100) + '%' }"></span></div>
            <div class="dash-list__value">{{ net.count }} 节点</div>
          </div>
          <div v-if="!networkNodes.length" class="dash-empty">{{ dataReady ? '暂无网络' : '加载中…' }}</div>
        </div>
      </v-card>

      <v-card class="dash-panel dash-span-8">
        <div class="dash-panel__head"><span>链路流量 Top</span><span class="dash-panel__hint">按实时收发排序</span></div>
        <div class="dash-links">
          <div class="dash-links__head">
            <span>链路</span><span>协议</span><span>延迟</span><span>丢包</span><span>容量</span><span>实时流量</span>
          </div>
          <div v-for="link in topLinks" :key="link.key" class="dash-links__row">
            <span class="dash-links__name">
              <img v-if="flagUrl(link.countryA)" class="node-flag" :src="flagUrl(link.countryA)" alt="" loading="lazy" />
              <span>{{ link.name }}</span>
            </span>
            <span class="dash-links__proto">
              <span v-if="link.protoLabel !== '-'" class="proto-badge" :class="'proto-badge--' + link.protoClass" :title="link.protoTitle">{{ link.protoLabel }}</span>
              <span v-else>-</span>
            </span>
            <span>{{ link.latency != null ? link.latency.toFixed(2) + ' ms' : '-' }}</span>
            <span>{{ link.loss != null ? link.loss.toFixed(2) + ' %' : '-' }}</span>
            <span>{{ link.capacity ? Math.round(link.capacity) + ' Mbps' : '未测量' }}</span>
            <span class="dash-links__bps">{{ fmtBps(link.bps) }}</span>
          </div>
          <div v-if="!topLinks.length" class="dash-empty">{{ dataReady ? '暂无链路' : '加载中…' }}</div>
        </div>
      </v-card>

      <v-card class="dash-panel dash-span-4">
        <div class="dash-panel__head"><span>节点地区分布</span></div>
        <div class="region-list">
          <div v-for="region in regionStats" :key="region.code" class="region-row">
            <img v-if="flagUrl(region.code)" class="region-flag" :src="flagUrl(region.code)" alt="" loading="lazy" />
            <span class="region-name" :title="region.label">{{ region.label }}</span>
            <span class="region-track"><span :style="{ width: region.percent + '%' }"></span></span>
            <span class="region-count">{{ region.count }}</span>
          </div>
          <div v-if="!regionStats.length" class="dash-empty">{{ dataReady ? '暂无地区数据' : '加载中…' }}</div>
        </div>
      </v-card>
    </div>
  </div>
</template>

<script setup>
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import * as echarts from 'echarts'
import { api } from '@/api'
import { flagUrl } from '@/utils/flags'
import {
  protocolClass,
  protocolLabel,
  protocolRaw,
} from '@/utils/protocol'

const { t } = useI18n()
const nodes = ref([])
const overview = ref(null)
const loading = ref(false)
const dataReady = ref(false)
const chartEl = ref(null)
const samples = ref([])
const bwRange = ref('live')
const rangeWindows = { live: 60_000, '1h': 3_600_000, '24h': 86_400_000, '7d': 7 * 86_400_000, '30d': 30 * 86_400_000 }
let chart = null
let timer = null
let eventSource = null
const graph = ref({ nodes: [], edges: [] })

const networks = computed(() => overview.value?.networks || [])
const online = computed(() => nodes.value.filter((node) => node.status === 'online').length)
const p2pGroups = computed(() => new Set(networks.value.flatMap((net) => net.p2p_groups || [])).size)
const totalRx = computed(() => nodes.value.reduce((sum, node) => sum + (Number(node.rx_bps) || 0), 0))
const totalTx = computed(() => nodes.value.reduce((sum, node) => sum + (Number(node.tx_bps) || 0), 0))
const totalTraffic = computed(() => nodes.value.reduce((sum, node) => {
  const traffic = node.traffic || {}
  return sum + Object.values(traffic).reduce((inner, value) => inner + (Number(value) || 0), 0)
}, 0))

const networkTraffic = computed(() => {
  const totals = {}
  for (const node of nodes.value) {
    for (const [name, value] of Object.entries(node.traffic || {})) {
      totals[name] = (totals[name] || 0) + (Number(value) || 0)
    }
  }
  const max = Math.max(1, ...Object.values(totals))
  return Object.entries(totals)
    .map(([name, traffic]) => ({ name, traffic, percent: Math.max(4, (traffic / max) * 100) }))
    .sort((a, b) => b.traffic - a.traffic)
})

const topNodes = computed(() => [...nodes.value]
  .filter((node) => node.status === 'online')
  .sort((a, b) => (b.cpu_pct || 0) - (a.cpu_pct || 0))
  .slice(0, 6))

const linkStats = computed(() => {
  const latencies = []
  const losses = []
  let count = 0
  let measured = 0
  for (const edge of graph.value.edges || []) {
    const tunnel = (edge.tunnels || [])[0]
    if (!tunnel) continue
    count += 1
    if (tunnel.latency_ms != null) latencies.push(Number(tunnel.latency_ms))
    if (tunnel.loss_pct != null) losses.push(Number(tunnel.loss_pct))
    if (tunnel.capacity_source === 'probe') measured += 1
  }
  return { count, measured, latencies, losses }
})
const linkCount = computed(() => linkStats.value.count)
const measuredLinks = computed(() => linkStats.value.measured)
const avgLatency = computed(() => {
  const values = linkStats.value.latencies
  if (!values.length) return '-'
  return `${(values.reduce((sum, value) => sum + value, 0) / values.length).toFixed(2)} ms`
})
const avgLoss = computed(() => {
  const values = linkStats.value.losses
  if (!values.length) return '-'
  return `${(values.reduce((sum, value) => sum + value, 0) / values.length).toFixed(2)} %`
})

const topLinks = computed(() => (graph.value.edges || [])
  .map((edge) => {
    const tunnel = (edge.tunnels || [])[0] || {}
    const nameA = graph.value.nodes.find((n) => n.peer_id === edge.a)?.alias
      || graph.value.nodes.find((n) => n.peer_id === edge.a)?.hostname || edge.a
    const nameB = graph.value.nodes.find((n) => n.peer_id === edge.b)?.alias
      || graph.value.nodes.find((n) => n.peer_id === edge.b)?.hostname || edge.b
    const nodeA = graph.value.nodes.find((n) => n.peer_id === edge.a)
    const nodeB = graph.value.nodes.find((n) => n.peer_id === edge.b)
    const tunnelTypes = [...new Set((edge.tunnels || []).map((item) => item.type).filter(Boolean))]
    const protocols = tunnelTypes.length ? tunnelTypes : (edge.tunnel_types || [])
    return {
      key: `${edge.a}-${edge.b}-${edge.network_name}`,
      name: `${nameA} ↔ ${nameB}`,
      protoLabel: protocolLabel(protocols),
      protoClass: protocolClass(protocols),
      protoTitle: protocolRaw(protocols),
      countryA: nodeA?.country_code || '',
      countryB: nodeB?.country_code || '',
      latency: tunnel.latency_ms,
      loss: tunnel.loss_pct,
      capacity: tunnel.capacity_mbps,
      bps: (Number(tunnel.rx_bps) || 0) + (Number(tunnel.tx_bps) || 0),
    }
  })
  .sort((a, b) => b.bps - a.bps)
  .slice(0, 6))

const regionStats = computed(() => {
  const counts = {}
  for (const node of nodes.value) {
    const code = (node.country_code || 'unknown').toLowerCase()
    counts[code] = (counts[code] || 0) + 1
  }
  const display = (() => {
    try { return new Intl.DisplayNames(['zh'], { type: 'region' }) } catch { return null }
  })()
  const max = Math.max(1, ...Object.values(counts))
  return Object.entries(counts)
    .map(([code, count]) => ({
      code,
      count,
      label: code === 'unknown' ? '未知' : (display?.of(code.toUpperCase()) || code.toUpperCase()),
      percent: Math.max(8, (count / max) * 100),
    }))
    .sort((a, b) => b.count - a.count)
    .slice(0, 8)
})

const networkNodes = computed(() => {
  const counts = {}
  for (const node of nodes.value) {
    for (const network of node.networks || []) {
      counts[network.name] = (counts[network.name] || 0) + 1
    }
  }
  return Object.entries(counts)
    .map(([name, count]) => ({ name, count }))
    .sort((a, b) => b.count - a.count)
})

function fmtBytes(value) {
  const units = ['B', 'KB', 'MB', 'GB', 'TB', 'PB']
  let size = Number(value) || 0
  let index = 0
  while (size >= 1024 && index < units.length - 1) {
    size /= 1024
    index += 1
  }
  return `${size.toFixed(index === 0 ? 0 : 1)} ${units[index]}`
}
function fmtBps(value) {
  return `${fmtBytes(value)}/s`
}

function renderChart() {
  if (!chartEl.value) return
  if (!chart) chart = echarts.init(chartEl.value, null, { renderer: 'canvas' })
  const now = Date.now()
  const min = now - (rangeWindows[bwRange.value] || 60_000)
  const axis = 'rgba(127,127,127,0.18)'
  chart.setOption({
    animation: false,
    grid: { left: 48, right: 16, top: 22, bottom: 26 },
    tooltip: {
      trigger: 'axis',
      appendTo: 'body',
      backgroundColor: 'rgba(30,30,30,0.94)',
      borderWidth: 0,
      textStyle: { color: '#eee', fontSize: 12 },
      formatter: (params) => params.map((p) => `${p.marker} ${p.seriesName} ${fmtBps(p.value[1])}`).join('<br/>'),
    },
    legend: { show: true, top: 0, right: 8, icon: 'roundRect', itemWidth: 10, itemHeight: 10, textStyle: { fontSize: 11 } },
    xAxis: { type: 'time', min, max: now, axisLine: { lineStyle: { color: axis } }, axisLabel: { fontSize: 9, color: 'rgba(127,127,127,0.9)' }, splitLine: { show: false } },
    yAxis: { type: 'value', axisLine: { show: false }, axisLabel: { fontSize: 9, color: 'rgba(127,127,127,0.9)', formatter: (value) => fmtBytes(value) }, splitLine: { lineStyle: { color: 'rgba(127,127,127,0.08)' } } },
    series: [
      { name: '发送', type: 'line', smooth: 0.6, showSymbol: false, data: samples.value.map((s) => [s.ts, s.tx]), lineStyle: { width: 1.6, color: '#3c96ca' }, areaStyle: { color: 'rgba(60,150,202,0.12)' } },
      { name: '接收', type: 'line', smooth: 0.6, showSymbol: false, data: samples.value.map((s) => [s.ts, s.rx]), lineStyle: { width: 1.6, color: '#4cc38a' }, areaStyle: { color: 'rgba(76,195,138,0.12)' } },
    ],
  }, { notMerge: false, replaceMerge: ['series'] })
  chart.resize()
}

async function load() {
  loading.value = true
  try {
    const [board, nodeData, topo] = await Promise.all([
      api.boardOverview(),
      api.boardNodes(),
      api.boardTopology().catch(() => null),
    ])
    overview.value = board
    nodes.value = nodeData.nodes || []
    if (topo?.graph) graph.value = topo.graph
    if (!samples.value.length) applyNodeSample()
    renderChart()
    dataReady.value = true
    loadBandwidthHistory(bwRange.value)
  } catch {
    // keep last data on transient failures
  } finally {
    loading.value = false
  }
}

async function loadBandwidthHistory(range) {
  try {
    const history = await api.boardBandwidthHistory(range === 'live' ? '60s' : range)
    const points = (history.points || []).map((point) => ({
      ts: Number(point.ts) * 1000,
      rx: Number(point.rx_bps) || 0,
      tx: Number(point.tx_bps) || 0,
    }))
    if (!points.length) return
    const merged = new Map()
    for (const point of [...points, ...samples.value]) merged.set(point.ts, point)
    samples.value = [...merged.values()].sort((a, b) => a.ts - b.ts)
    renderChart()
  } catch {}
}

function applyNodeSample() {
  samples.value.push({ ts: Date.now(), rx: totalRx.value, tx: totalTx.value })
  if (samples.value.length > 4000) samples.value.splice(0, samples.value.length - 4000)
  renderChart()
}

function connectEvents() {
  if (typeof EventSource === 'undefined') {
    timer = setInterval(load, 3000)
    return
  }
  eventSource = new EventSource('/api/board/events')
  eventSource.addEventListener('nodes', (event) => {
    try {
      const data = JSON.parse(event.data)
      if (Array.isArray(data.nodes)) {
        nodes.value = data.nodes
        applyNodeSample()
      }
    } catch {}
  })
  eventSource.addEventListener('topology', (event) => {
    try {
      const data = JSON.parse(event.data)
      if (data.graph) graph.value = data.graph
    } catch {}
  })
  eventSource.onerror = () => {
    eventSource?.close()
    eventSource = null
    if (!timer) timer = setInterval(load, 3000)
    setTimeout(connectEvents, 5000)
  }
}

watch(bwRange, async (range) => {
  await loadBandwidthHistory(range)
  renderChart()
})

onMounted(async () => {
  await load()
  connectEvents()
})
onBeforeUnmount(() => {
  if (eventSource) eventSource.close()
  if (timer) clearInterval(timer)
  chart?.dispose()
  chart = null
})
</script>

<style scoped>
.dashboard-page { width: 100%; display: flex; flex-direction: column; gap: 14px; }
.dash-head { display: flex; align-items: flex-start; justify-content: space-between; gap: 12px; }
.dash-cards { display: grid; grid-template-columns: repeat(4, minmax(0, 1fr)); gap: 12px; }
.dash-stat { padding: 16px 18px; position: relative; overflow: hidden; min-height: 100px; }
.dash-stat__icon { position: absolute; top: 14px; right: 14px; display: grid; place-items: center; width: 32px; height: 32px; border-radius: 9px; color: rgb(var(--v-theme-primary)); background: rgba(var(--v-theme-primary), .08); }
.dash-stat__sub { font-size: 15px; font-weight: 500; color: rgba(var(--v-theme-on-surface), .5); }
.dash-stat__delta { margin-top: 6px; font-size: 12px; color: rgba(var(--v-theme-on-surface), .55); }
.dash-grid { display: grid; grid-template-columns: repeat(12, minmax(0, 1fr)); gap: 12px; align-items: stretch; }
.dash-span-8 { grid-column: span 8; }
.dash-span-4 { grid-column: span 4; }
.dash-panel { padding: 16px 18px; min-width: 0; min-height: 0; display: flex; flex-direction: column; }
.dash-panel__head { display: flex; align-items: center; justify-content: space-between; gap: 10px; font-size: 14px; font-weight: 600; margin-bottom: 12px; }
.dash-panel__hint { font-size: 12px; font-weight: 500; color: rgba(var(--v-theme-on-surface), .5); }
.bw-toggle { border: 1px solid rgba(var(--v-theme-on-surface), .12); border-radius: 8px; overflow: hidden; }
.bw-toggle :deep(.v-btn) { text-transform: none; letter-spacing: 0; min-width: 40px; height: 26px; font-size: 11px; }
.dash-chart { width: 100%; height: 250px; min-height: 250px; }
.dash-list { display: flex; flex-direction: column; }
.dash-list__row { display: grid; grid-template-columns: minmax(0, 1fr) 72px 72px; align-items: center; gap: 10px; padding: 7px 0; border-bottom: 1px solid rgba(var(--v-theme-on-surface), .06); }
.dash-list__row:last-child { border-bottom: 0; }
.node-flag { width: 16px; height: 12px; border-radius: 2px; object-fit: cover; vertical-align: -1px; margin-right: 6px; }
.dash-list__name { font-size: 13px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.dash-list__bar { height: 5px; border-radius: 999px; background: rgba(var(--v-theme-on-surface), .1); overflow: hidden; }
.dash-list__bar span { display: block; height: 100%; border-radius: 999px; background: rgb(var(--v-theme-primary)); }
.dash-list__bar--cpu span { background: #4cc38a; }
.dash-list__value { font-size: 12px; text-align: right; font-variant-numeric: tabular-nums; white-space: nowrap; }
.dash-empty { font-size: 12px; color: rgba(var(--v-theme-on-surface), .5); padding: 10px 0; }
.region-list { display: flex; flex-direction: column; gap: 2px; }
.region-row { display: grid; grid-template-columns: 16px 84px minmax(0, 1fr) 32px; align-items: center; gap: 8px; padding: 6px 0; border-bottom: 1px solid rgba(var(--v-theme-on-surface), .06); }
.region-row:last-child { border-bottom: 0; }
.region-flag { width: 16px; height: 12px; border-radius: 2px; object-fit: cover; }
.region-name { font-size: 12px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.region-track { height: 8px; border-radius: 999px; background: rgba(var(--v-theme-on-surface), .08); overflow: hidden; }
.region-track span { display: block; height: 100%; border-radius: 999px; background: linear-gradient(90deg, #4cc38a, #3c96ca); }
.region-count { font-size: 12px; text-align: right; font-variant-numeric: tabular-nums; color: rgba(var(--v-theme-on-surface), .7); }
.dash-quality { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 14px 18px; }
.dash-quality__item .stat-value { font-size: 20px; }
.dash-links { display: flex; flex-direction: column; min-width: 0; }
.dash-links__head, .dash-links__row { display: grid; grid-template-columns: minmax(0, 1.8fr) 76px 70px 60px 90px minmax(80px, 1fr); gap: 8px; align-items: center; }
.dash-links__head { padding: 0 0 8px; font-size: 11.5px; color: rgba(var(--v-theme-on-surface), .5); border-bottom: 1px solid rgba(var(--v-theme-on-surface), .08); }
.dash-links__row { padding: 8px 0; font-size: 12px; border-bottom: 1px solid rgba(var(--v-theme-on-surface), .06); font-variant-numeric: tabular-nums; }
.dash-links__row:last-child { border-bottom: 0; }
.dash-links__name { font-size: 13px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.dash-links__bps { text-align: right; }
@media (max-width: 1100px) {
  .dash-cards { grid-template-columns: repeat(2, minmax(0, 1fr)); }
  .dash-span-8, .dash-span-4 { grid-column: span 12; }
}
@media (max-width: 640px) {
  .dash-cards { grid-template-columns: 1fr; }
  .dash-links__head, .dash-links__row { grid-template-columns: minmax(0, 1.6fr) 64px 60px 55px 80px 80px; }
}
</style>
