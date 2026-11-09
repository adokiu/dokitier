<template>
  <div class="networks-page">
    <div class="net-head">
      <div>
        <div class="page-title">网络</div>
        <div class="page-sub">所有网络的节点、网段与链路质量</div>
      </div>
      <v-btn color="primary" size="small" prepend-icon="mdi-plus" @click="openCreate">添加网络</v-btn>
    </div>

    <div class="net-cards">
      <v-card class="net-stat"><div class="stat-label">网络数量</div><div class="stat-value">{{ networks.length }}</div></v-card>
      <v-card class="net-stat"><div class="stat-label">在线节点</div><div class="stat-value">{{ totalOnline }}<span class="net-sub"> / {{ totalNodes }}</span></div></v-card>
      <v-card class="net-stat"><div class="stat-label">链路总数</div><div class="stat-value">{{ totalLinks }}</div></v-card>
      <v-card class="net-stat"><div class="stat-label">总流量</div><div class="stat-value">{{ humanBytes(totalTraffic) }}</div></v-card>
    </div>

    <v-card class="net-table-card">
      <DokTable :headers="headers" :items="networks" item-value="network" :items-per-page="20" hover>
        <template #item.network="{ item }"><span class="net-chip">{{ item.network }}</span></template>
        <template #item.status="{ item }">
          <v-chip size="x-small" :color="item.nodes_online === item.nodes_total ? 'success' : item.nodes_online > 0 ? 'warning' : 'error'" variant="tonal">
            {{ item.nodes_online === item.nodes_total ? '正常' : item.nodes_online > 0 ? '部分在线' : '离线' }}
          </v-chip>
        </template>
        <template #item.nodes="{ item }"><span class="num">{{ item.nodes_online }} / {{ item.nodes_total }}</span></template>
        <template #item.p2p_groups="{ item }"><span>{{ (item.p2p_groups || []).join(', ') || '-' }}</span></template>
        <template #item.subnets="{ item }"><span class="num">{{ (item.subnets || []).slice(0, 2).join(', ') || '-' }}</span></template>
        <template #item.quality="{ item }">
          <v-chip size="x-small" :color="qualityGrade(item).color" variant="tonal">{{ qualityGrade(item).label }}</v-chip>
        </template>
        <template #item.load="{ item }">
          <div class="load-cell">
            <div class="load-value num">{{ (item.load_pct || 0).toFixed(1) }}%</div>
            <div class="load-track"><span :style="{ width: (item.load_pct || 0) + '%' }"></span></div>
          </div>
        </template>
        <template #item.link_count="{ item }"><span class="num">{{ item.link_count }}</span></template>
        <template #item.traffic="{ item }"><span class="num">{{ humanBytes(item.traffic) }}</span></template>
        <template #item.actions="{ item }">
          <div class="d-flex gap-1">
            <v-btn size="x-small" variant="text" prepend-icon="mdi-cog" @click.stop="open(item)">配置</v-btn>
          </div>
        </template>
        <template #no-data><div class="net-empty">暂无网络</div></template>
      </DokTable>
    </v-card>

    <DetailDrawer v-model="createOpen" title="添加网络" subtitle="配置网络名称、密钥、地址与节点" :width="620">
      <div class="net-form">
        <ConfigField label="网络名称" required hint="全局唯一，只能包含字母、数字、-、_、.，长度 2-64。">
          <v-text-field v-model="form.name" density="compact" variant="outlined" hide-details placeholder="office-net" :error-messages="nameError" />
        </ConfigField>
        <ConfigField label="网络密钥" required hint="所有加入该网络的节点使用同一密钥；默认随机生成，点击右侧刷新图标重新生成。">
          <v-text-field v-model="form.secret" density="compact" variant="outlined" hide-details append-inner-icon="mdi-refresh" :error-messages="secretError" @click:append-inner="generateSecret" />
        </ConfigField>
        <div class="net-form__row">
          <ConfigField label="地址分配" hint="全自动：自动选未冲突网段并分配静态 IP；DHCP：由网络内 DHCP 分配；全手动：只创建配置，地址由你后续填写。">
            <v-select v-model="form.ip_mode" :items="ipModes" item-title="title" item-value="value" density="compact" variant="outlined" hide-details />
          </ConfigField>
          <ConfigField v-if="form.ip_mode !== 'manual'" label="网段 (/24)" hint="留空自动选择未使用的 10.x.x.0/24；手动填写时不能与任何现有网络重叠。">
            <v-text-field v-model="form.subnet" density="compact" variant="outlined" hide-details placeholder="留空自动选择" />
          </ConfigField>
        </div>
        <div>
          <div class="cfg-label-row net-label-row">
            <span class="cfg-label">选择节点</span>
            <v-tooltip location="top" max-width="360" open-delay="120">
              <template #activator="{ props }">
                <v-icon v-bind="props" icon="mdi-information-outline" size="14" class="cfg-info" />
              </template>
              <div class="cfg-tip">勾选要加入该网络的节点，可选的 P2P 分组用于限制同组节点互相连接。</div>
            </v-tooltip>
          </div>
          <div class="node-pick">
            <label v-for="node in allNodes" :key="node.node_id" class="node-pick__row" :class="{ 'node-pick__row--on': selected[node.node_id] }">
              <v-checkbox v-model="selected[node.node_id]" hide-details density="compact" />
              <div class="node-pick__info">
                <div class="node-pick__identity">
                  <span v-if="flagUrl(node.country_code)" class="node-flag-wrap"><img class="node-flag" :src="flagUrl(node.country_code)" alt="" loading="lazy" /></span>
                  <span class="node-pick__name">{{ node.alias || node.hostname || node.node_id }}</span>
                </div>
              </div>
              <v-text-field v-if="selected[node.node_id]" v-model="groups[node.node_id]" label="P2P 分组" density="compact" variant="outlined" hide-details placeholder="可选" class="node-pick__group" />
            </label>
            <div v-if="!allNodes.length" class="net-empty">暂无在线节点</div>
          </div>
        </div>
        <div class="switch-item">
          <div class="cfg-label-row"><span class="cfg-label">创建 TUN 接口</span></div>
          <v-switch v-model="form.tun" color="primary" density="compact" hide-details />
        </div>
        <div class="switch-item">
          <div class="cfg-label-row">
            <span class="cfg-label">保存后自动生成并下发节点配置</span>
            <v-tooltip location="top" max-width="340" open-delay="120">
              <template #activator="{ props }">
                <v-icon v-bind="props" icon="mdi-information-outline" size="14" class="cfg-info" />
              </template>
              <div class="cfg-tip">开启后根据现有链路质量自动选择 peer 并下发配置；关闭则只保存配置，由你后续手动配置。</div>
            </v-tooltip>
          </div>
          <v-switch v-model="form.auto_connect" color="primary" density="compact" hide-details />
        </div>
        <v-alert v-if="createError" type="error" variant="tonal" density="compact">{{ createError }}</v-alert>
        <div class="form-actions">
          <v-btn variant="text" @click="createOpen = false">取消</v-btn>
          <v-btn color="primary" :loading="saving" @click="submitCreate">保存</v-btn>
        </div>
      </div>
    </DetailDrawer>

    <v-dialog v-model="confirmOpen" max-width="420">
      <v-card class="pa-2">
        <v-card-title>自动连接网络？</v-card-title>
        <v-card-text>是否根据节点间网络质量自动生成拓扑并下发配置？否的话只创建网络，后续由你手动配置节点。</v-card-text>
        <v-card-actions><v-spacer /><v-btn variant="text" @click="createNetwork(false)">否，仅创建</v-btn><v-btn color="primary" @click="createNetwork(true)">是，自动生成</v-btn></v-card-actions>
      </v-card>
    </v-dialog>

    <v-snackbar v-model="snack" :color="snackColor">{{ snackText }}</v-snackbar>
    <DetailDrawer v-model="drawer" :title="detail?.network || ''" subtitle="网络参数" :width="620">
      <template v-if="detail">
        <div class="net-detail-grid">
          <div><div class="stat-label">节点</div><div class="stat-value">{{ detail.nodes_online }} / {{ detail.nodes_total }}</div></div>
          <div><div class="stat-label">链路</div><div class="stat-value">{{ detail.link_count }}</div></div>
          <div><div class="stat-label">平均延迟</div><div class="stat-value">{{ detail.avg_latency_ms ? detail.avg_latency_ms.toFixed(2) + ' ms' : '-' }}</div></div>
          <div><div class="stat-label">平均丢包</div><div class="stat-value">{{ detail.avg_loss_pct ? detail.avg_loss_pct.toFixed(2) + ' %' : '-' }}</div></div>
        </div>
        <div class="stat-label mt-4 mb-1">网段</div>
        <div class="net-tags"><span v-for="s in detail.subnets" :key="s" class="net-tag num">{{ s }}</span></div>
        <div class="stat-label mt-4 mb-1">P2P 分组</div>
        <div class="net-tags"><span v-for="g in detail.p2p_groups" :key="g" class="net-tag">{{ g }}</span></div>
        <div class="stat-label mt-4 mb-2">节点</div>
        <div class="net-node-list">
          <div v-for="node in detail.nodes" :key="node.node_id" class="net-node-row">
            <span>{{ node.node_id }}</span>
            <span class="num">{{ node.ipv4 || '-' }}</span>
            <v-chip size="x-small" :color="node.status === 'online' ? 'success' : 'error'" variant="tonal">{{ node.status }}</v-chip>
          </div>
        </div>
      </template>
    </DetailDrawer>
  </div>
