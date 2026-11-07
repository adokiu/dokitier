<template>
  <div>
    <div class="d-flex align-center mb-4">
      <div class="page-title">{{ t('nav.links') }}</div>
      <v-spacer />
      <v-btn size="small" variant="text" prepend-icon="mdi-refresh" :loading="loading" @click="load">刷新</v-btn>
    </div>

    <v-card>
      <v-data-table :headers="headers" :items="rows" :loading="loading" item-value="id">
        <template #item.id="{ item }"><span class="mono">{{ names[item.a] || item.a }} ↔ {{ names[item.b] || item.b }}</span></template>
        <template #item.tunnel="{ item }">{{ item.tunnel_types?.join(', ') || '-' }}</template>
        <template #item.latency="{ item }">{{ fmt(item.latency_ms, 'ms') }}</template>
        <template #item.loss="{ item }">{{ fmt(item.loss_pct, '%') }}</template>
        <template #item.rx="{ item }">{{ fmtBps(item.rx_bps) }}</template>
        <template #item.tx="{ item }">{{ fmtBps(item.tx_bps) }}</template>
        <template #item.bandwidth="{ item }">{{ item.bandwidth_mbps ? item.bandwidth_mbps + ' Mbps' : '-' }}</template>
        <template #item.actions="{ item }">
          <v-btn v-if="!boardMode" size="small" variant="text" @click="edit(item)">编辑</v-btn>
          <span v-else class="text-caption text-medium-emphasis">通过节点配置修改</span>
        </template>
      </v-data-table>
    </v-card>

    <v-dialog v-model="dialog" max-width="560">
      <v-card v-if="form" class="pa-2">
        <v-card-title>链路 {{ form.a }} ↔ {{ form.b }}</v-card-title>
        <v-card-text>
          <v-text-field v-model.number="form.bandwidth_mbps" label="带宽 (Mbps)" type="number" />
          <v-text-field v-model="form.latency_weight" label="延迟权重（留空用全局）" type="number" />
          <v-text-field v-model="form.loss_weight" label="丢包权重（留空用全局）" type="number" />
          <v-text-field v-model="form.bandwidth_weight" label="带宽权重（留空用全局）" type="number" />
          <v-text-field v-model="form.note" label="备注" />
          <v-switch v-model="form.disabled" label="禁用此链路" color="error" />
        </v-card-text>
        <v-card-actions>
          <v-spacer />
          <v-btn variant="text" @click="dialog = false">取消</v-btn>
          <v-btn color="primary" :loading="saving" @click="save">保存</v-btn>
        </v-card-actions>
      </v-card>
    </v-dialog>
    <v-snackbar v-model="snack" :color="snackColor">{{ snackText }}</v-snackbar>
  </div>
</template>

<script setup>

import { onMounted, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { api } from '@/api'

const { t } = useI18n()

const headers = [
  { title: '链路', key: 'id' },
  { title: '协议', key: 'tunnel' },
  { title: '延迟', key: 'latency' },
  { title: '丢包', key: 'loss' },
  { title: '接收', key: 'rx' },
  { title: '发送', key: 'tx' },
  { title: '带宽', key: 'bandwidth' },
  { title: '操作', key: 'actions', sortable: false },
]
const rows = ref([])
const boardMode = ref(false)
const names = ref({})
const loading = ref(false)
const dialog = ref(false)
const saving = ref(false)
const form = ref(null)
const snack = ref(false)
const snackText = ref('')
const snackColor = ref('success')

const fmt = (v, unit) => (v == null ? '-' : `${Number(v).toFixed(2)} ${unit}`)
function fmtBps(v) {
  if (v == null) return '-'
  const units = ['B/s', 'KB/s', 'MB/s', 'GB/s']
  let n = Number(v)
  let i = 0
  while (n >= 1024 && i < units.length - 1) { n /= 1024; i++ }
  return `${n.toFixed(1)} ${units[i]}`
}

function edit(item) {
  form.value = {
    a: item.a,
    b: item.b,
    bandwidth_mbps: item.bandwidth_mbps,
    latency_weight: item.latency_weight,
    loss_weight: item.loss_weight,
    bandwidth_weight: item.bandwidth_weight,
    note: item.note || '',
    disabled: !!item.disabled,
  }
  dialog.value = true
}

async function save() {
  saving.value = true
  try {
    const f = form.value
    await api.updateLink(f.a, f.b, {
      bandwidth_mbps: f.bandwidth_mbps === '' ? null : f.bandwidth_mbps,
      latency_weight: f.latency_weight === '' ? null : f.latency_weight,
      loss_weight: f.loss_weight === '' ? null : f.loss_weight,
      bandwidth_weight: f.bandwidth_weight === '' ? null : f.bandwidth_weight,
      note: f.note,
      disabled: f.disabled,
    })
    snackText.value = '已保存'
    snackColor.value = 'success'
    snack.value = true
    dialog.value = false
    load()
  } catch (e) {
    snackText.value = String(e.message || e)
    snackColor.value = 'error'
    snack.value = true
  } finally {
    saving.value = false
  }
}

async function load() {
  loading.value = true
  try {
    try {
      const data = await api.boardLinks()
      boardMode.value = true
      rows.value = (data.links || []).map((l) => ({ ...l, id: `${l.a}-${l.b}` }))
      names.value = data.names || {}
    } catch {
      boardMode.value = false
      const data = await api.links()
      rows.value = (data.links || []).map((l) => ({ ...l.metrics, id: `${l.metrics.a_peer_id}-${l.metrics.b_peer_id}` }))
      names.value = Object.fromEntries((data.nodes || []).map((n) => [n.peer_id, n.hostname]))
    }
  } finally {
    loading.value = false
  }
}

onMounted(load)
</script>
