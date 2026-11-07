<template>
  <div>
    <div class="d-flex align-center mb-3 flex-wrap gap-3">
      <div>
        <div class="page-title">节点</div>
        <div class="page-sub">{{ mode === 'board' ? '控制台节点与网络配置' : '本机节点与配置文件' }}</div>
      </div>
      <v-spacer />
      <v-chip size="small" variant="tonal" color="primary">共 {{ nodes.length }} 个节点</v-chip>
      <v-btn size="small" variant="text" prepend-icon="mdi-refresh" :loading="loading" @click="load">刷新</v-btn>
    </div>

    <v-card>
      <v-data-table :headers="headers" :items="nodes" :loading="loading" item-value="node_id" hover @click:row="(_, row) => open(row.item)">
        <template #item.hostname="{ item }">
          <div class="d-flex align-center gap-2">
            <v-icon icon="mdi-server-network" size="16" color="primary" />
            <span>{{ item.hostname || item.node_id }}</span>
          </div>
        </template>
        <template #item.status="{ item }">
          <v-chip size="x-small" :color="item.status === 'online' ? 'success' : 'error'" variant="tonal">{{ item.status }}</v-chip>
        </template>
        <template #item.networks="{ item }">
          <div class="d-flex flex-wrap gap-1">
            <v-chip v-for="n in networksOf(item)" :key="n.network_name" size="x-small" variant="tonal">
              {{ n.network_name }}<span v-if="n.p2p_group"> / {{ n.p2p_group }}</span>
            </v-chip>
            <span v-if="!networksOf(item).length" class="text-caption text-medium-emphasis">-</span>
          </div>
        </template>
        <template #item.version="{ item }"><span class="text-caption">{{ item.version || '-' }}</span></template>
        <template #item.last_seen="{ item }">{{ item.last_seen ? new Date(item.last_seen * 1000).toLocaleString() : '-' }}</template>
        <template #item.actions="{ item }">
          <div class="d-flex gap-1">
            <v-btn size="x-small" variant="text" prepend-icon="mdi-cog" @click.stop="open(item)">配置</v-btn>
            <v-btn size="x-small" variant="text" prepend-icon="mdi-refresh" @click.stop="refreshItem(item)">刷新</v-btn>
          </div>
        </template>
      </v-data-table>
    </v-card>

    <DetailDrawer v-model="drawer" :title="detail?.hostname || detail?.node_id || ''" :subtitle="detail ? detail.node_id : ''" :width="620">
      <template v-if="detail">
        <div class="stat-label mb-2">网络 ({{ currentNetworks.length }})</div>
        <div v-for="n in currentNetworks" :key="n.network_name" class="net-row" :class="{ active: n.network_name === network }" @click="selectNetwork(n.network_name)">
          <div>
            <div class="net-name">{{ n.network_name }}</div>
            <div class="net-meta">p2p 组 {{ n.p2p_group || '未分组' }} / {{ n.ipv4 || '-' }}</div>
          </div>
          <v-chip size="x-small" variant="tonal" color="primary">{{ n.instance_id ? '运行中' : '-' }}</v-chip>
        </div>

        <template v-if="network">
          <v-divider class="my-4" />
          <div class="d-flex align-center mb-2">
            <span class="stat-label">配置文件 / {{ network }}</span>
            <v-spacer />
            <v-btn size="x-small" variant="text" prepend-icon="mdi-history" @click="openVersions">历史</v-btn>
            <v-btn size="x-small" variant="text" prepend-icon="mdi-download" @click="downloadConfig">下载</v-btn>
          </div>
          <v-textarea v-model="configText" class="config-editor mono" rows="18" variant="outlined" density="compact" spellcheck="false" :loading="configLoading" />
          <v-alert v-if="configError" type="error" variant="tonal" density="compact" class="mb-2">{{ configError }}</v-alert>
          <v-alert v-if="saved" type="success" variant="tonal" density="compact" class="mb-2">已下发并重启该网络实例</v-alert>
          <div class="d-flex gap-2">
            <v-btn color="primary" :loading="saving" @click="saveConfig">保存并应用</v-btn>
            <v-btn variant="text" :loading="configLoading" @click="loadConfig">重新加载</v-btn>
          </div>
        </template>
      </template>
    </DetailDrawer>

    <v-dialog v-model="versionsOpen" max-width="620">
      <v-card class="pa-2">
        <v-card-title>配置历史 / {{ network }}</v-card-title>
        <v-list density="compact">
          <v-list-item v-for="v in versions" :key="v.id">
            <v-list-item-title>{{ new Date(v.created * 1000).toLocaleString() }}</v-list-item-title>
            <v-list-item-subtitle class="mono">{{ v.network }} / {{ v.hash.slice(0, 16) }}</v-list-item-subtitle>
            <template #append>
              <v-btn size="x-small" variant="text" :loading="rollingBack === v.id" @click="rollback(v)">回滚</v-btn>
            </template>
          </v-list-item>
          <v-list-item v-if="!versions.length"><v-list-item-title class="text-medium-emphasis">暂无版本</v-list-item-title></v-list-item>
        </v-list>
      </v-card>
    </v-dialog>
  </div>