</template>

<script setup>
import { computed, onBeforeUnmount, onMounted, ref } from 'vue'
import { api } from '@/api'
import DokTable from '@/components/DokTable.vue'
import DetailDrawer from '@/components/DetailDrawer.vue'
import { flagUrl } from '@/utils/flags'
import ConfigField from '@/components/ConfigField.vue'

const networks = ref([])
const loading = ref(false)
const drawer = ref(false)
const detail = ref(null)
const allNodes = ref([])
const createOpen = ref(false)
const confirmOpen = ref(false)
const saving = ref(false)
const createError = ref('')
const selected = ref({})
const groups = ref({})
const form = ref({ name: '', secret: '', ip_mode: 'auto', subnet: '', auto_connect: true, tun: true })
const snack = ref(false)
const snackText = ref('')
const snackColor = ref('success')
const ipModes = [
  { title: '全自动（静态分配）', value: 'auto' },
  { title: 'DHCP（指定网段）', value: 'dhcp' },
  { title: '全手动', value: 'manual' },
]

const headers = [
  { title: '网络', key: 'network' },
  { title: '状态', key: 'status', sortable: false },
  { title: '节点', key: 'nodes' },
  { title: 'P2P 分组', key: 'p2p_groups' },
  { title: '网段', key: 'subnets' },
  { title: '质量', key: 'quality', sortable: false },
  { title: '负载', key: 'load', sortable: false },
  { title: '链路', key: 'link_count' },
  { title: '流量', key: 'traffic' },
  { title: '操作', key: 'actions', sortable: false },
]
const totalOnline = computed(() => networks.value.reduce((sum, n) => sum + (n.nodes_online || 0), 0))
const totalNodes = computed(() => networks.value.reduce((sum, n) => sum + (n.nodes_total || 0), 0))
const totalLinks = computed(() => networks.value.reduce((sum, n) => sum + (n.link_count || 0), 0))
const totalTraffic = computed(() => networks.value.reduce((sum, n) => sum + (n.traffic || 0), 0))

