<template>
  <div>
    <div class="d-flex align-center mb-4">
      <div class="page-title">概览</div>
      <v-spacer />
      <v-btn size="small" variant="text" prepend-icon="mdi-refresh" :loading="loading" @click="load">刷新</v-btn>
    </div>

    <v-row>
      <v-col v-for="card in cards" :key="card.label" cols="12" sm="6" md="3">
        <v-card class="pa-5">
          <div class="d-flex align-center justify-space-between">
            <div>
              <div class="stat-label">{{ card.label }}</div>
              <div class="stat-value">{{ card.value }}</div>
            </div>
            <v-icon :icon="card.icon" size="36" color="primary" opacity=".7" />
          </div>
        </v-card>
      </v-col>
    </v-row>

    <v-row class="mt-2">
      <v-col cols="12" md="6">
        <v-card class="pa-5">
          <div class="page-title mb-3">路由权重</div>
          <v-table density="comfortable">
            <tbody>
              <tr v-for="(v, k) in weights" :key="k">
                <td class="text-medium-emphasis">{{ k }}</td>
                <td class="mono">{{ v }}</td>
              </tr>
            </tbody>
          </v-table>
        </v-card>
      </v-col>
      <v-col cols="12" md="6">
        <v-card class="pa-5">
          <div class="page-title mb-3">节点分布</div>
          <v-list density="compact">
            <v-list-item v-for="n in nodeList.slice(0, 8)" :key="n.peer_id">
              <template #prepend><v-icon icon="mdi-server" size="18" /></template>
              <v-list-item-title>{{ n.hostname || n.peer_id }}</v-list-item-title>
              <v-list-item-subtitle class="mono">{{ n.ipv4 || '-' }} / {{ n.version }}</v-list-item-subtitle>
            </v-list-item>
          </v-list>
          <v-btn variant="text" size="small" to="/nodes">查看全部</v-btn>
        </v-card>
      </v-col>
    </v-row>
  </div>
</template>

<script setup>
import { computed, onMounted, ref } from 'vue'
import { api } from '@/api'

const overview = ref({})
const nodeList = ref([])
const loading = ref(false)

const cards = computed(() => [
  { label: '网络实例', value: overview.value.instances ?? '-', icon: 'mdi-lan' },
  { label: '节点数', value: overview.value.nodes ?? '-', icon: 'mdi-server-network' },
  { label: '直连链路', value: overview.value.links ?? '-', icon: 'mdi-link-variant' },
  { label: '控制台', value: '在线', icon: 'mdi-shield-check' },
])

const weights = computed(() => ({
  '延迟权重': overview.value.routing?.latency_weight,
  '丢包权重': overview.value.routing?.loss_weight,
  '带宽权重': overview.value.routing?.bandwidth_weight,
  '参考带宽(Mbps)': overview.value.routing?.reference_bandwidth_mbps,
}))

async function load() {
  loading.value = true
  try {
    overview.value = await api.overview()
    const nodes = await api.nodes()
    nodeList.value = nodes.nodes || []
  } finally {
    loading.value = false
  }
}

onMounted(load)
</script>
