<template>
  <div>
    <div class="d-flex align-center mb-3 flex-wrap gap-3">
      <div>
        <div class="page-title">{{ t('nodes.title') }}</div>
        <div class="page-sub">{{ t('nodes.subtitle') }}</div>
      </div>
      <v-spacer />
      <v-select
        v-model="networkFilter"
        :items="networkOptions"
        :label="t('nodes.networkFilter')"
        density="compact"
        variant="outlined"
        hide-details
        style="max-width: 240px"
      />
      <v-chip size="small" variant="tonal" color="primary">{{ t('nodes.total', { n: filteredNodes.length }) }}</v-chip>
      <v-btn size="small" variant="text" prepend-icon="mdi-refresh" :loading="loading" @click="load">{{ t('common.refresh') }}</v-btn>
    </div>

    <v-card>
      <v-data-table
        :headers="headers"
        :items="filteredNodes"
        :loading="loading"
        item-value="node_id"
        hover
        @click:row="(_, row) => open(row.item)"
      >
        <template #item.hostname="{ item }">
          <div class="d-flex align-center gap-2 node-name">
            <img
              v-if="flagUrl(item)"
              :src="flagUrl(item)"
              class="node-flag"
              alt=""
              loading="lazy"
            />
            <v-icon v-else icon="mdi-earth" size="14" class="text-medium-emphasis" />
            <span class="node-host">{{ item.hostname || item.node_id }}</span>
            <v-chip v-if="item.status !== 'online'" size="x-small" color="error" variant="tonal">{{ item.status }}</v-chip>
          </div>
        </template>
        <template #item.ip="{ item }">
          <span class="mono text-caption">{{ item.public_ipv4 || item.public_ipv6 || overlayIp(item) || '-' }}</span>
        </template>
        <template #item.version="{ item }"><span class="text-caption">{{ item.version || '-' }}</span></template>
        <template #item.traffic="{ item }">
          <div class="traffic">
            <span class="up">↑ {{ bps(item.tx_bps) }}</span>
            <span class="down">↓ {{ bps(item.rx_bps) }}</span>
          </div>
        </template>
        <template #item.last_seen="{ item }">{{ item.last_seen ? new Date(item.last_seen * 1000).toLocaleString() : '-' }}</template>
        <template #item.actions="{ item }">
          <div class="d-flex gap-1">
            <v-btn size="x-small" variant="text" prepend-icon="mdi-cog" @click.stop="open(item)">{{ t('nodes.config') }}</v-btn>
            <v-btn size="x-small" variant="text" prepend-icon="mdi-refresh" @click.stop="refreshItem(item)">{{ t('common.refresh') }}</v-btn>
          </div>
        </template>
      </v-data-table>
    </v-card>

    <DetailDrawer v-model="drawer" :title="detail?.hostname || detail?.node_id || ''" :subtitle="detail ? detail.node_id : ''" :width="620">
      <template v-if="detail">
        <div class="stat-label mb-2">{{ t('nodes.networks') }} ({{ currentNetworks.length }})</div>
        <div v-for="n in currentNetworks" :key="n.network_name" class="net-row" :class="{ active: n.network_name === network }" @click="selectNetwork(n.network_name)">
          <div>
            <div class="net-name">{{ n.network_name }}</div>
            <div class="net-meta">p2p {{ n.p2p_group || '-' }} / {{ n.ipv4 || '-' }}</div>
          </div>
          <v-chip size="x-small" variant="tonal" color="primary">{{ t('nodes.running') }}</v-chip>
        </div>

        <template v-if="network">
          <v-divider class="my-4" />
          <div class="d-flex align-center mb-2">
            <span class="stat-label">{{ t('nodes.configFile') }} / {{ network }}</span>
            <v-spacer />
            <v-btn size="x-small" variant="text" prepend-icon="mdi-history" @click="openVersions">{{ t('nodes.history') }}</v-btn>
            <v-btn size="x-small" variant="text" prepend-icon="mdi-download" @click="downloadConfig">{{ t('nodes.download') }}</v-btn>
          </div>
          <v-textarea v-model="configText" class="config-editor mono" rows="16" variant="outlined" density="compact" spellcheck="false" :loading="configLoading" />
          <v-alert v-if="configError" type="error" variant="tonal" density="compact" class="mb-2">{{ configError }}</v-alert>
          <v-alert v-if="saved" type="success" variant="tonal" density="compact" class="mb-2">{{ t('nodes.applied') }}</v-alert>
          <div class="d-flex gap-2">
            <v-btn color="primary" :loading="saving" @click="saveConfig">{{ t('nodes.saveApply') }}</v-btn>
            <v-btn variant="text" :loading="configLoading" @click="loadConfig">{{ t('nodes.reload') }}</v-btn>
          </div>
        </template>
      </template>
    </DetailDrawer>

    <v-dialog v-model="versionsOpen" max-width="620">
      <v-card class="pa-2">
        <v-card-title>{{ t('nodes.history') }} / {{ network }}</v-card-title>
        <v-list density="compact">
          <v-list-item v-for="v in versions" :key="v.id">
            <v-list-item-title>{{ new Date(v.created * 1000).toLocaleString() }}</v-list-item-title>
            <v-list-item-subtitle class="mono">{{ v.network }} / {{ v.hash.slice(0, 16) }}</v-list-item-subtitle>
            <template #append>
              <v-btn size="x-small" variant="text" :loading="rollingBack === v.id" @click="rollback(v)">回滚</v-btn>
            </template>
          </v-list-item>
          <v-list-item v-if="!versions.length"><v-list-item-title class="text-medium-emphasis">{{ t('nodes.noVersions') }}</v-list-item-title></v-list-item>
        </v-list>
      </v-card>
    </v-dialog>
  </div>
