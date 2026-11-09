<template>
  <div class="settings-page">
    <header class="settings-page__header">
      <h1 class="settings-page__title">系统设置</h1>
    </header>

    <div class="settings-layout">
      <nav class="settings-nav">
        <button
          v-for="item in sections"
          :key="item.id"
          type="button"
          class="settings-nav__item"
          :class="{ 'settings-nav__item--active': activeSection === item.id }"
          @click="activeSection = item.id"
        >
          <span class="settings-nav__icon"><v-icon size="15">{{ item.icon }}</v-icon></span>
          <span>{{ item.label }}</span>
        </button>
      </nav>

      <main class="settings-main">
        <section v-show="activeSection === 'routing'" class="settings-section">
          <div class="settings-section__heading">
            <div class="settings-section__title">路由决策</div>
          </div>
          <div class="system-config-group">
            <div class="system-config-group__heading">
              <div class="system-config-group__title">权重与代价参数</div>
              
            </div>
            <div class="settings-group-card config-form">
              <div v-for="field in routingFields" :key="field.key" class="config-row">
                <div class="property-info">
                  <div class="property-name">{{ field.label }}</div>
                  <div v-if="field.hint" class="property-hint">{{ field.hint }}</div>
                </div>
                <div class="config-input">
                  <v-text-field v-model.number="routing[field.key]" type="number" density="compact" variant="outlined" hide-details />
                </div>
              </div>
              <div class="config-row">
                <div class="property-info"><div class="property-name">启用加权选路</div><div class="property-hint">关闭后按最少跳数</div></div>
                <div class="config-input config-input--switch"><v-switch v-model="routing.weighted" color="primary" density="compact" hide-details /></div>
              </div>
            </div>
          </div>
        </section>

        <section v-show="activeSection === 'probe'" class="settings-section">
          <div class="settings-section__heading">
            <div class="settings-section__title">主动带宽测量</div>
          </div>
          <div class="system-config-group">
            <div class="system-config-group__heading">
              <div class="system-config-group__title">Burst 探测参数</div>
              
            </div>
            <div class="settings-group-card config-form">
              <div class="config-row">
                <div class="property-info"><div class="property-name">启用主动 burst 探测</div><div class="property-hint">只在空闲链路上限频执行，单链路约 32 KB/小时</div></div>
                <div class="config-input config-input--switch"><v-switch v-model="routing.probe_enabled" color="primary" density="compact" hide-details /></div>
              </div>
              <div v-for="field in probeFields" :key="field.key" class="config-row">
                <div class="property-info">
                  <div class="property-name">{{ field.label }}</div>
                  <div v-if="field.hint" class="property-hint">{{ field.hint }}</div>
                </div>
                <div class="config-input">
                  <v-text-field v-model.number="routing[field.key]" type="number" density="compact" variant="outlined" hide-details />
                </div>
              </div>
            </div>
          </div>
        </section>

        <section v-show="activeSection === 'geo'" class="settings-section">
          <div class="settings-section__heading">
            <div class="settings-section__title">IP 地理位置</div>
          </div>
          <div class="system-config-group">
            <div class="system-config-group__heading">
              <div class="system-config-group__title">解析服务</div>
            </div>
            <div class="settings-group-card config-form">
              <div class="config-row">
                <div class="property-info"><div class="property-name">IP 地理位置 API</div><div class="property-hint">节点国家/地区旗帜使用的解析服务</div></div>
                <div class="config-input"><v-select v-model="geoProvider" :items="geoOptions" density="compact" variant="outlined" hide-details /></div>
              </div>
            </div>
          </div>
        </section>
        <section v-show="activeSection === 'security'" class="settings-section">
          <div class="settings-section__heading"><div class="settings-section__title">安全</div></div>
          <div class="system-config-group">
            <div class="system-config-group__heading"><div class="system-config-group__title">认证与访问</div></div>
            <div class="settings-group-card config-form">
              <div class="config-row"><div class="property-info"><div class="property-name">信任代理请求头</div><div class="property-hint">启用后从 X-Forwarded-For / X-Real-IP 获取客户端 IP；关闭则仅使用连接地址。</div></div><div class="config-input config-input--switch"><v-switch v-model="security.trust_proxy" color="primary" density="compact" hide-details /></div></div>
              <div class="config-row"><div class="property-info"><div class="property-name">登录速率限制</div><div class="property-hint">关闭后不再限制登录、TOTP 等身份验证请求频率。</div></div><div class="config-input config-input--switch"><v-switch v-model="security.rate_limit" color="primary" density="compact" hide-details /></div></div>
              <div class="config-row"><div class="property-info"><div class="property-name">WebUI TOTP 双因素认证</div><div class="property-hint">启用后登录 WebUI 需要额外输入验证码。</div></div><div class="config-input config-input--switch"><v-btn size="small" :color="security.totp ? 'error' : 'primary'" variant="tonal" @click="security.totp ? disableTotp() : startTotp()">{{ security.totp ? '关闭 TOTP' : '绑定认证器' }}</v-btn></div></div>
            </div>
          </div>
        </section>
        <section v-show="activeSection === 'account'" class="settings-section">
          <div class="settings-section__heading">
            <div class="settings-section__title">账户</div>
          </div>
          <div class="system-config-group">
            <div class="system-config-group__heading">
              <div class="system-config-group__title">登录账户</div>
              <span v-if="accountLoaded" class="config-saved">当前用户：{{ account.username }}</span>
            </div>
            <div class="settings-group-card config-form">
              <div class="config-row">
                <div class="property-info"><div class="property-name">当前密码</div><div class="property-hint">修改用户名或密码前需要验证当前密码。</div></div>
                <div class="config-input"><v-text-field v-model="accountForm.current_password" type="password" density="compact" variant="outlined" hide-details autocomplete="current-password" /></div>
              </div>
              <div class="config-row">
                <div class="property-info"><div class="property-name">新用户名</div><div class="property-hint">留空表示不修改；仅支持字母、数字、-、_、.。</div></div>
                <div class="config-input"><v-text-field v-model="accountForm.new_username" density="compact" variant="outlined" hide-details :placeholder="account.username" autocomplete="username" /></div>
              </div>
              <div class="config-row">
                <div class="property-info"><div class="property-name">新密码</div><div class="property-hint">留空表示不修改；至少 6 位。</div></div>
                <div class="config-input"><v-text-field v-model="accountForm.new_password" type="password" density="compact" variant="outlined" hide-details autocomplete="new-password" /></div>
              </div>
              <div class="config-row">
                <div class="property-info"><div class="property-name">确认新密码</div></div>
                <div class="config-input"><v-text-field v-model="accountForm.confirm_password" type="password" density="compact" variant="outlined" hide-details autocomplete="new-password" /></div>
              </div>
              <div class="config-row">
                <div class="property-info"><div class="property-name">保存</div><div class="property-hint">修改成功后当前会话保持登录。</div></div>
                <div class="config-input"><v-btn size="small" color="primary" :loading="savingAccount" @click="saveAccount">保存账户设置</v-btn></div>
              </div>
            </div>
          </div>
        </section>
        <section v-show="activeSection === 'perf'" class="settings-section">
          <div class="settings-section__heading">
            <div class="settings-section__title">性能配置</div>
          </div>
          <div class="system-config-group">
            <div class="system-config-group__heading">
              <div class="system-config-group__title">运行状态</div>
              <span class="config-saved">每 10 秒刷新</span>
            </div>
            <div class="settings-group-card config-form">
              <div class="config-row">
                <div class="property-info"><div class="property-name">数据库类型</div><div class="property-hint">console 当前连接的后端</div></div>
                <div class="config-input">{{ perf.db_kind || '-' }}</div>
              </div>
              <div class="config-row">
                <div class="property-info"><div class="property-name">数据库大小</div><div class="property-hint">当前数据库占用空间</div></div>
                <div class="config-input">{{ perf.db_size != null ? humanBytes(perf.db_size) : '-' }}</div>
              </div>
              <div class="config-row">
                <div class="property-info"><div class="property-name">console 进程内存</div><div class="property-hint">RSS 常驻内存</div></div>
                <div class="config-input">{{ perf.rss_bytes != null ? humanBytes(perf.rss_bytes) : '-' }}</div>
              </div>
              <div class="config-row">
                <div class="property-info"><div class="property-name">console 运行时长</div></div>
                <div class="config-input">{{ humanDuration(perf.uptime_secs || 0) }}</div>
              </div>
              <div class="config-row">
                <div class="property-info"><div class="property-name">节点数</div></div>
                <div class="config-input">{{ perf.nodes ?? '-' }}</div>
              </div>
              <div class="config-row">
                <div class="property-info"><div class="property-name">配置版本数</div></div>
                <div class="config-input">{{ perf.config_versions ?? '-' }}</div>
              </div>
              <div class="config-row">
                <div class="property-info"><div class="property-name">链路指标行数</div></div>
                <div class="config-input">{{ perf.link_metrics ?? '-' }}</div>
              </div>
              <div class="config-row">
                <div class="property-info"><div class="property-name">审计日志行数</div></div>
                <div class="config-input">{{ perf.audit_logs ?? '-' }}</div>
              </div>
              <div class="config-row">
                <div class="property-info"><div class="property-name">配置漂移节点</div><div class="property-hint">节点上报的实例与期望配置不一致的数量</div></div>
                <div class="config-input">{{ driftCount }}</div>
              </div>
              <div class="config-row">
                <div class="property-info"><div class="property-name">实测链路 / 未测量</div><div class="property-hint">容量来源统计，实测由 burst 探测产生</div></div>
                <div class="config-input">{{ linkStats.probe }} / {{ linkStats.unmeasured }}</div>
              </div>
              <div class="config-row">
                <div class="property-info"><div class="property-name">实测容量中位数</div></div>
                <div class="config-input">{{ linkStats.median ? linkStats.median.toFixed(0) + ' Mbps' : '-' }}</div>
              </div>
            </div>
          </div>
        </section>
      </main>
    </div>

    <v-dialog v-model="disableDialog" max-width="420">
      <v-card class="pa-2">
        <v-card-title>关闭 TOTP</v-card-title>
        <v-card-text class="d-flex flex-column gap-3">
          <div class="text-caption">请输入当前认证器中的 6 位验证码确认关闭。</div>
          <v-text-field v-model="disableCode" label="验证码" maxlength="6" inputmode="numeric" />
        </v-card-text>
        <v-card-actions><v-spacer /><v-btn variant="text" @click="disableDialog = false">取消</v-btn><v-btn color="error" @click="confirmDisableTotp">关闭 TOTP</v-btn></v-card-actions>
      </v-card>
    </v-dialog>
    <v-dialog v-model="totpDialog" max-width="440">
      <v-card class="pa-2">
        <v-card-title>绑定 TOTP 认证器</v-card-title>
        <v-card-text class="d-flex flex-column gap-3">
          <div class="text-caption">用认证器扫描二维码（SHA256 / 6 位 / 30 秒）：</div>
          <div class="totp-qr"><img v-if="totpQr" :src="totpQr" alt="TOTP QR" width="200" height="200" /></div>
          <div class="text-caption">无法扫码时手动输入密钥：</div>
          <code style="word-break:break-all">{{ totpSecret }}</code>
          <v-text-field v-model="totpCode" label="输入 6 位验证码" maxlength="6" />
        </v-card-text>
        <v-card-actions><v-spacer /><v-btn variant="text" @click="totpDialog = false">取消</v-btn><v-btn color="primary" @click="confirmTotp">确认绑定</v-btn></v-card-actions>
      </v-card>
    </v-dialog>
    <v-snackbar v-model="snack" :color="snackColor">{{ snackText }}</v-snackbar>
  </div>