</template>

<script setup>
import { computed, onMounted, ref } from 'vue'
import { api } from '@/api'
import DetailDrawer from '@/components/DetailDrawer.vue'

const headers = [
  { title: '节点', key: 'hostname' },
  { title: '状态', key: 'status' },
  { title: '网络', key: 'networks', sortable: false },
  { title: '版本', key: 'version' },
  { title: '最后上报', key: 'last_seen' },
  { title: '操作', key: 'actions', sortable: false },
]
const mode = ref('board')
const nodes = ref([])
const loading = ref(false)
const drawer = ref(false)
const detail = ref(null)
const network = ref('')
const configText = ref('')
const configLoading = ref(false)
const configError = ref('')
const saving = ref(false)
const saved = ref(false)
const versionsOpen = ref(false)
const versions = ref([])
const rollingBack = ref(null)

function networksOf(item) {
  return item.snapshot?.instances || item.instances || []
}

const currentNetworks = computed(() => (detail.value ? networksOf(detail.value) : []))

function detailSubtitle() {
  return detail.value?.node_id || ''
}

async function load() {
  loading.value = true
  try {
    try {
      const data = await api.boardNodes()
      mode.value = 'board'
      nodes.value = (data.nodes || []).map((n) => ({
        ...n,
        hostname: n.hostname || n.snapshot?.instances?.[0]?.hostname,
        version: n.version || n.snapshot?.instances?.[0]?.version,
      }))
    } catch {
      mode.value = 'local'
      nodes.value = (await api.nodes()).nodes || []
    }
  } finally {
    loading.value = false
  }
}

async function refreshItem(item) {
  try {
    const fresh = await api.boardNode(item.node_id)
    Object.assign(item, fresh)
  } catch {}
}

async function open(item) {
  detail.value = item
  drawer.value = true
  network.value = networksOf(item)[0]?.network_name || ''
  configText.value = ''
  configError.value = ''
  saved.value = false
  if (network.value) await loadConfig()
}

async function selectNetwork(name) {
  network.value = name
  configText.value = ''
  configError.value = ''
  saved.value = false
  await loadConfig()
}

async function loadConfig() {
  configLoading.value = true
  configError.value = ''
  saved.value = false
  try {
    if (mode.value === 'board') {
      const res = await api.boardCommand(detail.value.node_id, { kind: 'get_config' }, network.value)
      if (!res.ok) throw new Error(res.error || '读取失败')
      configText.value = res.config || ''
    } else {
      const data = await api.getConfig()
      configText.value = data.toml || ''
    }
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
    if (mode.value === 'board') {
      const res = await api.boardCommand(detail.value.node_id, { kind: 'apply_config', toml: configText.value }, network.value)
      if (!res.ok) throw new Error(res.error || '应用失败')
    } else {
      await api.saveConfig(configText.value)
    }
    saved.value = true
    await load()
  } catch (e) {
    configError.value = String(e.message || e)
  } finally {
    saving.value = false
  }
}

async function openVersions() {
  versionsOpen.value = true
  try {
    const data = await api.boardVersions(detail.value.node_id)
    versions.value = data.versions || []
  } catch {
    versions.value = []
  }
}

async function rollback(version) {
  rollingBack.value = version.id
  try {
    const data = await api.boardVersion(detail.value.node_id, version.id)
    const res = await api.boardCommand(detail.value.node_id, { kind: 'apply_config', toml: data.toml }, data.network || network.value)
    if (!res.ok) throw new Error(res.error || '回滚失败')
    versionsOpen.value = false
    saved.value = true
    configText.value = data.toml
    await load()
  } catch (e) {
    configError.value = String(e.message || e)
  } finally {
    rollingBack.value = null
  }
}

function downloadConfig() {
  const blob = new Blob([configText.value], { type: 'text/plain' })
  const a = document.createElement('a')
  a.href = URL.createObjectURL(blob)
  a.download = `${detail.value?.node_id || 'node'}-${network.value}.toml`
  a.click()
  URL.revokeObjectURL(a.href)
}

onMounted(load)
</script>

<style scoped>
.page-sub { margin-top: 4px; font-size: 12px; color: var(--dok-muted); }
.net-row { display: flex; align-items: center; justify-content: space-between; gap: 12px; padding: 10px 12px; border-radius: 10px; cursor: pointer; border: 1px solid transparent; }
.net-row:hover { background: rgba(60, 150, 202, .05); }
.net-row.active { background: rgba(60, 150, 202, .08); border-color: rgba(60, 150, 202, .25); }
.net-name { font-size: 13px; font-weight: 600; }
.net-meta { margin-top: 3px; font-size: 11px; color: var(--dok-muted); }
.config-editor :deep(textarea) { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 12px; line-height: 1.55; }
</style>