function humanBytes(value) {
  const units = ['B', 'KB', 'MB', 'GB', 'TB']
  let size = Number(value) || 0
  let index = 0
  while (size >= 1024 && index < units.length - 1) { size /= 1024; index += 1 }
  return `${size.toFixed(index === 0 ? 0 : 1)} ${units[index]}`
}
const secretError = computed(() => {
  const secret = form.value.secret || ''
  if (!secret) return ''
  if (secret.length < 16) return '密钥至少 16 位'
  return ''
})

const nameError = computed(() => {
  const name = form.value.name.trim()
  if (!name) return ''
  if (!/^[A-Za-z0-9_.-]{2,64}$/.test(name)) return '只能包含字母/数字/-/_/.，2-64 位'
  if (networks.value.some((n) => n.network === name)) return '网络名称已存在'
  return ''
})

function generateSecret() {
  const bytes = new Uint8Array(24)
  crypto.getRandomValues(bytes)
  form.value.secret = btoa(String.fromCharCode(...bytes)).replace(/[+/=]/g, '').slice(0, 32)
}

function openCreate() {
  createError.value = ''
  form.value = { name: '', secret: '', ip_mode: 'auto', subnet: '', auto_connect: true, tun: true }
  generateSecret()
  selected.value = {}
  groups.value = {}
  createOpen.value = true
  api.boardNodes().then((data) => { allNodes.value = (data.nodes || []).filter((n) => n.status === 'online') }).catch(() => {})
}