</template>

<script setup>
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { api } from '@/api'
import ConfigField from '@/components/ConfigField.vue'

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
const geoProvider = ref('ipwho.is')
const geoOptions = ['ipwho.is', 'ip.sb', 'ipinfo.io', 'ipapi.co']
const savingGeo = ref(false)
const savingRouting = ref(false)
const account = ref({ username: '' })
const accountLoaded = ref(false)
const accountForm = ref({ current_password: '', new_username: '', new_password: '', confirm_password: '' })
const savingAccount = ref(false)
const activeSection = ref('routing')
const sections = [
  { id: 'routing', label: '路由决策', icon: 'mdi-routes' },
  { id: 'probe', label: '主动测量', icon: 'mdi-speedometer' },
  { id: 'geo', label: '地理位置', icon: 'mdi-map-marker' },
  { id: 'security', label: '安全', icon: 'mdi-shield-lock' },
  { id: 'account', label: '账户', icon: 'mdi-account-cog' },
  { id: 'perf', label: '性能配置', icon: 'mdi-chart-box' },
]
const savedHint = ref('')
const settingsLoaded = ref(false)
const perf = ref({})
const driftCount = ref(0)
const security = ref({ trust_proxy: true, rate_limit: true, totp: false })
const totpDialog = ref(false)
const totpSecret = ref('')
const totpQr = ref('')
const totpCode = ref('')
const disableDialog = ref(false)
const disableCode = ref('')
const linkStats = ref({ probe: 0, declared: 0, reference: 0, median: null })
let perfTimer = null
let autoSaveTimer = null
const routing = ref({
  latency_weight: 1, loss_weight: 20, bandwidth_weight: 0.5, reference_bandwidth_mbps: 100,
  utilization_weight: 10, hop_weight: 0.05, loss_gain: 1,
  tcp_loss_weight: 1, udp_loss_weight: 1, kcp_loss_weight: 1, quic_loss_weight: 1, 
  min_bandwidth_mbps: 0, weighted: true,
  probe_enabled: true, probe_interval_secs: 3600, probe_payload_bytes: 524288,
  probe_repeats: 2, probe_max_concurrent: 2, probe_idle_utilization_pct: 20,
  probe_timeout_ms: 5000, probe_max_range_mbps: 5000,
})
const probeFields = [
  { key: 'probe_interval_secs', label: '测量间隔（秒）', hint: '同一链路两次探测的最小间隔，默认 3600' },
  { key: 'probe_payload_bytes', label: '单次 payload（字节）', hint: '一次 burst 的响应大小，默认 65536' },
  { key: 'probe_repeats', label: '重复次数', hint: '多次采样取中位数，默认 2' },
  { key: 'probe_max_concurrent', label: '最大并发', hint: '每轮最多同时探测的链路数，默认 2' },
  { key: 'probe_idle_utilization_pct', label: '空闲阈值（%）', hint: '链路利用率高于该值时跳过探测，默认 20' },
  { key: 'probe_timeout_ms', label: '超时（毫秒）', hint: '单次探测 RPC 超时，默认 5000' },
  { key: 'probe_max_range_mbps', label: '量程上限（Mbps）', hint: '超过该值按上限截断，默认 5000' },
]
const routingFields = [
  { key: 'latency_weight', label: '延迟权重', hint: '延迟对代价的影响，越大越偏好低延迟路径' },
  { key: 'loss_weight', label: '丢包权重', hint: '丢包项权重，按 -ln(1-loss) 计算' },
  { key: 'bandwidth_weight', label: '带宽权重', hint: '带宽项权重，参考带宽 / 链路容量' },
  { key: 'reference_bandwidth_mbps', label: '参考带宽（Mbps）', hint: '带宽代价的参考值，同时作为无实测/无声明时的回退容量' },
  { key: 'utilization_weight', label: '利用率权重', hint: '对链路利用率的平方惩罚，避免压满链路' },
  { key: 'hop_weight', label: '每跳权重', hint: '每多一跳增加的固定代价' },
  { key: 'loss_gain', label: '丢包放大系数', hint: '整体放大丢包项，默认 1.0' },
  { key: 'min_bandwidth_mbps', label: '最小可用带宽（Mbps）', hint: '低于该容量的链路直接淘汰，0 表示不限制' },
  { key: 'tcp_loss_weight', label: 'TCP 丢包权重', hint: 'TCP 重传率在评分中的权重' },
  { key: 'udp_loss_weight', label: 'UDP 丢包权重', hint: 'UDP ping 超时率的权重' },
  { key: 'kcp_loss_weight', label: 'KCP 丢包权重', hint: 'KCP ping 超时率的权重' },
  { key: 'quic_loss_weight', label: 'QUIC 丢包权重', hint: 'QUIC lost/sent 比例的权重' },
]
const lastUpdated = computed(() => {
  const ts = summary.value.last_updated
  return ts ? new Date(ts * 1000).toLocaleString() : '-'
})

