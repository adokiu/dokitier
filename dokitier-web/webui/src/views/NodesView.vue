<template>
  <div>
    <div class="d-flex align-center mb-3 flex-wrap gap-3">
      <div>
        <div class="page-title">节点</div>
        <div class="page-sub">节点信息、直连链路与本地配置文件</div>
      </div>
      <v-spacer />
      <v-chip size="small" variant="tonal" color="primary">共 {{ nodes.length }} 个节点</v-chip>
      <v-chip size="small" variant="tonal" color="success">本地 {{ localCount }} 个</v-chip>
      <v-btn size="small" variant="text" prepend-icon="mdi-refresh" :loading="loading" @click="load">刷新</v-btn>
    </div>

    <v-card>
      <v-data-table
        :headers="headers"
        :items="sortedNodes"
        :loading="loading"
        item-value="peer_id"
        hover
        @click:row="(_, row) => open(row.item)"
      >
        <template #item.hostname="{ item }">
          <div class="d-flex align-center gap-2">
            <v-icon :icon="item.local ? 'mdi-lan-connect' : 'mdi-server-network'" size="16" :color="item.local ? 'primary' : 'medium-emphasis'" />
            <span>{{ item.hostname }}</span>
            <v-chip v-if="item.local" size="x-small" color="primary" variant="tonal">本机</v-chip>
          </div>
        </template>
        <template #item.ipv4="{ item }"><span class="mono">{{ item.ipv4 || '-' }}</span></template>
        <template #item.version="{ item }"><v-chip size="x-small" variant="tonal">{{ item.version || '-' }}</v-chip></template>
        <template #item.instance_id="{ item }"><span class="mono text-caption">{{ item.instance_id || '-' }}</span></template>
        <template #item.next_hop_peer_id="{ item }"><span class="mono">{{ item.next_hop_peer_id ?? '-' }}</span></template>
      </v-data-table>
    </v-card>

    <DetailDrawer v-model="drawer" :title="detail?.hostname || ''" :subtitle="detailSubtitle" :width="560">
      <template v-if="detail">
        <v-tabs v-model="tab" density="compact" color="primary" class="mb-3">
          <v-tab value="overview">概览</v-tab>
          <v-tab v-if="detail.local" value="config">配置文件</v-tab>
        </v-tabs>

        <v-window v-model="tab">
          <v-window-item value="overview">
            <div class="node-grid">
              <div><div class="stat-label">Peer ID</div><div class="node-value mono">{{ detail.peer_id }}</div></div>
              <div><div class="stat-label">IPv4</div><div class="node-value mono">{{ detail.ipv4 || '-' }}</div></div>
              <div><div class="stat-label">IPv6</div><div class="node-value mono">{{ detail.ipv6 || '-' }}</div></div>
              <div><div class="stat-label">版本</div><div class="node-value">{{ detail.version || '-' }}</div></div>
              <div><div class="stat-label">实例 ID</div><div class="node-value mono">{{ detail.instance_id || '-' }}</div></div>
              <div><div class="stat-label">角色</div><div class="node-value">{{ detail.local ? '本机节点' : '远端节点' }}</div></div>
              <div><div class="stat-label">下一跳</div><div class="node-value mono">{{ detail.next_hop_peer_id ?? '-' }}</div></div>
              <div><div class="stat-label">路由成本</div><div class="node-value">{{ detail.cost ?? '-' }}</div></div>
            </div>

            <div class="stat-label mt-4 mb-2">直连链路 ({{ detail.conns?.length || 0 }})</div>
            <div v-for="c in detail.conns || []" :key="c.conn_id" class="conn-row">
              <div class="d-flex align-center gap-2">
                <v-chip size="x-small" variant="tonal">{{ c.tunnel?.tunnel_type || 'unknown' }}</v-chip>
                <span class="mono text-caption">{{ c.tunnel?.remote_addr || c.conn_id }}</span>
              </div>
              <div class="text-caption text-medium-emphasis mt-1">
                延迟 {{ c.stats?.latency_us ? (c.stats.latency_us / 1000).toFixed(2) + ' ms' : '-' }}
                / 丢包 {{ c.loss_rate != null ? (c.loss_rate * 100).toFixed(2) + ' %' : '-' }}
                / 收 {{ c.stats ? formatBytes(c.stats.rx_bytes) : '-' }}
                / 发 {{ c.stats ? formatBytes(c.stats.tx_bytes) : '-' }}
              </div>
            </div>
          </v-window-item>

          <v-window-item v-if="detail.local" value="config">
            <div class="d-flex align-center mb-2">
              <span class="mono text-caption">{{ configPath || '配置文件' }}</span>
              <v-spacer />
              <v-btn size="x-small" variant="text" prepend-icon="mdi-content-copy" @click="copyConfig">复制</v-btn>
              <v-btn size="x-small" variant="text" prepend-icon="mdi-download" @click="downloadConfig">下载</v-btn>
            </div>
            <v-textarea
              v-model="configText"
              class="config-editor mono"
              rows="24"
              variant="outlined"
              density="compact"
              spellcheck="false"
              :loading="configLoading"
            />
            <v-alert v-if="configError" type="error" variant="tonal" density="compact" class="mb-2">{{ configError }}</v-alert>
            <v-alert v-if="saved" type="success" variant="tonal" density="compact" class="mb-2">
              已写入配置文件。重启 dokitier-core 后生效。
            </v-alert>
            <div class="d-flex gap-2">
              <v-btn color="primary" :loading="saving" @click="saveConfig">保存配置</v-btn>
              <v-btn variant="text" :loading="configLoading" @click="loadConfig">重新加载</v-btn>
            </div>
          </v-window-item>
        </v-window>
      </template>
    </DetailDrawer>
  </div>
