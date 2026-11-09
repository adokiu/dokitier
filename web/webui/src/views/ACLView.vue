<template>
  <div class="acl-page">
    <div class="acl-head">
      <div>
        <div class="page-title">访问控制 ACL</div>
        <div class="page-sub">统一管理所有节点的 ACL 规则，保存后转换为各节点 TOML 的 [acl.acl_v1] 并下发</div>
      </div>
      <div class="acl-actions">
        <v-btn size="small" variant="text" prepend-icon="mdi-plus" @click="openChain">添加规则链</v-btn>
        <v-btn color="primary" size="small" :loading="saving" prepend-icon="mdi-content-save" @click="save">保存并下发</v-btn>
      </div>
    </div>

    <div class="acl-cards">
      <v-card class="acl-stat"><div class="stat-label">规则链</div><div class="stat-value">{{ chains.length }}</div></v-card>
      <v-card class="acl-stat"><div class="stat-label">规则总数</div><div class="stat-value">{{ rules.length }}</div></v-card>
      <v-card class="acl-stat"><div class="stat-label">放行</div><div class="stat-value">{{ rules.filter((r) => r.action === 1).length }}</div></v-card>
      <v-card class="acl-stat"><div class="stat-label">丢弃</div><div class="stat-value">{{ rules.filter((r) => r.action === 2).length }}</div></v-card>
    </div>

    <v-tabs v-model="tab" density="compact" class="acl-tabs mb-3">
      <v-tab value="rules">规则</v-tab>
      <v-tab value="chains">规则链</v-tab>
    </v-tabs>

    <div v-show="tab === 'rules'" class="acl-panel">
      <div class="acl-rules-head">
        <div class="acl-chain-select">
          <span class="acl-chain-label">规则链</span>
          <v-select v-model="selectedChain" :items="chainNames" density="compact" variant="outlined" hide-details />
        </div>
        <v-spacer />
        <v-btn size="small" variant="tonal" prepend-icon="mdi-plus" :disabled="!chains.length" @click="openRule">添加规则</v-btn>
      </div>

    <v-card class="acl-table-card">
      <DokTable :headers="headers" :items="rules" item-value="key" :items-per-page="20" hover>
        <template #item.action="{ item }">
          <v-chip size="x-small" :color="item.action === 1 ? 'success' : item.action === 2 ? 'error' : 'default'">
            {{ actionLabel(item.action) }}
          </v-chip>
        </template>
        <template #item.protocol="{ item }">{{ protocolLabel(item.protocol) }}</template>
        <template #item.source="{ item }">{{ (item.source_ips || []).join(', ') || '-' }}</template>
        <template #item.destination="{ item }">{{ (item.destination_ips || []).join(', ') || '-' }}</template>
        <template #item.ports="{ item }">{{ (item.ports || []).join(', ') || '-' }}</template>
        <template #item.priority="{ item }"><span class="num">{{ item.priority ?? 100 }}</span></template>
        <template #item.enabled="{ item }">
          <v-switch :model-value="item.enabled !== false" density="compact" hide-details color="primary" @update:model-value="(v) => setRuleEnabled(item, v)" />
        </template>
        <template #item.actions="{ item }">
          <v-btn size="x-small" variant="text" color="error" @click="removeRule(item)">删除</v-btn>
        </template>
        <template #no-data><div class="acl-empty">暂无 ACL 规则</div></template>
      </DokTable>
    </v-card>

    </div>

    <div v-show="tab === 'chains'" class="acl-panel">
    <v-card v-if="chains.length" class="acl-table-card">
      <div class="acl-table-head"><span>规则链</span><span class="acl-count">{{ chains.length }} 条链</span></div>
      <DokTable :headers="chainHeaders" :items="chains" item-value="name" :items-per-page="20" hover>
        <template #item.chain_type="{ item }">{{ chainTypeLabel(item.chain_type) }}</template>
        <template #item.default_action="{ item }">{{ actionLabel(item.default_action) }}</template>
        <template #item.enabled="{ item }">
          <v-switch v-model="item.enabled" density="compact" hide-details color="primary" />
        </template>
        <template #item.actions="{ item }">
          <v-btn size="x-small" variant="text" color="error" @click="removeChain(item)">删除</v-btn>
        </template>
      </DokTable>
    </v-card>

    </div>

    <v-dialog v-model="chainDialog" max-width="460">
      <v-card class="pa-2">
        <v-card-title>添加规则链</v-card-title>
        <v-card-text class="d-flex flex-column gap-3">
          <v-text-field v-model="chainForm.name" label="链名" placeholder="inbound" />
          <v-select v-model="chainForm.chain_type" :items="chainTypes" item-title="title" item-value="value" label="类型" />
          <v-select v-model="chainForm.default_action" :items="actions" item-title="title" item-value="value" label="默认动作" />
        </v-card-text>
        <v-card-actions><v-spacer /><v-btn variant="text" @click="chainDialog = false">取消</v-btn><v-btn color="primary" @click="addChain">添加</v-btn></v-card-actions>
      </v-card>
    </v-dialog>

    <v-dialog v-model="ruleDialog" max-width="560">
      <v-card class="pa-2">
        <v-card-title>添加 ACL 规则</v-card-title>
        <v-card-text class="d-flex flex-column gap-3">
          <v-select v-model="ruleForm.chain" :items="chains" item-title="name" item-value="name" label="规则链" />
          <v-text-field v-model="ruleForm.name" label="规则名" placeholder="allow-web" />
          <v-row dense>
            <v-col cols="6"><v-select v-model="ruleForm.action" :items="actions" item-title="title" item-value="value" label="动作" /></v-col>
            <v-col cols="6"><v-select v-model="ruleForm.protocol" :items="protocols" item-title="title" item-value="value" label="协议" /></v-col>
          </v-row>
          <v-row dense>
            <v-col cols="6"><v-text-field v-model="ruleForm.source" label="源 CIDR" placeholder="10.0.0.0/8, 0.0.0.0/0" /></v-col>
            <v-col cols="6"><v-text-field v-model="ruleForm.destination" label="目的 CIDR" placeholder="10.144.0.3/32" /></v-col>
          </v-row>
          <v-row dense>
            <v-col cols="6"><v-text-field v-model="ruleForm.ports" label="端口" placeholder="80, 443, 1000-2000" /></v-col>
            <v-col cols="6"><v-text-field v-model.number="ruleForm.priority" type="number" label="优先级" /></v-col>
          </v-row>
        </v-card-text>
        <v-card-actions><v-spacer /><v-btn variant="text" @click="ruleDialog = false">取消</v-btn><v-btn color="primary" @click="addRule">添加</v-btn></v-card-actions>
      </v-card>
    </v-dialog>

    <v-snackbar v-model="snack" :color="snackColor">{{ snackText }}</v-snackbar>
  </div>