watch(routing, () => {
  if (!settingsLoaded.value) return
  clearTimeout(autoSaveTimer)
  autoSaveTimer = setTimeout(() => saveRouting(), 800)
}, { deep: true })

async function loadAccount() {
  try {
    const res = await api.account()
    account.value = { username: res.username || '' }
    accountLoaded.value = true
  } catch {}
}
async function saveAccount() {
  if (!accountForm.value.current_password) {
    snackText.value = '请先填写当前密码'
    snackColor.value = 'error'
    snack.value = true
    return
  }
  if (accountForm.value.new_password && accountForm.value.new_password !== accountForm.value.confirm_password) {
    snackText.value = '两次输入的新密码不一致'
    snackColor.value = 'error'
    snack.value = true
    return
  }
  if (!accountForm.value.new_username.trim() && !accountForm.value.new_password) {
    snackText.value = '没有需要修改的内容'
    snackColor.value = 'error'
    snack.value = true
    return
  }
  savingAccount.value = true
  try {
    const body = { current_password: accountForm.value.current_password }
    if (accountForm.value.new_username.trim()) body.new_username = accountForm.value.new_username.trim()
    if (accountForm.value.new_password) body.new_password = accountForm.value.new_password
    const res = await api.updateAccount(body)
    if (!res.ok) throw new Error(res.error || '保存失败')
    account.value = { username: res.username || account.value.username }
    accountForm.value = { current_password: '', new_username: '', new_password: '', confirm_password: '' }
    snackText.value = '账户已更新'
    snackColor.value = 'success'
    snack.value = true
  } catch (err) {
    snackText.value = String(err.message || err)
    snackColor.value = 'error'
    snack.value = true
  } finally {
    savingAccount.value = false
  }
}
async function startTotp() {
  try {
    const res = await api.totpSetup()
    totpSecret.value = res.secret
    totpQr.value = `https://api.qrserver.com/v1/create-qr-code/?size=200x200&data=${encodeURIComponent(res.url || '')}`
    totpCode.value = ''
    totpDialog.value = true
  } catch {}
}
async function confirmTotp() {
  try {
    const res = await api.totpVerify(totpCode.value)
    if (!res.ok) throw new Error(res.error)
    security.value.totp = true
    totpDialog.value = false
    snackText.value = 'TOTP 已启用'
    snackColor.value = 'success'
    snack.value = true
  } catch (e) {
    snackText.value = String(e.message || e)
    snackColor.value = 'error'
    snack.value = true
  }
}
function disableTotp() {
  disableCode.value = ''
  disableDialog.value = true
}
async function confirmDisableTotp() {
  try {
    const res = await api.totpDisable(disableCode.value)
    if (!res.ok) throw new Error(res.error || 'invalid code')
    security.value.totp = false
    disableDialog.value = false
    snackText.value = 'TOTP 已关闭'
    snackColor.value = 'success'
    snack.value = true
  } catch (e) {
    snackText.value = String(e.message || e)
    snackColor.value = 'error'
    snack.value = true
  }
}

