<template>
  <div>
    <div class="page-title mb-4">全局路由权重</div>
    <v-row>
      <v-col cols="12" md="6">
        <v-card class="pa-6">
          <v-text-field v-model.number="form.latency_weight" label="延迟权重" type="number" />
          <v-text-field v-model.number="form.loss_weight" label="丢包权重" type="number" />
          <v-text-field v-model.number="form.bandwidth_weight" label="带宽权重" type="number" />
          <v-text-field v-model.number="form.reference_bandwidth_mbps" label="参考带宽 (Mbps)" type="number" />
          <v-select v-model="form.mode" :items="modes" label="选路模式" />
          <v-alert type="info" variant="tonal" density="compact" class="mb-4">
            成本 = 延迟×延迟权重 + 丢包率×丢包权重 + (参考带宽/链路带宽)×带宽权重
          </v-alert>
          <v-btn color="primary" :loading="saving" @click="save">保存</v-btn>
        </v-card>
      </v-col>
      <v-col cols="12" md="6">
        <v-card class="pa-6">
          <div class="page-title mb-3">同步状态</div>
          <v-row class="mb-2">
            <v-col cols="6">
              <div class="stat-label">同步记录</div>
              <div class="stat-value">{{ summary.records ?? '-' }}</div>
            </v-col>
            <v-col cols="6">
              <div class="stat-label">最近更新</div>
              <div class="stat-value" style="font-size: 16px">{{ lastUpdated }}</div>
            </v-col>
          </v-row>
          <div class="stat-label mb-2">同步对端</div>
          <v-table density="compact" class="mb-3">
            <thead><tr><th>对端</th><th>状态</th><th>失败</th><th>冲突</th><th>最后成功</th><th>错误</th></tr></thead>
            <tbody>
              <tr v-for="peer in peers" :key="peer.peer">
                <td class="mono" style="font-size: 12px">{{ peer.peer }}</td>
                <td>
                  <v-chip size="x-small" :color="peer.ok ? 'success' : 'error'">
                    {{ peer.ok ? '正常' : '失败' }}
                  </v-chip>
                </td>
                <td>{{ peer.failures || 0 }}</td>
                <td :class="peer.conflicts ? 'text-warning' : ''">{{ peer.conflicts || 0 }}</td>
                <td>{{ peer.last_success ? new Date(peer.last_success * 1000).toLocaleTimeString() : '-' }}</td>
                <td class="text-error" style="font-size: 12px">{{ peer.last_error || '' }}</td>
              </tr>
            </tbody>
          </v-table>
          <v-table density="compact" height="220">
            <thead><tr><th>键</th><th>类型</th><th>来源</th></tr></thead>
            <tbody>
              <tr v-for="item in records" :key="item.key">
                <td class="mono" style="font-size: 12px">{{ item.key }}</td>
                <td>{{ item.namespace }}</td>
                <td>{{ item.origin }}</td>
              </tr>
            </tbody>
          </v-table>
        </v-card>
      </v-col>
    </v-row>
    <v-snackbar v-model="snack" :color="snackColor">{{ snackText }}</v-snackbar>
  </div>
</template>

<script setup>
import { computed, onMounted, ref } from 'vue'
import { api } from '@/api'

const form = ref({
  latency_weight: 1,
  loss_weight: 20,
  bandwidth_weight: 0.5,
  reference_bandwidth_mbps: 100,
  mode: 'weighted',
})
const modes = [
  { title: '加权成本（延迟/丢包/带宽）', value: 'weighted' },
  { title: '最少跳数（权重只做平局比较）', value: 'least_hop' },
]
const saving = ref(false)
const snack = ref(false)
const snackText = ref('')
const snackColor = ref('success')
const summary = ref({})
const records = ref([])
const peers = ref([])
const lastUpdated = computed(() => {
  const ts = summary.value.last_updated
  return ts ? new Date(ts * 1000).toLocaleString() : '-'
})

onMounted(async () => {
  const s = await api.settings()
  for (const key of Object.keys(form.value)) {
    if (s[`routing.${key}`] != null) {
      form.value[key] = key === 'mode' ? s[`routing.${key}`] : Number(s[`routing.${key}`])
    }
  }
  try {
    const data = await api.sync()
    summary.value = data.summary || {}
    records.value = data.local?.keys || []
    peers.value = data.runtime?.peers || []
  } catch {
    summary.value = {}
  }
})

async function save() {
  saving.value = true
  try {
    const payload = {}
    for (const [k, v] of Object.entries(form.value)) payload[`routing.${k}`] = String(v)
    await api.saveSettings(payload)
    snackText.value = '已保存并同步'
    snackColor.value = 'success'
    snack.value = true
  } catch (e) {
    snackText.value = String(e.message || e)
    snackColor.value = 'error'
    snack.value = true
  } finally { saving.value = false }
}
</script>