</template>

<script setup>
import { computed, onMounted, ref, watch } from 'vue'
import { api } from '@/api'
import DokTable from '@/components/DokTable.vue'

const chains = ref([])
const tab = ref('rules')
const selectedChain = ref('')
const saving = ref(false)
const snack = ref(false)
const snackText = ref('')
const snackColor = ref('success')

const chainTypes = [
  { title: '入站 Inbound', value: 1 },
  { title: '出站 Outbound', value: 2 },
  { title: '转发 Forward', value: 3 },
]
const actions = [
  { title: '放行', value: 1 },
  { title: '丢弃', value: 2 },
  { title: '不处理', value: 0 },
]
const protocols = [
  { title: 'TCP', value: 1 },
  { title: 'UDP', value: 2 },
  { title: 'ICMP', value: 3 },
  { title: 'ICMPv6', value: 4 },
  { title: '任意', value: 5 },
]

const headers = [
  { title: '动作', key: 'action' },
  { title: '协议', key: 'protocol' },
  { title: '源 CIDR', key: 'source' },
  { title: '目的 CIDR', key: 'destination' },
  { title: '端口', key: 'ports' },
  { title: '优先级', key: 'priority' },
  { title: '启用', key: 'enabled', sortable: false },
  { title: '操作', key: 'actions', sortable: false },
]
const chainNames = computed(() => chains.value.map((chain) => chain.name))
const chainHeaders = [
  { title: '链名', key: 'name' },
  { title: '类型', key: 'chain_type' },
  { title: '默认动作', key: 'default_action' },
  { title: '启用', key: 'enabled', sortable: false },
  { title: '操作', key: 'actions', sortable: false },
]

const allRules = computed(() => chains.value.flatMap((chain) =>
  (chain.rules || []).map((rule, index) => ({
    ...rule,
    key: `${chain.name}-${index}`,
    chain_name: chain.name,
    chain_ref: chain,
    index,
  })),
))
const rules = computed(() => (selectedChain.value
  ? allRules.value.filter((rule) => rule.chain_name === selectedChain.value)
  : allRules.value))

const chainDialog = ref(false)
const chainForm = ref({ name: 'inbound', chain_type: 1, default_action: 1 })
const ruleDialog = ref(false)
const ruleForm = ref({ chain: 'inbound', name: '', action: 1, protocol: 1, source: '', destination: '', ports: '', priority: 100 })

watch(chainNames, (names) => {
  if (!names.length) {
    selectedChain.value = ''
    return
  }
  if (!names.includes(selectedChain.value)) selectedChain.value = names[0]
}, { immediate: true })

function actionLabel(value) {
  return actions.find((item) => item.value === value)?.title || String(value ?? '-')
}
function protocolLabel(value) {
  return protocols.find((item) => item.value === value)?.title || String(value ?? '-')
}
function chainTypeLabel(value) {
  return chainTypes.find((item) => item.value === value)?.title || String(value ?? '-')
}