watch(security, async () => {
  if (!settingsLoaded.value) return
  try {
    await api.setBoardSecurity(security.value)
  } catch {}
}, { deep: true })

watch(geoProvider, () => {
  if (!settingsLoaded.value) return
  clearTimeout(autoSaveTimer)
  autoSaveTimer = setTimeout(() => saveGeo(), 800)
})

async function loadPerf() {
  try {
    const data = await api.boardPerf()
    perf.value = data || {}
  } catch {}
  try {
    const drift = await api.boardDrift()
    driftCount.value = drift.count || 0
  } catch {}
  try {
    const data = await api.boardNodes()
    const nodes = data.nodes || []
    const stats = { probe: 0, unmeasured: 0 }
    const measured = []
    for (const node of nodes) {
      for (const instance of (node.snapshot && node.snapshot.instances) || []) {
        for (const edge of (instance.topology && instance.topology.edges) || []) {
          const tunnel = (edge.tunnels || [])[0]
          if (!tunnel) continue
          const source = tunnel.capacity_source
          if (source === 'probe') {
            stats.probe += 1
            if (tunnel.capacity_mbps) measured.push(Number(tunnel.capacity_mbps))
          } else {
            stats.unmeasured += 1
          }
        }
      }
    }
    measured.sort((a, b) => a - b)
    linkStats.value = { ...stats, median: measured.length ? measured[Math.floor(measured.length / 2)] : null }
  } catch {}
}