function submitCreate() {
  if (!form.value.name.trim()) { createError.value = '请填写网络名称'; return }
  if (nameError.value) { createError.value = nameError.value; return }
  if (!form.value.secret) { createError.value = '请填写网络密钥'; return }
  if (secretError.value) { createError.value = secretError.value; return }
  if (!Object.values(selected.value).some(Boolean)) { createError.value = '至少选择一个节点'; return }
  if (form.value.auto_connect) { confirmOpen.value = true; return }
  createNetwork(false)
}

async function createNetwork(autoConnect) {
  confirmOpen.value = false
  saving.value = true
  createError.value = ''
  try {
    const nodes = allNodes.value.filter((n) => selected.value[n.node_id]).map((n) => ({
      node_id: n.node_id,
      p2p_group: (groups.value[n.node_id] || '').trim() || null,
    }))
    const result = await api.createNetwork({
      name: form.value.name.trim(),
      secret: form.value.secret,
      ip_mode: form.value.ip_mode,
      subnet: form.value.subnet.trim() || null,
      nodes,
      auto_connect: autoConnect,
      no_tun: !form.value.tun,
    })
    if (!result.ok) throw new Error((result.failed && result.failed[0] && result.failed[0].error) || result.error || '创建失败')
    createOpen.value = false
    snackText.value = autoConnect ? `网络已创建并下发 ${result.applied.length} 个节点` : '网络已创建'
    snackColor.value = 'success'
    snack.value = true
    await load()
  } catch (e) {
    createError.value = String(e.message || e)
  } finally {
    saving.value = false
  }
}

function qualityGrade(item) {
  // 质量只看丢包/稳定性；跨洲 IEPL/IPLC 的绝对延迟高是物理距离决定的，
  // 不能因此判定链路差。延迟只在详情里作为参考信息展示。
  const loss = Number(item.avg_loss_pct) || 0
  if (!item.link_count) return { label: '未测量', color: 'default' }
  if (loss < 0.1) return { label: '优秀', color: 'success' }
  if (loss < 0.5) return { label: '良好', color: 'info' }
  if (loss < 1.5) return { label: '中等', color: 'warning' }
  if (loss < 5) return { label: '较差', color: 'deep-orange' }
  return { label: '极差', color: 'error' }
}

function open(item) { detail.value = item; drawer.value = true }
async function load() {
  loading.value = true
  try {
    const data = await api.boardNetworks()
    networks.value = data.networks || []
  } finally { loading.value = false }
}
let timer = null
onMounted(() => { load(); timer = setInterval(load, 5000) })
onBeforeUnmount(() => { if (timer) clearInterval(timer) })
</script>