</template>

<script setup>
import { computed, onMounted, ref, watch } from 'vue'
import { api } from '@/api'
import DetailDrawer from '@/components/DetailDrawer.vue'

const headers = [
  { title: '节点', key: 'hostname' },
  { title: 'IPv4', key: 'ipv4' },
  { title: '版本', key: 'version' },
  { title: '实例 ID', key: 'instance_id' },
  { title: '下一跳', key: 'next_hop_peer_id' },
  { title: '成本', key: 'cost' },
]
const nodes = ref([])
const localCount = computed(() => nodes.value.filter((n) => n.local).length)
const sortedNodes = computed(() => [...nodes.value].sort((a, b) => {
  if (!!a.local !== !!b.local) return a.local ? -1 : 1
  return String(a.hostname || '').localeCompare(String(b.hostname || ''))
}))
const loading = ref(false)
const drawer = ref(false)
const detail = ref(null)
const tab = ref('overview')

const configText = ref('')
const configPath = ref('')
const configLoading = ref(false)
const configError = ref('')
const saving = ref(false)
const saved = ref(false)

const detailSubtitle = computed(() => {
  if (!detail.value) return ''
  return `${detail.value.peer_id || ''}  ${detail.value.ipv4 || ''}`
})

function formatBytes(v) {
  if (v == null) return '-'
  const units = ['B', 'KB', 'MB', 'GB']
  let n = Number(v), i = 0
  while (n >= 1024 && i < units.length - 1) { n /= 1024; i++ }
  return `${n.toFixed(1)} ${units[i]}`
}

async function load() {
  loading.value = true
  try { nodes.value = (await api.nodes()).nodes || [] } finally { loading.value = false }
}

async function open(item) {
  detail.value = await api.node(item.peer_id)
  tab.value = 'overview'
  configText.value = ''
  configError.value = ''
  saved.value = false
  drawer.value = true
}

async function loadConfig() {
  configLoading.value = true
  configError.value = ''
  saved.value = false
  try {
    const data = await api.getConfig()
    configText.value = data.toml || ''
    configPath.value = data.path || ''
  } catch (e) {
    configError.value = String(e.message || e)
  } finally {
    configLoading.value = false
  }
}

async function saveConfig() {
  saving.value = true
  configError.value = ''
  saved.value = false
  try {
    await api.saveConfig(configText.value)
    saved.value = true
  } catch (e) {
    configError.value = String(e.message || e)
  } finally {
    saving.value = false
  }
}

async function copyConfig() {
  try { await navigator.clipboard.writeText(configText.value) } catch {}
}

function downloadConfig() {
  const blob = new Blob([configText.value], { type: 'text/plain' })
  const a = document.createElement('a')
  a.href = URL.createObjectURL(blob)
  a.download = 'dokitier.toml'
  a.click()
  URL.revokeObjectURL(a.href)
}

watch(tab, (value) => {
  if (value === 'config' && !configText.value && !configLoading.value) loadConfig()
})

onMounted(load)
</script>

<style scoped>
.page-sub { margin-top: 4px; font-size: 12px; color: var(--dok-muted); }
.node-grid { display: grid; grid-template-columns: 1fr 1fr; gap: 14px 18px; }
.node-value { margin-top: 3px; font-size: 12.5px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.conn-row { padding: 8px 0; }
.config-editor :deep(textarea) { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 12px; line-height: 1.55; }
</style>