async function load() {
  try {
    const data = await api.boardAcl()
    chains.value = (data.chains || []).map((chain) => ({
      ...chain,
      enabled: chain.enabled !== false,
      rules: (chain.rules || []).map((rule) => ({ ...rule, enabled: rule.enabled !== false })),
    }))
  } catch {}
}

function openChain() {
  chainForm.value = { name: 'inbound', chain_type: 1, default_action: 1 }
  chainDialog.value = true
}
function addChain() {
  const name = (chainForm.value.name || '').trim()
  if (!name) return
  chains.value.push({ name, chain_type: chainForm.value.chain_type, default_action: chainForm.value.default_action, enabled: true, rules: [] })
  selectedChain.value = name
  chainDialog.value = false
}
function removeChain(chain) {
  chains.value = chains.value.filter((item) => item !== chain)
}

function openRule() {
  if (!chains.value.length) return
  ruleForm.value = { chain: selectedChain.value || chains.value[0].name, name: '', action: 1, protocol: 1, source: '', destination: '', ports: '', priority: 100 }
  ruleDialog.value = true
}
function splitList(text) {
  return String(text || '').split(',').map((value) => value.trim()).filter(Boolean)
}
function addRule() {
  const chain = chains.value.find((item) => item.name === ruleForm.value.chain)
  if (!chain) return
  chain.rules = chain.rules || []
  chain.rules.push({
    name: ruleForm.value.name || `rule-${chain.rules.length + 1}`,
    action: ruleForm.value.action,
    protocol: ruleForm.value.protocol,
    source_ips: splitList(ruleForm.value.source),
    destination_ips: splitList(ruleForm.value.destination),
    ports: splitList(ruleForm.value.ports),
    priority: Number(ruleForm.value.priority) || 100,
    enabled: true,
  })
  ruleDialog.value = false
}
function removeRule(row) {
  const chain = chains.value.find((item) => item.name === row.chain_name)
  if (chain) chain.rules.splice(row.index, 1)
}
function setRuleEnabled(row, value) {
  const chain = chains.value.find((item) => item.name === row.chain_name)
  if (chain) chain.rules[row.index].enabled = value
}

async function save() {
  saving.value = true
  try {
    const payload = chains.value.map((chain) => ({
      name: chain.name,
      chain_type: chain.chain_type,
      default_action: chain.default_action,
      enabled: chain.enabled !== false,
      rules: (chain.rules || []).map((rule) => ({
        name: rule.name,
        action: rule.action,
        protocol: rule.protocol,
        priority: rule.priority ?? 100,
        enabled: rule.enabled !== false,
        source_ips: rule.source_ips || [],
        destination_ips: rule.destination_ips || [],
        ports: rule.ports || [],
      })),
    }))
    const res = await api.setBoardAcl(payload)
    if (!res.ok) throw new Error(res.error || 'save failed')
    snackText.value = 'ACL 已转换并下发到所有节点'
    snackColor.value = 'success'
    snack.value = true
  } catch (error) {
    snackText.value = String(error.message || error)
    snackColor.value = 'error'
    snack.value = true
  } finally {
    saving.value = false
  }
}

onMounted(load)
</script>

<style scoped>
.acl-page { width: 100%; }
.acl-head { display: flex; align-items: flex-start; justify-content: space-between; gap: 16px; margin-bottom: 18px; flex-wrap: wrap; }
.acl-actions { display: flex; align-items: center; gap: 10px; flex-wrap: wrap; }
.acl-cards { display: grid; grid-template-columns: repeat(4, minmax(0, 1fr)); gap: 12px; margin-bottom: 16px; }
.acl-stat { padding: 16px 18px; }
.acl-table-card { overflow: hidden; }
.acl-table-head { display: flex; align-items: center; justify-content: space-between; padding: 14px 18px; border-bottom: 1px solid rgba(var(--v-theme-on-surface), .08); font-size: 14px; font-weight: 600; }
.acl-count { font-size: 12px; font-weight: 500; color: rgba(var(--v-theme-on-surface), .55); }
.net-chip { display: inline-flex; align-items: center; padding: 2px 8px; border-radius: 6px; font-size: 11.5px; font-weight: 600; color: rgb(var(--v-theme-primary)); background: rgba(var(--v-theme-primary), .08); }
.num { font-variant-numeric: tabular-nums; }
.acl-tabs :deep(.v-tab) { text-transform: none; letter-spacing: 0; font-weight: 600; }
.acl-panel { min-width: 0; }
.acl-rules-head { display: flex; align-items: center; gap: 12px; margin-bottom: 12px; flex-wrap: wrap; }
.acl-chain-select { display: flex; align-items: center; gap: 10px; width: 280px; }
.acl-chain-label { font-size: 13px; font-weight: 600; white-space: nowrap; }
.acl-empty { text-align: center; color: rgba(var(--v-theme-on-surface), .5); padding: 24px 0; font-size: 13px; }
@media (max-width: 900px) { .acl-cards { grid-template-columns: repeat(2, minmax(0, 1fr)); } }
</style>