<style scoped>
.networks-page { width: 100%; }
.net-head { display: flex; align-items: flex-start; justify-content: space-between; gap: 16px; margin-bottom: 16px; }
.net-cards { display: grid; grid-template-columns: repeat(4, minmax(0, 1fr)); gap: 12px; margin-bottom: 16px; }
.net-stat { padding: 16px 18px; }
.net-sub { font-size: 14px; color: rgba(var(--v-theme-on-surface), .5); }
.net-table-card { overflow: hidden; }
.cfg-tip { font-size: 12px; line-height: 1.5; color: rgba(var(--v-theme-on-surface), .7); }
.cfg-info { color: rgba(var(--v-theme-on-surface), .45); }
.net-form :deep(.cfg-label) { font-size: 13px; font-weight: 500; }
.net-label-row { display: flex; align-items: center; gap: 4px; font-size: 13px; font-weight: 500; color: rgba(var(--v-theme-on-surface), .62); margin-bottom: 5px; }
.net-form { display: flex; flex-direction: column; gap: 14px; }
.net-form__row { display: grid; grid-template-columns: 1fr 1fr; gap: 12px; }
.node-pick { display: flex; flex-direction: column; }
.node-pick__row { display: flex; align-items: center; gap: 10px; min-height: 40px; padding: 6px 8px; border-radius: 8px; cursor: pointer; }
.node-pick__row--on { background: rgba(var(--v-theme-primary), .06); }
.node-pick__info { flex: 1 1 40%; min-width: 0; }
.node-pick__identity { display: flex; align-items: center; gap: 8px; min-width: 0; }
.node-pick__name { font-size: 14px; font-weight: 500; line-height: 20px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.node-pick__group { flex: 0 0 160px; max-width: 160px; }
.node-flag-wrap { display: inline-flex; align-items: center; justify-content: center; height: 20px; flex: 0 0 auto; }
.node-flag { display: block; width: 20px; height: 15px; border-radius: 2px; object-fit: cover; }
.switch-item { display: flex; flex-direction: column; gap: 2px; align-items: flex-start; }
.switch-item :deep(.cfg-label-row) { display: flex; align-items: center; gap: 4px; font-size: 13px; font-weight: 500; color: rgba(var(--v-theme-on-surface), .62); }
.form-actions { display: flex; justify-content: flex-end; gap: 10px; margin-top: 4px; padding-top: 12px; }
@media (max-width: 640px) { .net-form__row { grid-template-columns: 1fr; } .node-pick__group { flex-basis: 120px; } }
.load-cell { display: block; width: 120px; }
.load-value { font-size: 12px; line-height: 16px; white-space: nowrap; }
.load-track { width: 120px; height: 4px; margin-top: 3px; border-radius: 999px; background: rgba(var(--v-theme-on-surface), .14); overflow: hidden; }
.load-track span { display: block; height: 100%; border-radius: 999px; background: rgb(var(--v-theme-primary)); transition: width .3s ease; }
.net-chip { display: inline-flex; padding: 2px 8px; border-radius: 6px; font-size: 11.5px; font-weight: 600; color: rgb(var(--v-theme-primary)); background: rgba(var(--v-theme-primary), .08); }
.num { font-variant-numeric: tabular-nums; }
.net-empty { text-align: center; padding: 24px 0; color: rgba(var(--v-theme-on-surface), .5); }
.net-detail-grid { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 14px 18px; }
.net-tags { display: flex; flex-wrap: wrap; gap: 6px; }
.net-tag { padding: 2px 8px; border-radius: 6px; background: rgba(var(--v-theme-on-surface), .06); font-size: 12px; }
.net-node-list { display: flex; flex-direction: column; }
.net-node-row { display: grid; grid-template-columns: 1fr 1fr 70px; gap: 10px; align-items: center; padding: 6px 0; border-bottom: 1px solid rgba(var(--v-theme-on-surface), .06); font-size: 13px; }
@media (max-width: 1000px) { .net-cards { grid-template-columns: repeat(2, minmax(0, 1fr)); } }
</style>
