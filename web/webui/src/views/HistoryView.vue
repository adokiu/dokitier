<template>
  <div>
    <div class="d-flex align-center mb-4 flex-wrap gap-3">
      <div class="page-title">' + t('nav.history') + '</div>
      <v-spacer />
      <v-select v-model="selected" :items="linkOptions" item-title="label" item-value="value" density="compact" style="max-width: 360px" label="选择链路" />
      <v-btn-toggle v-model="range" mandatory density="compact" variant="outlined">
        <v-btn value="24h">24 小时</v-btn>
        <v-btn value="7d">7 天</v-btn>
      </v-btn-toggle>
    </div>

    <v-card class="pa-4 mb-4">
      <div class="page-title mb-2">延迟 / 丢包</div>
      <div ref="latencyEl" style="width: 100%; height: 300px"></div>
    </v-card>

    <v-card class="pa-4">
      <div class="page-title mb-2">带宽使用</div>
      <div ref="bwEl" style="width: 100%; height: 300px"></div>
    </v-card>
  </div>
</template>

<script setup>
const { t } = useI18n()

import { onBeforeUnmount, onMounted, ref, watch } from 'vue'
import * as echarts from 'echarts'
import { api } from '@/api'

const selected = ref(null)
const range = ref('24h')
const points = ref([])
const linkOptions = ref([])
const latencyEl = ref(null)
const bwEl = ref(null)
let latencyChart = null
let bwChart = null

function baseOption() {
  return {
    tooltip: { trigger: 'axis' },
    legend: { top: 0 },
    grid: { left: 48, right: 56, top: 36, bottom: 32 },
    xAxis: { type: 'time' },
    yAxis: { type: 'value' },
  }
}

function render() {
  if (!latencyChart || !bwChart) return
  const ts = points.value.map((p) => p.ts * 1000)
  const axis = ts.length ? { min: ts[0] - 300000, max: ts[ts.length - 1] + 300000 } : {}
  latencyChart.setOption({
    ...baseOption(),
    xAxis: { type: 'time', ...axis },
    yAxis: [
      { type: 'value', name: 'ms' },
      { type: 'value', name: '%', position: 'right' },
    ],
    series: [
      {
        name: '延迟 (ms)',
        type: 'line',
        smooth: 0.8,
        showSymbol: true,
        symbolSize: 6,
        data: ts.map((t, i) => [t, points.value[i].latency_ms]),
      },
      {
        name: '丢包 (%)',
        type: 'line',
        smooth: 0.8,
        showSymbol: true,
        symbolSize: 6,
        yAxisIndex: 1,
        data: ts.map((t, i) => [t, points.value[i].loss_pct]),
      },
    ],
  })
  bwChart.setOption({
    ...baseOption(),
    xAxis: { type: 'time', ...axis },
    yAxis: { type: 'value', name: 'Mbps' },
    series: [
      {
        name: '接收 (Mbps)',
        type: 'line',
        smooth: 0.8,
        showSymbol: true,
        symbolSize: 6,
        areaStyle: { opacity: 0.15 },
        data: ts.map((t, i) => [t, points.value[i].rx_bps ? (points.value[i].rx_bps * 8) / 1e6 : 0]),
      },
      {
        name: '发送 (Mbps)',
        type: 'line',
        smooth: 0.8,
        showSymbol: true,
        symbolSize: 6,
        areaStyle: { opacity: 0.15 },
        data: ts.map((t, i) => [t, points.value[i].tx_bps ? (points.value[i].tx_bps * 8) / 1e6 : 0]),
      },
    ],
  })
}

async function load() {
  if (!selected.value) return
  if (selected.value.includes('|')) {
    const [network, pair] = selected.value.split('|')
    const [a, b] = pair.split(':').map(Number)
    const data = await api.boardHistory(network, a, b, range.value)
    points.value = (data.points || []).map((p) => ({
      ts: p.ts, latency_ms: p.latency_ms, loss_pct: p.loss_pct, rx_bps: 0, tx_bps: 0,
    }))
    render()
    return
  }
  const [a, b] = selected.value.split(':').map(Number)
  const data = await api.history(a, b, range.value)
  points.value = data.points || []
  render()
}

watch([selected, range], load)

function resize() {
  latencyChart && latencyChart.resize()
  bwChart && bwChart.resize()
}

onMounted(async () => {
  latencyChart = echarts.init(latencyEl.value)
  bwChart = echarts.init(bwEl.value)
  render()
  try {
    const board = await api.boardLinks()
    if (board && (board.links || []).length) {
      const names = board.names || {}
      linkOptions.value = board.links.map((l) => ({
        label: `${l.network} / ${names[l.a] || l.a} ↔ ${names[l.b] || l.b}`,
        value: `${l.network}|${l.a}:${l.b}`,
      }))
      if (linkOptions.value.length) selected.value = linkOptions.value[0].value
      window.addEventListener('resize', resize)
      await load()
      return
    }
  } catch {}
  const data = await api.topology()
  const names = Object.fromEntries((data.graph?.nodes || []).map((n) => [n.peer_id, n.hostname || n.peer_id]))
  linkOptions.value = (data.graph?.edges || []).map((e) => ({
    label: `${names[e.a]} ↔ ${names[e.b]}`,
    value: `${e.a}:${e.b}`,
  }))
  if (linkOptions.value.length) selected.value = linkOptions.value[0].value
  window.addEventListener('resize', resize)
})

onBeforeUnmount(() => {
  window.removeEventListener('resize', resize)
  latencyChart && latencyChart.dispose()
  bwChart && bwChart.dispose()
})
</script>