onMounted(async () => {
  loadAccount()
  try {
    const geo = await api.boardGeoSetting()
    if (geo.provider) geoProvider.value = geo.provider
  } catch {}
  try {
    const params = await api.boardRoutingSettings()
    routing.value = { ...routing.value, ...params }
  } catch {}
  try {
    const sec = await api.boardSecurity()
    security.value = { trust_proxy: sec.trust_proxy !== false, rate_limit: sec.rate_limit !== false, totp: sec.totp === true }
  } catch {}
  await nextTick()
  settingsLoaded.value = true
  loadPerf()
  perfTimer = setInterval(loadPerf, 10000)
})

onBeforeUnmount(() => {
  if (perfTimer) clearInterval(perfTimer)
})

function humanBytes(value) {
  if (value == null) return '-'
  const units = ['B', 'KB', 'MB', 'GB', 'TB']
  let size = Number(value)
  let index = 0
  while (size >= 1024 && index < units.length - 1) {
    size /= 1024
    index += 1
  }
  return `${size.toFixed(index === 0 ? 0 : 1)} ${units[index]}`
}

function humanDuration(seconds) {
  const total = Math.max(0, Number(seconds) || 0)
  const days = Math.floor(total / 86400)
  const hours = Math.floor((total % 86400) / 3600)
  const minutes = Math.floor((total % 3600) / 60)
  if (days > 0) return `${days} 天 ${hours} 小时`
  if (hours > 0) return `${hours} 小时 ${minutes} 分`
  return `${minutes} 分 ${Math.floor(total % 60)} 秒`
}