</template>

<script setup>
import { computed, onMounted, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { api } from '@/api'
import DetailDrawer from '@/components/DetailDrawer.vue'

const { t } = useI18n()
const headers = computed(() => [
  { title: t('nodes.columns.hostname'), key: 'hostname' },
  { title: t('nodes.columns.ip'), key: 'ip' },
  { title: t('nodes.columns.version'), key: 'version' },
  { title: t('nodes.columns.network'), key: 'traffic', sortable: false },
  { title: t('nodes.columns.lastSeen'), key: 'last_seen' },
  { title: t('nodes.columns.actions'), key: 'actions', sortable: false },
])

const nodes = ref([])
const loading = ref(false)
const drawer = ref(false)
const detail = ref(null)
const network = ref('')
const networkFilter = ref('')
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

const networkOptions = computed(() => {
  const names = new Set()
  for (const node of nodes.value) for (const n of networksOf(node)) if (n.network_name) names.add(n.network_name)
  return [...names]
})

const filteredNodes = computed(() => {
  if (!networkFilter.value) return nodes.value
  return nodes.value.filter((node) => networksOf(node).some((n) => n.network_name === networkFilter.value))
})

const currentNetworks = computed(() => (detail.value ? networksOf(detail.value) : []))

function bps(v) {
  if (v == null) return '0 B/s'
  const units = ['B/s', 'KB/s', 'MB/s', 'GB/s']
  let n = Number(v)
  let i = 0
  while (n >= 1024 && i < units.length - 1) { n /= 1024; i++ }
  return `${n.toFixed(1)} ${units[i]}`
}

function overlayIp(item) {
  const inst = networksOf(item)[0]
  return inst?.ipv4 || ''
}

function flagUrl(item) {
  const code = item.country_code
  return code && code.length === 2 ? `https://flagcdn.com/${code.toLowerCase()}.svg` : ''
}

async function load() {
  loading.value = true
  try {
    const data = await api.boardNodes()
    nodes.value = (data.nodes || []).map((n) => ({
      ...n,
      hostname: n.hostname || n.snapshot?.instances?.[0]?.hostname,
      version: n.version || n.snapshot?.instances?.[0]?.version,
    }))
    if (!networkFilter.value && networkOptions.value.length) networkFilter.value = networkOptions.value[0]
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
    const res = await api.boardCommand(detail.value.node_id, { kind: 'get_config' }, network.value)
    if (!res.ok) throw new Error(res.error || 'read failed')
    configText.value = res.config || ''
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
    const res = await api.boardCommand(detail.value.node_id, { kind: 'apply_config', toml: configText.value }, network.value)
    if (!res.ok) throw new Error(res.error || 'apply failed')
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
    if (!res.ok) throw new Error(res.error || 'rollback failed')
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
.node-name { min-width: 0; gap: 8px !important; }
.node-flag { width: 20px; height: 14px; border-radius: 2px; object-fit: cover; flex: 0 0 auto; }
.node-host { font-weight: 500; }
.traffic { display: flex; gap: 12px; font-size: 11px; color: var(--dok-muted); }
.traffic .up { color: #2e7d32; }
.traffic .down { color: #1565c0; }
.net-row { display: flex; align-items: center; justify-content: space-between; gap: 12px; padding: 10px 12px; border-radius: 10px; cursor: pointer; }
.net-row:hover { background: rgba(60, 150, 202, .05); }
.net-row.active { background: rgba(60, 150, 202, .08); }
.net-name { font-size: 13px; font-weight: 600; }
.net-meta { margin-top: 3px; font-size: 11px; color: var(--dok-muted); }
.config-editor :deep(textarea) { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 12px; line-height: 1.55; }
</style>
