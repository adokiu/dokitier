<template>
  <div class="routes-page">
    <div class="routes-head">
      <div>
        <div class="page-title">路由</div>
        <div class="page-sub">当前网络实例计算出的 OSPF 路由与静态路由</div>
      </div>
      <div class="routes-actions">
        <v-select v-model="networkFilter" :items="networkOptions" density="compact" variant="outlined" hide-details class="routes-filter" />
        <v-btn v-if="!boardMode" color="primary" size="small" prepend-icon="mdi-plus" @click="openAdd">新增静态路由</v-btn>
      </div>
    </div>

    <div class="routes-cards">
      <v-card class="routes-stat">
        <div class="stat-label">路由总数</div>
        <div class="stat-value">{{ filteredRows.length }}</div>
      </v-card>
      <v-card class="routes-stat">
        <div class="stat-label">网络数</div>
        <div class="stat-value">{{ networkOptions.length - 1 }}</div>
      </v-card>
      <v-card class="routes-stat">
        <div class="stat-label">平均成本</div>
        <div class="stat-value">{{ averageCost }}</div>
      </v-card>
      <v-card class="routes-stat">
        <div class="stat-label">最高成本</div>
        <div class="stat-value">{{ maxCost }}</div>
      </v-card>
    </div>

    <v-card class="routes-table-card">
      <DokTable
        :headers="ospfHeaders"
        :items="filteredRows"
        :loading="loading"
        item-value="peer_id"
        :items-per-page="20"
        hover
      >
        <template #item.network="{ item }"><span class="net-chip">{{ item.network || '-' }}</span></template>
        <template #item.hostname="{ item }"><span class="host-cell">{{ item.hostname || '-' }}</span></template>
        <template #item.ipv4="{ item }"><span class="num">{{ item.ipv4 || '-' }}</span></template>
        <template #item.next_hop_peer_id="{ item }">
          <div class="nexthop-stack">
            <span>{{ nextHopName(item.next_hop_peer_id) }}</span>
            <span class="peer-id">{{ item.next_hop_peer_id }}</span>
          </div>
        </template>
        <template #item.cost="{ item }">
          <div class="cost-cell">
            <span class="cost-bar"><span class="cost-bar__fill" :style="{ width: costBarWidth(item.cost) }"></span></span>
            <span class="cost-value">{{ item.cost ?? '-' }}</span>
          </div>
        </template>
        <template #item.path_latency="{ item }">
          <span class="num">{{ item.path_latency != null ? item.path_latency.toFixed(2) + ' ms' : '-' }}</span>
        </template>
        <template #item.status><span class="route-ok">可达</span></template>
        <template #no-data><div class="routes-empty">暂无路由数据</div></template>
      </DokTable>
    </v-card>

    <v-card v-if="!boardMode" class="routes-table-card mt-4">
      <div class="routes-table-head"><span>静态路由</span><span class="routes-table-count">{{ staticRoutes.length }} 条</span></div>
      <DokTable :headers="staticHeaders" :items="staticRoutes" item-value="id" :items-per-page="20" hover>
        <template #item.dest_cidr="{ item }"><span class="num">{{ item.dest_cidr }}</span></template>
        <template #item.next_hop_peer_id="{ item }"><span class="num">{{ item.next_hop_peer_id }}</span></template>
        <template #item.metric="{ item }"><span class="num">{{ item.metric }}</span></template>
        <template #item.enabled="{ item }"><v-chip size="x-small" :color="item.enabled ? 'success' : 'default'">{{ item.enabled ? '启用' : '停用' }}</v-chip></template>
        <template #item.note="{ item }">{{ item.note || '-' }}</template>
        <template #item.actions="{ item }"><v-btn size="x-small" variant="text" color="error" @click="remove(item)">删除</v-btn></template>
        <template #no-data><div class="routes-empty">暂无静态路由</div></template>
      </DokTable>
    </v-card>

    <v-dialog v-model="dialog" max-width="520">
      <v-card class="pa-2">
        <v-card-title>新增静态路由</v-card-title>
        <v-card-text>
          <v-text-field v-model="form.dest_cidr" label="目标网段" placeholder="10.1.0.0/16" />
          <v-text-field v-model.number="form.next_hop_peer_id" label="下一跳 Peer ID" type="number" />
          <v-text-field v-model.number="form.metric" label="Metric" type="number" />
          <v-text-field v-model="form.note" label="备注" />
        </v-card-text>
        <v-card-actions>
          <v-spacer />
          <v-btn variant="text" @click="dialog = false">取消</v-btn>
          <v-btn color="primary" @click="save">保存</v-btn>
        </v-card-actions>
      </v-card>
    </v-dialog>
  </div>
</template>

<script setup>
import { computed, onBeforeUnmount, onMounted, ref } from 'vue'
import { api } from '@/api'
import DokTable from '@/components/DokTable.vue'

const boardMode = ref(false)
const loading = ref(false)
const rows = ref([])
const staticRoutes = ref([])
const networkFilter = ref('全部网络')
const dialog = ref(false)
const form = ref({ dest_cidr: '', next_hop_peer_id: null, metric: 100, note: '' })