async function saveRouting() {
  savingRouting.value = true
  try {
    const res = await api.setBoardRouting(routing.value)
    if (!res.ok) throw new Error(res.error || 'save failed')
    snackText.value = '路由参数已下发'
    snackColor.value = 'success'
    snack.value = true
  } catch (e) {
    snackText.value = String(e.message || e)
    snackColor.value = 'error'
    snack.value = true
  } finally {
    savingRouting.value = false
  }
}

async function saveGeo() {
  savingGeo.value = true
  try {
    const res = await api.setBoardGeo(geoProvider.value)
    if (!res.ok) throw new Error(res.error || 'save failed')
    snackText.value = 'IP 地理位置 API 已切换'
    snackColor.value = 'success'
    snack.value = true
  } catch (e) {
    snackText.value = String(e.message || e)
    snackColor.value = 'error'
    snack.value = true
  } finally {
    savingGeo.value = false
  }
}

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
<style scoped>
.settings-page {
  --settings-border: rgba(17, 24, 39, 0.13);
  --settings-divider: rgba(17, 24, 39, 0.09);
  display: flex;
  flex-direction: column;
  width: min(100%, 940px);
  margin: 0 auto;
  padding: 36px 18px 8px;
}
.settings-page__header { margin-bottom: 28px; }
.settings-page__title { margin: 0; color: rgb(var(--v-theme-on-surface)); font-size: 1.72rem; font-weight: 780; line-height: 1.25; }
.settings-layout { display: grid; grid-template-columns: 126px minmax(0, 1fr); gap: 34px; align-items: start; }
.settings-nav { display: flex; flex-direction: column; gap: 5px; padding-top: 2px; }
.settings-nav__item {
  position: relative; display: flex; align-items: center; gap: 8px; width: 100%; min-height: 34px;
  padding: 6px 9px 6px 12px; border: 0; border-radius: 8px; background: transparent;
  color: rgba(var(--v-theme-on-surface), 0.64); cursor: pointer; font: inherit; font-size: 0.84rem;
  font-weight: 680; line-height: 1.25; text-align: left;
}
.settings-nav__item:hover { background: rgba(var(--v-theme-on-surface), 0.045); color: rgb(var(--v-theme-on-surface)); }
.settings-nav__item--active { background: rgba(var(--v-theme-on-surface), 0.07); color: rgb(var(--v-theme-on-surface)); font-weight: 760; }
.settings-nav__item--active::before {
  position: absolute; top: 8px; bottom: 8px; left: 0; width: 2px; border-radius: 999px;
  background: rgba(var(--v-theme-on-surface), 0.52); content: "";
}
.settings-nav__icon { width: 15px; display: flex; align-items: center; justify-content: center; }
.settings-section__heading { margin-bottom: 22px; }
.settings-section__title { color: rgb(var(--v-theme-on-surface)); font-size: 1.34rem; font-weight: 780; line-height: 1.25; }
.system-config-group { --config-form-divider: var(--settings-divider); margin-bottom: 30px; }
.system-config-group__heading { display: flex; align-items: center; justify-content: space-between; gap: 10px; margin-bottom: 12px; }
.system-config-group__title { color: rgb(var(--v-theme-on-surface)); font-size: 1.04rem; font-weight: 760; line-height: 1.32; }
.totp-qr { display: flex; justify-content: center; padding: 8px 0; }
.totp-qr img { border-radius: 10px; background: #fff; padding: 8px; }
.config-saved { font-size: 12px; color: rgba(var(--v-theme-on-surface), .55); }
.settings-group-card {
  overflow: hidden; border: 1px solid var(--settings-border); border-radius: 10px;
  background: rgb(var(--v-theme-surface)); box-shadow: none;
}
.config-row {
  display: grid; grid-template-columns: minmax(0, 1fr) minmax(180px, 34%);
  gap: 18px; align-items: center; min-height: 60px; padding: 12px 16px;
  border-bottom: 1px solid var(--settings-divider);
}
.config-row:last-child { border-bottom: 0; }
.property-info { min-width: 0; }
.property-name { color: rgb(var(--v-theme-on-surface)); font-size: 0.88rem; font-weight: 700; line-height: 1.4; }
.property-hint { margin-top: 4px; color: rgba(var(--v-theme-on-surface), 0.76); font-size: 0.78rem; line-height: 1.45; }
.config-input { display: flex; justify-content: flex-end; min-width: 0; }
.config-input > :deep(.v-input) { width: 100%; max-width: 270px; }
.config-input--switch > :deep(.v-input) { width: auto; max-width: none; margin-left: auto; }
.config-input :deep(.v-field) { border-radius: 10px; background: transparent; }
@media (max-width: 720px) {
  .settings-layout { grid-template-columns: 1fr; gap: 16px; }
  .settings-nav { flex-direction: row; flex-wrap: wrap; }
  .config-row { display: block; padding: 14px 16px; }
  .config-input { justify-content: stretch; padding-top: 10px; }
  .config-input > :deep(.v-input) { max-width: none; }
}
</style>
