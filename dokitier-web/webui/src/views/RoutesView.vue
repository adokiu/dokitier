<template>
  <div>
    <div class="d-flex align-center mb-4">
      <div class="page-title">路由</div>
      <v-spacer />
      <v-btn v-if="!boardMode" color="primary" size="small" prepend-icon="mdi-plus" @click="openAdd">新增静态路由</v-btn>
    </div>

    <v-card class="pa-1">
      <v-card-title v-if="!boardMode">静态路由</v-card-title>
      <v-data-table :headers="staticHeaders" :items="staticRoutes" item-value="id" density="comfortable">
        <template #item.dest_cidr="{ item }"><span class="mono">{{ item.dest_cidr }}</span></template>
        <template #item.enabled="{ item }"><v-chip size="x-small" :color="item.enabled ? 'success' : 'default'">{{ item.enabled ? '启用' : '停用' }}</v-chip></template>
        <template #item.actions="{ item }">
          <v-btn size="small" variant="text" color="error" @click="remove(item)">删除</v-btn>
        </template>
      </v-data-table>
    </v-card>

    <v-card class="pa-1 mt-4">
      <v-card-title>OSPF 路由</v-card-title>
      <v-data-table :headers="ospfHeaders" :items="ospfRows" density="compact">
        <template #item.ipv4="{ item }"><span class="mono">{{ item.ipv4 || '-' }}</span></template>
      </v-data-table>
    </v-card>

    <v-dialog v-model="dialog" max-width="520">
      <v-card class="pa-2">
        <v-card-title>新增静态路由</v-card-title>
        <v-card-text>
          <v-text-field v-model="form.dest_cidr" label="目标网段 (例如 10.1.0.0/16)" />
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
import { onMounted, ref } from 'vue'
import { api } from '@/api'

const staticHeaders = [
  { title: '目标', key: 'dest_cidr' },
  { title: '下一跳', key: 'next_hop_peer_id' },
  { title: 'Metric', key: 'metric' },
  { title: '状态', key: 'enabled' },
  { title: '备注', key: 'note' },
  { title: '操作', key: 'actions', sortable: false },
]
const ospfHeaders = [
  { title: 'Peer ID', key: 'peer_id' },
  { title: '主机名', key: 'hostname' },
  { title: '下一跳', key: 'next_hop_peer_id' },
  { title: '成本', key: 'cost' },
  { title: '延迟', key: 'path_latency' },
  { title: 'IPv4', key: 'ipv4' },
]

const boardMode = ref(false)
const staticRoutes = ref([])
const ospfRows = ref([])
const dialog = ref(false)
const form = ref({ dest_cidr: '', next_hop_peer_id: null, metric: 100, note: '' })

async function load() {
  try {
    const board = await api.boardRoutes()
    if (board && Array.isArray(board.routes) && board.routes.length) {
      boardMode.value = true
      staticRoutes.value = []
      ospfRows.value = board.routes
      return
    }
  } catch {}
  boardMode.value = false
  const data = await api.routes()
  staticRoutes.value = data.static || []
  ospfRows.value = (data.ospf?.instances || []).flatMap((i) => i.routes || [])
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

onMounted(load)
</script>