const ospfHeaders = [
  { title: '网络', key: 'network' },
  { title: '主机名', key: 'hostname' },
  { title: 'IPv4', key: 'ipv4' },
  { title: '下一跳', key: 'next_hop_peer_id' },
  { title: '成本', key: 'cost' },
  { title: '延迟', key: 'path_latency' },
  { title: '状态', key: 'status', sortable: false },
]
const staticHeaders = [
  { title: '目标网段', key: 'dest_cidr' },
  { title: '下一跳', key: 'next_hop_peer_id' },
  { title: 'Metric', key: 'metric' },
  { title: '状态', key: 'enabled' },
  { title: '备注', key: 'note' },
  { title: '操作', key: 'actions', sortable: false },
]

const networkOptions = computed(() => ['全部网络', ...new Set(rows.value.map((row) => row.network).filter(Boolean))])
const filteredRows = computed(() => {
  if (networkFilter.value === '全部网络') return rows.value
  return rows.value.filter((row) => row.network === networkFilter.value)
})
const averageCost = computed(() => {
  const costs = filteredRows.value.map((row) => Number(row.cost)).filter((value) => Number.isFinite(value))
  if (!costs.length) return '-'
  return (costs.reduce((sum, value) => sum + value, 0) / costs.length).toFixed(1)
})
const maxCost = computed(() => {
  const costs = filteredRows.value.map((row) => Number(row.cost)).filter((value) => Number.isFinite(value))
  return costs.length ? Math.max(...costs) : '-'
})

function nextHopName(peerId) {
  const match = rows.value.find((row) => row.peer_id === peerId)
  return match?.hostname || String(peerId)
}

function costBarWidth(cost) {
  const max = maxCost.value === '-' ? 1 : Number(maxCost.value)
  const value = Number(cost)
  if (!Number.isFinite(value) || !max) return '0%'
  return `${Math.max(6, (value / max) * 100)}%`
}

async function load() {
  loading.value = true
  try {
    const board = await api.boardRoutes()
    if (board && Array.isArray(board.routes) && board.routes.length) {
      boardMode.value = true
      rows.value = board.routes
      staticRoutes.value = []
      return
    }
    boardMode.value = false
    const data = await api.routes()
    staticRoutes.value = data.static || []
    rows.value = (data.ospf?.instances || []).flatMap((instance) => instance.routes || [])
  } catch {
    // keep the previous rows on transient errors
  } finally {
    loading.value = false
  }
}

function openAdd() {
  form.value = { dest_cidr: '', next_hop_peer_id: null, metric: 100, note: '' }
  dialog.value = true
}

async function save() {
  await api.createStaticRoute({
    dest_cidr: form.value.dest_cidr,
    next_hop_peer_id: Number(form.value.next_hop_peer_id),
    metric: Number(form.value.metric) || 100,
    note: form.value.note,
  })
  dialog.value = false
  load()
}

async function remove(item) {
  await api.deleteStaticRoute(item.id)
  load()
}

let timer = null
onMounted(() => { load(); timer = setInterval(load, 10000) })
onBeforeUnmount(() => { if (timer) clearInterval(timer) })
</script>

<style scoped>
.routes-page { width: 100%; }
.routes-head { display: flex; align-items: flex-start; justify-content: space-between; gap: 16px; margin-bottom: 18px; flex-wrap: wrap; }
.routes-actions { display: flex; align-items: center; gap: 10px; flex-wrap: wrap; }
.routes-filter { width: 190px; }
.routes-cards { display: grid; grid-template-columns: repeat(4, minmax(0, 1fr)); gap: 12px; margin-bottom: 16px; }
.routes-stat { padding: 16px 18px; }
.routes-table-card { overflow: hidden; }
.routes-table-head { display: flex; align-items: center; justify-content: space-between; padding: 14px 18px; border-bottom: 1px solid rgba(var(--v-theme-on-surface), .08); font-size: 14px; font-weight: 600; }
.routes-table-count { font-size: 12px; font-weight: 500; color: rgba(var(--v-theme-on-surface), .55); }
.num { font-variant-numeric: tabular-nums; }
.routes-table-card :deep(.v-data-table-footer) { border-top: 1px solid rgba(var(--v-theme-on-surface), .08); }
.net-chip { display: inline-flex; align-items: center; padding: 2px 8px; border-radius: 6px; font-size: 11.5px; font-weight: 600; color: rgb(var(--v-theme-primary)); background: rgba(var(--v-theme-primary), .08); }
.host-cell { font-weight: 600; }
.nexthop-stack { display: flex; flex-direction: column; gap: 1px; }
.peer-id { font-size: 11px; color: rgba(var(--v-theme-on-surface), .5); }
.cost-cell { min-width: 150px; }
.cost-bar { display: inline-block; width: 72px; height: 5px; margin-right: 10px; border-radius: 999px; background: rgba(var(--v-theme-on-surface), .1); overflow: hidden; vertical-align: middle; }
.cost-bar__fill { display: block; height: 100%; border-radius: 999px; background: rgb(var(--v-theme-primary)); }
.cost-value { font-weight: 600; }
.route-ok { display: inline-flex; align-items: center; gap: 6px; font-size: 12px; color: rgb(var(--v-theme-success)); }
.route-ok::before { width: 6px; height: 6px; border-radius: 50%; background: currentColor; content: ""; }
.routes-empty { text-align: center; color: rgba(var(--v-theme-on-surface), .5); padding: 24px 0; font-size: 13px; }
@media (max-width: 900px) { .routes-cards { grid-template-columns: repeat(2, minmax(0, 1fr)); } }
</style>
