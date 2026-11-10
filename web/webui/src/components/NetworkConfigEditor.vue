<template>
  <div class="net-editor">
    <div class="editor-head">
      <v-switch v-model="advanced" label="高级（TOML）" density="compact" hide-details color="primary" />
    </div>

    <v-alert v-if="showErrors && errors.length" type="error" variant="tonal" density="compact" class="mb-3 editor-errors">
      {{ errors[0] }}<span v-if="errors.length > 1">（共 {{ errors.length }} 项问题）</span>
    </v-alert>

    <v-textarea
      v-if="advanced"
      v-model="rawToml"
      class="mono"
      rows="18"
      variant="outlined"
      density="compact"
      spellcheck="false"
    />

    <template v-else>
      <div class="section-title">基础设置</div>
      <div class="cfg-grid">
        <ConfigField label="主机名" hint="留空默认使用系统主机名。示例：node-bj-01">
          <v-text-field v-model="form.hostname" density="compact" variant="outlined" hide-details placeholder="留空默认使用主机名" />
        </ConfigField>
        <ConfigField label="虚拟 IPv4" hint="CIDR 格式。示例：10.126.126.1/24；开启 DHCP 时留空">
          <v-text-field v-model="form.ipv4" density="compact" variant="outlined" hide-details placeholder="10.126.126.1/24" :disabled="form.dhcp_mode !== 'off'" />
        </ConfigField>
        <ConfigField label="网络名称" hint="同一虚拟网络的标识。示例：easytier-lab">
          <v-text-field v-model="form.network_name" density="compact" variant="outlined" hide-details placeholder="easytier" />
        </ConfigField>
        <ConfigField label="网络密码" hint="网络密钥，加入同一网络必须一致。示例：s3cret">
          <v-text-field v-model="form.network_secret" density="compact" variant="outlined" hide-details placeholder="Enter a password" type="password" />
        </ConfigField>
        <ConfigField label="p2p_group" hint="P2P 隔离组，同组才允许自动直连。示例：cn">
          <v-text-field v-model="form.p2p_group" density="compact" variant="outlined" hide-details placeholder="cn" />
        </ConfigField>
        <ConfigField label="DHCP" hint="关闭=使用静态 IPv4；自动=按 /24 分配；指定网段=任意 CIDR（如 10.77.0.0/16，前缀需 ≤30）">
          <v-select v-model="form.dhcp_mode" :items="dhcpModes" item-title="title" item-value="value" density="compact" variant="outlined" hide-details />
        </ConfigField>
        <ConfigField v-if="form.dhcp_mode === 'cidr'" label="DHCP 网段" hint="任意 IPv4 CIDR。示例：10.77.0.0/16">
          <v-text-field v-model="form.dhcp_cidr" density="compact" variant="outlined" hide-details placeholder="10.77.0.0/16" />
        </ConfigField>
      </div>

      <div class="section-title section-sep">IPv6</div>
      <div class="cfg-grid">
        <ConfigField label="虚拟 IPv6" hint="可选的虚拟 IPv6 地址（CIDR）。示例：fd00::1/64。">
          <v-text-field v-model="form.ipv6" density="compact" variant="outlined" hide-details placeholder="留空不启用 IPv6" />
        </ConfigField>
        <ConfigField label="公网 IPv6 前缀" hint="手动指定要共享的公网 IPv6 子网，不自动从系统路由检测。示例：2001:db8:1::/64。">
          <v-text-field v-model="form.ipv6_public_addr_prefix" density="compact" variant="outlined" hide-details placeholder="2001:db8:1::/64" :disabled="!form.ipv6_public_addr_auto" />
        </ConfigField>
      </div>
      <div class="flags-grid">
        <div class="flag-item">
          <div class="cfg-label-row">
            <span class="cfg-label">自动获取公网 IPv6</span>
            <v-tooltip location="top" max-width="360" open-delay="120">
              <template #activator="{ props }"><v-icon v-bind="props" icon="mdi-information-outline" size="14" class="cfg-info" /></template>
              <div class="cfg-tip">自动从共享了 IPv6 子网的对等节点获取一个公网 IPv6 地址。</div>
            </v-tooltip>
          </div>
          <v-switch v-model="form.ipv6_public_addr_auto" density="compact" hide-details color="primary" />
        </div>
        <div class="flag-item">
          <div class="cfg-label-row">
            <span class="cfg-label">作为 IPv6 提供节点</span>
            <v-tooltip location="top" max-width="360" open-delay="120">
              <template #activator="{ props }"><v-icon v-bind="props" icon="mdi-information-outline" size="14" class="cfg-info" /></template>
              <div class="cfg-tip">将此节点的公网 IPv6 子网共享给其他节点，使它们也能获得公网 IPv6 地址（仅 Linux 支持）。</div>
            </v-tooltip>
          </div>
          <v-switch v-model="form.ipv6_public_addr_provider" density="compact" hide-details color="primary" />
        </div>
      </div>

      <div class="section-title section-sep">
        初始节点
        <v-tooltip location="top" max-width="380" open-delay="120">
          <template #activator="{ props }"><v-icon v-bind="props" icon="mdi-information-outline" size="14" class="cfg-info" /></template>
          <div class="cfg-tip">对端地址同样先选协议再填地址。示例：tcp + 1.2.3.4:11010，或 udp + [2001:db8::1]:11010。</div>
        </v-tooltip>
      </div>
      <div v-for="(p, i) in form.peers" :key="'p' + i" class="row-item">
        <v-select v-model="p.proto" :items="protoOptions" density="compact" variant="outlined" hide-details class="proto-select" />
        <v-text-field v-model="p.addr" density="compact" variant="outlined" hide-details placeholder="1.2.3.4:11010" />
        <v-btn size="small" variant="text" icon="mdi-delete-outline" @click="form.peers.splice(i, 1)" />
      </div>
      <v-btn size="small" variant="text" prepend-icon="mdi-plus" @click="form.peers.push({ proto: 'tcp', addr: '' })">添加初始节点</v-btn>

      <div class="section-title section-sep">子网代理</div>
      <div v-for="(s, i) in form.proxy_network" :key="'s' + i" class="row-item">
        <v-text-field v-model="form.proxy_network[i]" density="compact" variant="outlined" hide-details placeholder="10.0.0.0/24" />
        <v-btn size="small" variant="text" icon="mdi-delete-outline" @click="form.proxy_network.splice(i, 1)" />
      </div>
      <v-btn size="small" variant="text" prepend-icon="mdi-plus" @click="form.proxy_network.push('')">添加子网代理</v-btn>

      <div class="section-title section-sep">路由与出口</div>
      <div class="cfg-grid">
        <ConfigField label="自定义路由" hint="每行一个 IPv4 CIDR，流量会走虚拟网络。示例：192.168.77.0/24">
          <v-textarea v-model="form.routes" rows="2" density="compact" variant="outlined" hide-details placeholder="192.168.77.0/24" />
        </ConfigField>
        <ConfigField label="出口节点列表" hint="转发所有流量的出口节点，虚拟 IPv4 地址，优先级由列表顺序决定。">
          <v-textarea v-model="form.exit_nodes" rows="2" density="compact" variant="outlined" hide-details placeholder="10.88.0.2" />
        </ConfigField>
        <ConfigField label="出站绑定网卡" hint="core 自身出站连接（隧道、探测）强制从该网卡走，避免被出口节点默认路由吸入隧道。留空走系统默认路由。">
          <v-text-field v-model="form.outbound_interface" density="compact" variant="outlined" hide-details placeholder="eth1" />
        </ConfigField>
      </div>

      <div class="section-title section-sep">端口转发</div>
      <div v-for="(pf, i) in form.port_forwards" :key="'pf' + i" class="forward-card">
        <div class="forward-row">
          <ConfigField label="协议"><v-select v-model="pf.proto" :items="['tcp', 'udp']" density="compact" variant="outlined" hide-details /></ConfigField>
          <ConfigField label="绑定地址"><v-text-field v-model="pf.bind_addr" density="compact" variant="outlined" hide-details placeholder="0.0.0.0" /></ConfigField>
          <ConfigField label="绑定端口"><v-text-field v-model.number="pf.bind_port" type="number" density="compact" variant="outlined" hide-details placeholder="65535" /></ConfigField>
          <v-btn size="small" variant="text" icon="mdi-delete-outline" class="forward-del" @click="form.port_forwards.splice(i, 1)" />
        </div>
        <div class="forward-row forward-row--target">
          <ConfigField label="目标地址"><v-text-field v-model="pf.dst_addr" density="compact" variant="outlined" hide-details placeholder="10.126.126.1" /></ConfigField>
          <ConfigField label="目标端口"><v-text-field v-model.number="pf.dst_port" type="number" density="compact" variant="outlined" hide-details placeholder="65535" /></ConfigField>
        </div>
      </div>
      <v-btn size="small" variant="text" prepend-icon="mdi-plus" @click="form.port_forwards.push({ proto: 'tcp', bind_addr: '0.0.0.0', bind_port: 65535, dst_addr: '', dst_port: 65535 })">添加端口转发</v-btn>

      <div class="section-title section-sep">访问控制与白名单</div>
      <div class="cfg-grid">
        <ConfigField label="TCP 白名单" hint="放行的目标端口，逗号分隔。示例：80,443,1000-2000">
          <v-text-field v-model="form.tcp_whitelist" density="compact" variant="outlined" hide-details placeholder="80, 443" />
        </ConfigField>
        <ConfigField label="UDP 白名单" hint="放行的目标端口，逗号分隔。示例：53,123">
          <v-text-field v-model="form.udp_whitelist" density="compact" variant="outlined" hide-details placeholder="53, 123" />
        </ConfigField>
        <ConfigField label="socks5 服务器" hint="启用 socks5 服务器，允许 socks5 客户端访问虚拟网络。格式：socks5://host:port。">
          <v-text-field v-model="form.socks5_proxy" density="compact" variant="outlined" hide-details placeholder="socks5://0.0.0.0:1080" />
        </ConfigField>
        <ConfigField label="VPN Portal" hint="开启 WireGuard 客户端接入，需填 WG 监听地址。示例：0.0.0.0:11011">
          <v-switch v-model="form.vpn_portal" density="compact" hide-details color="primary" />
        </ConfigField>
        <ConfigField v-if="form.vpn_portal" label="VPN Portal WG 监听" hint="WireGuard 监听地址。示例：0.0.0.0:11011">
          <v-text-field v-model="form.vpn_portal_listen" density="compact" variant="outlined" hide-details placeholder="0.0.0.0:11011" />
        </ConfigField>
      </div>

      <div class="section-title section-sep">VPN Portal 客户端</div>
      <div v-for="(c, i) in form.vpn_clients" :key="'vc' + i" class="forward-card">
        <div class="vpn-grid">
          <ConfigField label="名称"><v-text-field v-model="c.name" density="compact" variant="outlined" hide-details placeholder="laptop" /></ConfigField>
          <ConfigField label="虚拟 IP"><v-text-field v-model="c.virtual_ip" density="compact" variant="outlined" hide-details placeholder="10.60.0.2/24" /></ConfigField>
          <ConfigField label="组"><v-text-field v-model="c.groups" density="compact" variant="outlined" hide-details placeholder="group-a, group-b" /></ConfigField>
          <v-btn size="small" variant="text" icon="mdi-delete-outline" class="forward-del" @click="form.vpn_clients.splice(i, 1)" />
        </div>
      </div>
      <v-btn size="small" variant="text" prepend-icon="mdi-plus" @click="form.vpn_clients.push({ name: '', virtual_ip: '', groups: '' })">添加客户端</v-btn>

      <div class="section-title section-sep">TUN 与传输</div>
      <div class="cfg-grid">
        <ConfigField label="TUN 接口名称" hint="可选的 TUN 接口名称。留空自动生成；多个网络使用相同名称会在设置 IP 时冲突。">
          <v-text-field v-model="form.flags.dev_name" density="compact" variant="outlined" hide-details placeholder="留空自动生成" />
        </ConfigField>
        <ConfigField label="MTU" hint="TUN 设备的 MTU，默认非加密 1380、加密 1360。">
          <v-text-field v-model.number="form.flags.mtu" type="number" density="compact" variant="outlined" hide-details placeholder="1380" />
        </ConfigField>
        <ConfigField label="实例接收限速" hint="限制当前网络实例整体入站流量的总接收速率，默认无限制，单位 BPS（字节/秒）。">
          <v-text-field v-model.number="form.flags.instance_recv_bps_limit" type="number" density="compact" variant="outlined" hide-details placeholder="留空不限速" />
        </ConfigField>
        <ConfigField label="网络白名单" hint="仅转发白名单网络的流量，支持通配符，多个网络名用空格分隔；为空则禁用转发，默认允许所有（*）。">
          <v-text-field v-model="form.flags.relay_network_whitelist" density="compact" variant="outlined" hide-details placeholder="*" />
        </ConfigField>
        <ConfigField label="节点带宽（Mbps）" hint="节点的可用带宽，参与路由代价计算：带宽越低代价越高。留空使用默认参考带宽。示例：100">
          <v-text-field v-model.number="form.node_bandwidth_mbps" type="number" density="compact" variant="outlined" hide-details placeholder="100" />
        </ConfigField>
      </div>

      <div class="section-title section-sep">功能开关</div>
      <div class="flags-grid">
        <div v-for="f in toggles" :key="f.key" class="flag-item">
          <div class="cfg-label-row">
            <span class="cfg-label">{{ f.label }}</span>
            <v-tooltip location="top" max-width="360" open-delay="120">
              <template #activator="{ props }">
                <v-icon v-bind="props" icon="mdi-information-outline" size="14" class="cfg-info" />
              </template>
              <div class="cfg-tip" v-html="f.hint" />
            </v-tooltip>
          </div>
          <v-switch v-if="!f.invert" v-model="form.flags[f.key]" density="compact" hide-details color="primary" />
          <v-switch v-else :model-value="!form.flags[f.key]" density="compact" hide-details color="primary" @update:model-value="(v) => (form.flags[f.key] = !v)" />
        </div>
      </div>

      <div class="section-title section-sep">访问控制（ACL 规则链）</div>
      <div v-for="(chain, ci) in form.acl_chains" :key="'ch' + ci" class="acl-chain">
        <div class="acl-chain-grid">
          <ConfigField label="链名"><v-text-field v-model="chain.name" density="compact" variant="outlined" hide-details placeholder="inbound" /></ConfigField>
          <ConfigField label="类型"><v-select v-model="chain.type" :items="chainTypes" item-title="title" item-value="value" density="compact" variant="outlined" hide-details /></ConfigField>
          <ConfigField label="默认动作"><v-select v-model="chain.default_action" :items="actions" item-title="title" item-value="value" density="compact" variant="outlined" hide-details /></ConfigField>
          <v-btn size="small" variant="text" icon="mdi-delete-outline" class="forward-del" @click="form.acl_chains.splice(ci, 1)" />
        </div>
        <div v-for="(rule, ri) in chain.rules" :key="'r' + ri" class="acl-rule">
          <div class="acl-rule-top">
            <ConfigField label="动作"><v-select v-model="rule.action" :items="actions" item-title="title" item-value="value" density="compact" variant="outlined" hide-details /></ConfigField>
            <ConfigField label="协议"><v-select v-model="rule.protocol" :items="protocols" item-title="title" item-value="value" density="compact" variant="outlined" hide-details /></ConfigField>
            <ConfigField label="端口"><v-text-field v-model="rule.ports" density="compact" variant="outlined" hide-details placeholder="80, 443" /></ConfigField>
            <v-btn size="small" variant="text" icon="mdi-close" class="forward-del" @click="chain.rules.splice(ri, 1)" />
          </div>
          <div class="acl-rule-cidrs">
            <ConfigField label="源 CIDR"><v-text-field v-model="rule.source_ips" density="compact" variant="outlined" hide-details placeholder="10.0.0.0/8" /></ConfigField>
            <ConfigField label="目的 CIDR"><v-text-field v-model="rule.destination_ips" density="compact" variant="outlined" hide-details placeholder="10.88.0.3/32" /></ConfigField>
          </div>
        </div>
        <v-btn size="small" variant="text" prepend-icon="mdi-plus" @click="chain.rules.push({ action: 1, protocol: 5, source_ips: '', destination_ips: '', ports: '' })">添加规则</v-btn>
      </div>
      <v-btn size="small" variant="text" prepend-icon="mdi-plus" @click="form.acl_chains.push({ name: 'inbound', type: 1, default_action: 2, rules: [] })">添加规则链</v-btn>

      <div class="section-title section-sep">配置预览</div>
      <pre class="config-preview mono">{{ generated }}</pre>
    </template>
  </div>
</template>

<script setup>
import { computed, ref, watch } from 'vue'
import ConfigField from './ConfigField.vue'

const props = defineProps({
  toml: { type: String, default: '' },
})
const emit = defineEmits(['update:toml'])

const advanced = ref(false)
const showErrors = ref(false)
const rawToml = ref(props.toml)
let internal = false

const dhcpModes = [
  { title: '关闭', value: 'off' },
  { title: '自动（/24）', value: 'on' },
  { title: '指定网段', value: 'cidr' },
]

const protoOptions = ['tcp', 'udp', 'quic', 'ws', 'wss', 'faketcp']

function splitEndpoint(url) {
  const m = String(url || '').trim().match(/^([a-z0-9+.-]+):\/\/(.*)$/i)
  return m ? { proto: m[1].toLowerCase(), addr: m[2] } : { proto: 'tcp', addr: String(url || '') }
}

function joinEndpoint(proto, addr) {
  const value = String(addr || '').trim()
  if (!value) return ''
  let host = value
  let port = ''
  const closeBracket = value.lastIndexOf(']')
  if (value.startsWith('[') && closeBracket > 0 && value[closeBracket + 1] === ':') {
    host = value.slice(1, closeBracket)
    port = value.slice(closeBracket + 2)
  } else {
    const i = value.lastIndexOf(':')
    if (i > 0) {
      host = value.slice(0, i)
      port = value.slice(i + 1)
    }
  }
  if (!port) return ''
  const needsBracket = host.includes(':') && !host.startsWith('[')
  const h = needsBracket ? `[${host}]` : host
  return `${proto || 'tcp'}://${h}:${port}`
}

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

const toggles = [
  { key: 'latency_first', label: '延迟优先', hint: '延迟优先模式：尝试使用最低延迟路径转发流量；默认使用最短路径。' },
  { key: 'use_smoltcp', label: '用户态协议栈', hint: '为子网代理和 KCP 代理启用 smoltcp 用户态协议栈。' },
  { key: 'enable_ipv6', label: '禁用 IPv6', hint: '不使用 IPv6。', invert: true },
  { key: 'enable_kcp_proxy', label: 'KCP 代理', hint: '使用 KCP 代理 TCP 流，提高 UDP 丢包网络上的延迟和吞吐量。' },
  { key: 'disable_kcp_input', label: '禁用 KCP 输入', hint: '不允许其他节点使用 KCP 代理 TCP 流到此节点；开启 KCP 代理的节点访问本节点时仍用原始 TCP 连接。' },
  { key: 'enable_quic_proxy', label: 'QUIC 代理', hint: '使用 QUIC 代理 TCP 流，提高 UDP 丢包网络上的延迟和吞吐量。' },
  { key: 'disable_quic_input', label: '禁用 QUIC 输入', hint: '不允许其他节点使用 QUIC 代理 TCP 流到此节点；开启 QUIC 代理的节点访问本节点时仍用原始 TCP 连接。' },
  { key: 'disable_p2p', label: '禁用 P2P', hint: '禁用普通自动 P2P；仍会与标记为 need-p2p 的节点建立 P2P，其他节点不应主动与本节点建立 P2P。' },
  { key: 'p2p_only', label: '仅 P2P', hint: '仅与已经建立 P2P 连接的对等节点通信。' },
  { key: 'lazy_p2p', label: '延迟 P2P', hint: '仅在实际流量需要某个对等节点时才尝试建立 P2P；被标记为 need-p2p 的节点仍会主动连接。' },
  { key: 'bind_device', label: '仅物理网卡', hint: '将连接器套接字绑定到物理设备以避免路由问题（例如子网代理网段与节点网段冲突时）。' },
  { key: 'no_tun', label: '无 TUN 模式', hint: '不创建 TUN 设备，可以使用子网代理访问节点。' },
  { key: 'enable_exit_node', label: '出口节点', hint: '允许此节点成为出口节点。' },
  { key: 'relay_all_peer_rpc', label: '转发 RPC 包', hint: '转发所有对等节点的 RPC 数据包，即使对等节点不在转发网络白名单中，帮助其建立 P2P 连接。' },
  { key: 'need_p2p', label: '需要 P2P', hint: '声明即使其他节点启用了延迟 P2P（lazy_p2p），也应主动与当前节点建立 P2P 连接。' },
  { key: 'multi_thread', label: '多线程', hint: '为每个实例使用多线程运行时，默认开启。' },
  { key: 'proxy_forward_by_system', label: '系统转发', hint: '通过系统内核转发子网代理数据包，禁用内置 NAT。' },
  { key: 'enable_encryption', label: '禁用加密', hint: '禁用对等节点通信的加密，默认开启加密；必须与对等节点设置一致。', invert: true },
  { key: 'disable_tcp_hole_punching', label: '禁用 TCP 打洞', hint: '禁用 TCP 打洞功能。' },
  { key: 'disable_udp_hole_punching', label: '禁用 UDP 打洞', hint: '禁用 UDP 打洞功能。' },
  { key: 'enable_udp_broadcast_relay', label: 'UDP 广播中继', hint: '仅 Windows：捕获物理网卡上的本机 UDP 广播包并转发给 EasyTier 对等节点，帮助局域网游戏发现房间，需要管理员权限。' },
  { key: 'disable_upnp', label: '禁用 UPnP', hint: '禁用符合条件监听器的运行时 UPnP/NAT-PMP 端口映射；自动端口映射默认开启。' },
  { key: 'disable_sym_hole_punching', label: '禁用对称 NAT 打洞', hint: '禁用基于生日攻击的对称 NAT（NAT4）UDP 打洞；该方式可能被运营商封锁。' },
  { key: 'accept_dns', label: '魔法 DNS', hint: '启用魔法 DNS，可用域名访问其他节点（如 <hostname>.et.net）；会修改系统 DNS 设置，请谨慎启用。' },
  { key: 'private_mode', label: '私有模式', hint: '仅允许两类 foreign network 接入：能验证其使用相同 network secret 的节点，或已通过管理节点传播而被信任的 credential 节点；secret 不同或缺失会被拒绝。' },
]


const emptyForm = () => ({
  hostname: '',
  ipv4: '',
  network_name: '',
  network_secret: '',
  p2p_group: '',
  dhcp_mode: 'off',
  dhcp_cidr: '',
  ipv6: '',
  ipv6_public_addr_prefix: '',
  ipv6_public_addr_auto: false,
  ipv6_public_addr_provider: false,
  node_bandwidth_mbps: '',
  peers: [],
  proxy_network: [],
  routes: '',
  exit_nodes: '',
  outbound_interface: '',
  tcp_whitelist: '',
  udp_whitelist: '',
  socks5_proxy: '',
  vpn_portal: false,
  vpn_portal_listen: '0.0.0.0:11011',
  vpn_clients: [],
  acl_chains: [],
  port_forwards: [],
  flags: {
    dev_name: '',
    mtu: '',
    instance_recv_bps_limit: '',
    relay_network_whitelist: '',
    latency_first: false,
    use_smoltcp: false,
    enable_ipv6: true,
    enable_kcp_proxy: false,
    disable_kcp_input: false,
    enable_quic_proxy: false,
    disable_quic_input: false,
    disable_p2p: false,
    p2p_only: false,
    lazy_p2p: false,
    bind_device: true,
    no_tun: false,
    enable_exit_node: false,
    relay_all_peer_rpc: false,
    need_p2p: false,
    multi_thread: true,
    proxy_forward_by_system: false,
    enable_encryption: true,
    disable_tcp_hole_punching: false,
    disable_udp_hole_punching: false,
    enable_udp_broadcast_relay: false,
    disable_upnp: false,
    disable_sym_hole_punching: false,
    accept_dns: false,
    private_mode: false,
  },
})

const form = ref(emptyForm())

function parseToml(text) {
  const f = emptyForm()
  if (!text) return f
  const pick = (re) => {
    const m = text.match(re)
    return m ? m[1] : ''
  }
  f.hostname = pick(/^\s*hostname\s*=\s*"([^"]*)"/m)
  f.ipv4 = pick(/^\s*ipv4\s*=\s*"([^"]*)"/m)
  f.ipv6 = pick(/^\s*ipv6\s*=\s*"([^"]*)"/m)
  f.ipv6_public_addr_prefix = pick(/^\s*ipv6_public_addr_prefix\s*=\s*"([^"]*)"/m)
  f.node_bandwidth_mbps = pick(/^\s*node_bandwidth_mbps\s*=\s*([0-9.]+)/m)
  f.p2p_group = pick(/^\s*p2p_group\s*=\s*"([^"]*)"/m)
  f.network_name = pick(/network_name\s*=\s*"([^"]*)"/)
  f.network_secret = pick(/network_secret\s*=\s*"([^"]*)"/)
  const dhcpRaw = (text.match(/^\s*dhcp\s*=\s*(.+)$/m) || [])[1] || ''
  if (/^true/.test(dhcpRaw)) {
    f.dhcp_mode = 'on'
  } else if (dhcpRaw.startsWith('"')) {
    f.dhcp_mode = 'cidr'
    f.dhcp_cidr = (dhcpRaw.match(/"([^"]*)"/) || [])[1] || ''
  } else {
    f.dhcp_mode = 'off'
  }
  f.ipv6_public_addr_auto = /^\s*ipv6_public_addr_auto\s*=\s*true/m.test(text)
  f.ipv6_public_addr_provider = /^\s*ipv6_public_addr_provider\s*=\s*true/m.test(text)
  f.proxy_network = [...text.matchAll(/\[\[proxy_network\]\][\s\S]*?cidr\s*=\s*"([^"]+)"/g)].map((m) => m[1])
  f.routes = arrayOf(text, 'routes').join('\n')
  f.exit_nodes = arrayOf(text, 'exit_nodes').join('\n')
  f.outbound_interface = pick(/^\s*outbound_interface\s*=\s*"([^"]*)"/m)
  f.tcp_whitelist = arrayOf(text, 'tcp_whitelist').join(', ')
  f.udp_whitelist = arrayOf(text, 'udp_whitelist').join(', ')
  f.socks5_proxy = pick(/^\s*socks5_proxy\s*=\s*"([^"]*)"/m)
  f.peers = [...text.matchAll(/\[\[peer\]\]\s*\n\s*uri\s*=\s*"([^"]+)"/g)].map((m) => splitEndpoint(m[1]))
  const pf = [...text.matchAll(/\[\[port_forward\]\]([\s\S]*?)(?=\n\[|\n*$)/g)]
  f.port_forwards = pf.map((m) => {
    const bind = (m[1].match(/bind_addr\s*=\s*"([^"]*)"/) || [])[1] || '0.0.0.0:0'
    const dst = (m[1].match(/dst_addr\s*=\s*"([^"]*)"/) || [])[1] || ':0'
    const split = (addr) => {
      const i = addr.lastIndexOf(':')
      return [addr.slice(0, i), Number(addr.slice(i + 1)) || 0]
    }
    const [bindAddr, bindPort] = split(bind)
    const [dstAddr, dstPort] = split(dst)
    return {
      proto: (m[1].match(/proto\s*=\s*"([^"]*)"/) || [])[1] || 'tcp',
      bind_addr: bindAddr,
      bind_port: bindPort,
      dst_addr: dstAddr,
      dst_port: dstPort,
    }
  })
  f.vpn_portal = /\[vpn_portal_config\][\s\S]*?enabled\s*=\s*true/.test(text)
  f.vpn_portal_listen = pick(/\[vpn_portal_config\][\s\S]*?wireguard_listen\s*=\s*"([^"]*)"/) || '0.0.0.0:11011'
  f.vpn_clients = [...text.matchAll(/\[\[vpn_portal_config\.clients\]\]([\s\S]*?)(?=\n\[|$)/g)].map((m) => ({
    name: (m[1].match(/name\s*=\s*"([^"]*)"/) || [])[1] || '',
    virtual_ip: (m[1].match(/virtual_ip\s*=\s*"([^"]*)"/) || [])[1] || '',
    groups: arrayOf(m[1], 'groups').join(', '),
  }))
  const flagsBlock = (text.match(/\[flags\]([\s\S]*?)(?=\n\[|$)/) || [])[1] || ''
  for (const key of Object.keys(f.flags)) {
    if (typeof f.flags[key] === 'boolean') {
      const m = flagsBlock.match(new RegExp(`^\\s*${key}\\s*=\\s*(true|false)`, 'm'))
      if (m) f.flags[key] = m[1] === 'true'
    } else {
      const m = flagsBlock.match(new RegExp(`^\\s*${key}\\s*=\\s*"?([^"\\n]*)"?`, 'm'))
      if (m) f.flags[key] = m[1].trim()
    }
  }
  const aclBody = (text.match(/\[acl\.acl_v1\]([\s\S]*)$/) || [])[1] || ''
  const chainBlocks = aclBody.split(/\[\[acl\.acl_v1\.chains\]\]/).slice(1)
  f.acl_chains = chainBlocks.map((block) => {
    const head = block.split(/\[\[acl\.acl_v1\.chains\.rules\]\]/)[0]
    const rules = block.split(/\[\[acl\.acl_v1\.chains\.rules\]\]/).slice(1).map((rb) => ({
      action: Number((rb.match(/action\s*=\s*(\d+)/) || [])[1] || 0),
      protocol: Number((rb.match(/protocol\s*=\s*(\d+)/) || [])[1] || 5),
      source_ips: arrayOf(rb, 'source_ips').join(', '),
      destination_ips: arrayOf(rb, 'destination_ips').join(', '),
      ports: arrayOf(rb, 'ports').join(', '),
    }))
    return {
      name: (head.match(/name\s*=\s*"([^"]*)"/) || [])[1] || 'chain',
      type: Number((head.match(/chain_type\s*=\s*(\d+)/) || [])[1] || 1),
      default_action: Number((head.match(/default_action\s*=\s*(\d+)/) || [])[1] || 2),
      rules,
    }
  })
  return f
}

function arrayOf(text, key) {
  const m = text.match(new RegExp(`^\\s*${key}\\s*=\\s*\\[([^\\]]*)\\]`, 'm'))
  if (!m) return []
  return m[1].split(',').map((s) => s.trim().replace(/^"|"$/g, '')).filter(Boolean)
}

function str(v) {
  return `"${String(v ?? '').replace(/\\/g, '\\\\').replace(/"/g, '\\"')}"`
}

function buildToml(f) {
  const lines = []
  if (f.hostname.trim()) lines.push(`hostname = ${str(f.hostname.trim())}`)
  if (f.ipv4.trim()) lines.push(`ipv4 = ${str(f.ipv4.trim())}`)
  if (f.p2p_group.trim()) lines.push(`p2p_group = ${str(f.p2p_group.trim())}`)
  if (f.dhcp_mode === 'on') lines.push('dhcp = true')
  else if (f.dhcp_mode === 'cidr' && f.dhcp_cidr.trim()) lines.push(`dhcp = ${str(f.dhcp_cidr.trim())}`)
  if (f.ipv6.trim()) lines.push(`ipv6 = ${str(f.ipv6.trim())}`)
  if (f.ipv6_public_addr_prefix.trim()) lines.push(`ipv6_public_addr_prefix = ${str(f.ipv6_public_addr_prefix.trim())}`)
  if (f.node_bandwidth_mbps !== '' && f.node_bandwidth_mbps != null) lines.push(`node_bandwidth_mbps = ${Number(f.node_bandwidth_mbps)}`)
  if (f.ipv6_public_addr_auto) lines.push('ipv6_public_addr_auto = true')
  if (f.ipv6_public_addr_provider) lines.push('ipv6_public_addr_provider = true')
  // ALL remaining bare (top-level) keys MUST be emitted before ANY [[table]]
  // section: in TOML every key after a section header belongs to that table.
  // These used to be emitted after [[proxy_network]], which silently
  // reassigned exit_nodes / outbound_interface / routes / socks5 / whitelists
  // into the proxy_network table — exit routing then never applied (config
  // get_exit_nodes() read the root table and found nothing).
  const routes = f.routes.split('\n').map((s) => s.trim()).filter(Boolean)
  if (routes.length) lines.push(`routes = [${routes.map(str).join(', ')}]`)
  const exits = f.exit_nodes.split('\n').map((s) => s.trim()).filter(Boolean)
  if (exits.length) lines.push(`exit_nodes = [${exits.map(str).join(', ')}]`)
  if (f.outbound_interface) lines.push(`outbound_interface = ${str(f.outbound_interface)}`)
  if (f.socks5_proxy) lines.push(`socks5_proxy = ${str(f.socks5_proxy)}`)
  const tcpw = f.tcp_whitelist.split(',').map((s) => s.trim()).filter(Boolean)
  if (tcpw.length) lines.push(`tcp_whitelist = [${tcpw.map(str).join(', ')}]`)
  const udpw = f.udp_whitelist.split(',').map((s) => s.trim()).filter(Boolean)
  if (udpw.length) lines.push(`udp_whitelist = [${udpw.map(str).join(', ')}]`)
  if (f.proxy_network.filter(Boolean).length) {
    for (const cidr of f.proxy_network.filter(Boolean)) {
      lines.push('', '[[proxy_network]]', `cidr = ${str(cidr)}`)
    }
  }
  for (const pf of f.port_forwards) {
    if (!pf.dst_addr) continue
    lines.push('', '[[port_forward]]')
    lines.push(`proto = ${str(pf.proto)}`)
    lines.push(`bind_addr = ${str(`${pf.bind_addr || '0.0.0.0'}:${Number(pf.bind_port) || 0}`)}`)
    lines.push(`dst_addr = ${str(`${pf.dst_addr}:${Number(pf.dst_port) || 0}`)}`)
  }
  lines.push('', '[network_identity]', `network_name = ${str(f.network_name)}`, `network_secret = ${str(f.network_secret)}`)
  for (const peer of f.peers.map((p) => joinEndpoint(p.proto, p.addr)).filter(Boolean)) {
    lines.push('', '[[peer]]', `uri = ${str(peer)}`)
  }
  if (f.vpn_portal) {
    lines.push('', '[vpn_portal_config]', 'enabled = true', `wireguard_listen = ${str(f.vpn_portal_listen || '0.0.0.0:11011')}`)
    for (const c of f.vpn_clients) {
      if (!c.name || !c.virtual_ip) continue
      lines.push('', '[[vpn_portal_config.clients]]', `name = ${str(c.name)}`, `virtual_ip = ${str(c.virtual_ip)}`)
      const groups = c.groups.split(',').map((g) => g.trim()).filter(Boolean)
      if (groups.length) lines.push(`groups = [${groups.map(str).join(', ')}]`)
    }
  }
  if (f.acl_chains.length) {
    lines.push('', '[acl.acl_v1]')
    for (const chain of f.acl_chains) {
      lines.push('', '[[acl.acl_v1.chains]]')
      lines.push(`name = ${str(chain.name || 'chain')}`)
      lines.push(`chain_type = ${Number(chain.type) || 1}`)
      lines.push(`default_action = ${Number(chain.default_action) || 2}`)
      lines.push('enabled = true')
      for (const rule of chain.rules) {
        lines.push('', '[[acl.acl_v1.chains.rules]]')
        lines.push(`name = ${str(rule.name || 'rule')}`)
        lines.push('enabled = true')
        lines.push(`action = ${Number(rule.action) || 0}`)
        lines.push(`protocol = ${Number(rule.protocol) || 5}`)
        const src = rule.source_ips.split(',').map((v) => v.trim()).filter(Boolean)
        const dst = rule.destination_ips.split(',').map((v) => v.trim()).filter(Boolean)
        const ports = rule.ports.split(',').map((v) => v.trim()).filter(Boolean)
        if (src.length) lines.push(`source_ips = [${src.map(str).join(', ')}]`)
        if (dst.length) lines.push(`destination_ips = [${dst.map(str).join(', ')}]`)
        if (ports.length) lines.push(`ports = [${ports.map(str).join(', ')}]`)
      }
    }
  }

  const flags = f.flags
  lines.push('', '[flags]')
  for (const [key, value] of Object.entries(flags)) {
    if (value === '' || value == null) continue
    if (typeof value === 'boolean') lines.push(`${key} = ${value}`)
    else if (typeof value === 'number') lines.push(`${key} = ${value}`)
    else lines.push(`${key} = ${str(value)}`)
  }
  return lines.join('\n')
}

const generated = computed(() => buildToml(form.value))

const IPV4 = /^(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})(?:\/(\d{1,2}))?$/
function validIpv4(text, requirePrefix = false, maxPrefix = 32) {
  const m = String(text || '').trim().match(IPV4)
  if (!m) return false
  const parts = [m[1], m[2], m[3], m[4]].map((v) => Number(v))
  if (parts.some((v) => v > 255)) return false
  if (requirePrefix && m[5] == null) return false
  if (m[5] != null && (Number(m[5]) > maxPrefix)) return false
  return true
}
function validPortSpec(text) {
  const t = String(text || '').trim()
  if (!t) return false
  if (t.includes('-')) {
    const [a, b] = t.split('-').map((v) => Number(v))
    return Number.isInteger(a) && Number.isInteger(b) && a >= 1 && b <= 65535 && a <= b
  }
  const n = Number(t)
  return Number.isInteger(n) && n >= 1 && n <= 65535
}
function validUrl(text, schemes) {
  try {
    const u = new URL(String(text || '').trim())
    return schemes.includes(u.protocol.replace(':', '')) && !!u.host
  } catch {
    return false
  }
}
function validHostPort(text) {
  const t = String(text || '').trim()
  const i = t.lastIndexOf(':')
  if (i <= 0) return false
  const host = t.slice(0, i)
  const port = Number(t.slice(i + 1))
  return host.length > 0 && Number.isInteger(port) && port >= 1 && port <= 65535
}
function splitList(text) {
  return String(text || '').split(/[\n,]/).map((v) => v.trim()).filter(Boolean)
}

const errors = computed(() => {
  const list = []
  const push = (msg) => list.push(msg)
  if (!form.value.network_name.trim()) push('网络名称不能为空')
  if (form.value.dhcp_mode === 'off') {
    if (!form.value.ipv4.trim()) push('未开启 DHCP 时必须填写虚拟 IPv4')
    else if (!validIpv4(form.value.ipv4, true)) push(`虚拟 IPv4 格式错误：${form.value.ipv4}`)
  } else if (form.value.dhcp_mode === 'cidr') {
    if (!validIpv4(form.value.dhcp_cidr, true, 30)) push(`DHCP 网段格式错误（需要 CIDR 且前缀 ≤30）：${form.value.dhcp_cidr}`)
  }
  const ipv6ok = (text) => /^[0-9a-fA-F:]+(\/\d{1,3})?$/.test(String(text || '').trim()) && String(text).includes(':')
  if (form.value.ipv6.trim() && !ipv6ok(form.value.ipv6)) push(`虚拟 IPv6 格式错误：${form.value.ipv6}`)
  if (form.value.ipv6_public_addr_prefix.trim() && !ipv6ok(form.value.ipv6_public_addr_prefix)) push(`公网 IPv6 前缀格式错误：${form.value.ipv6_public_addr_prefix}`)
  for (const p of form.value.peers) {
    if (!p.addr.trim()) continue
    const url = joinEndpoint(p.proto, p.addr)
    if (!url || !validUrl(url, ['tcp', 'udp', 'quic', 'ws', 'wss', 'faketcp'])) push(`初始节点地址无效：${p.addr}`)
  }
  for (const c of form.value.proxy_network.filter(Boolean)) {
    if (!validIpv4(c, true)) push(`子网代理 CIDR 无效：${c}`)
  }
  for (const r of splitList(form.value.routes)) if (!validIpv4(r, true)) push(`自定义路由无效：${r}`)
  for (const e of splitList(form.value.exit_nodes)) if (!validIpv4(e, false)) push(`出口节点 IP 无效：${e}`)
  for (const w of splitList(form.value.tcp_whitelist)) if (!validPortSpec(w)) push(`TCP 白名单端口无效：${w}`)
  for (const w of splitList(form.value.udp_whitelist)) if (!validPortSpec(w)) push(`UDP 白名单端口无效：${w}`)
  if (form.value.socks5_proxy && !validUrl(form.value.socks5_proxy, ['socks5'])) push(`socks5 服务器地址无效：${form.value.socks5_proxy}`)
  if (form.value.vpn_portal) {
    if (!validHostPort(form.value.vpn_portal_listen)) push(`VPN Portal WG 监听无效：${form.value.vpn_portal_listen}`)
  }
  for (const c of form.value.vpn_clients) {
    if (!c.name.trim()) push('VPN Portal 客户端名称不能为空')
    if (!validIpv4(c.virtual_ip, true)) push(`VPN Portal 客户端虚拟 IP 无效：${c.virtual_ip}`)
  }
  for (const pf of form.value.port_forwards) {
    if (!validIpv4(pf.bind_addr, false)) push(`端口转发绑定地址无效：${pf.bind_addr}`)
    if (!validPortSpec(pf.bind_port)) push(`端口转发绑定端口无效：${pf.bind_port}`)
    if (!validIpv4(pf.dst_addr, false)) push(`端口转发目标地址无效：${pf.dst_addr}`)
    if (!validPortSpec(pf.dst_port)) push(`端口转发目标端口无效：${pf.dst_port}`)
  }
  const mtu = Number(form.value.flags.mtu)
  if (form.value.flags.mtu !== '' && (!Number.isInteger(mtu) || mtu < 576 || mtu > 65535)) push(`MTU 无效：${form.value.flags.mtu}`)
  if (form.value.flags.instance_recv_bps_limit !== '' && Number(form.value.flags.instance_recv_bps_limit) <= 0) push('接收限速必须大于 0')
  if (form.value.node_bandwidth_mbps !== '' && !(Number(form.value.node_bandwidth_mbps) > 0)) push('节点带宽必须大于 0')
  for (const chain of form.value.acl_chains) {
    if (!chain.name.trim()) push('ACL 链名不能为空')
    for (const rule of chain.rules) {
      if (rule.source_ips && splitList(rule.source_ips).some((v) => !validIpv4(v, true))) push(`ACL 源 CIDR 无效：${rule.source_ips}`)
      if (rule.destination_ips && splitList(rule.destination_ips).some((v) => !validIpv4(v, true))) push(`ACL 目的 CIDR 无效：${rule.destination_ips}`)
      if (rule.ports && splitList(rule.ports).some((v) => !validPortSpec(v))) push(`ACL 端口无效：${rule.ports}`)
    }
  }
  return list
})

function validate() {
  showErrors.value = true
  return errors.value[0] || ''
}

defineExpose({ validate })

watch(
  () => props.toml,
  (text) => {
    if (internal) return
    showErrors.value = false
    rawToml.value = text
    form.value = parseToml(text)
  },
  { immediate: true },
)

function emitToml(value) {
  internal = true
  rawToml.value = value
  emit('update:toml', value)
  queueMicrotask(() => { internal = false })
}

watch(generated, (value) => {
  if (!advanced.value) emitToml(value)
})

watch(rawToml, (value) => {
  if (advanced.value) emitToml(value)
})

watch(advanced, (value) => {
  if (value) rawToml.value = generated.value
  else form.value = parseToml(rawToml.value)
})
</script>

<style scoped>
.net-editor {
  font-size: 13px;
}
.editor-head {
  margin-bottom: 2px;
}
.section-title {
  display: flex;
  align-items: center;
  gap: 5px;
  margin: 20px 0 10px;
  font-size: 11px;
  font-weight: 600;
  letter-spacing: .05em;
  text-transform: uppercase;
  color: rgba(var(--v-theme-on-surface), .6);
}
.cfg-grid {
  display: grid;
  grid-template-columns: 1fr 1fr;
  gap: 14px 16px;
}
.proto-select { max-width: 120px; }
.cfg-info { color: rgba(var(--v-theme-on-surface), .5); cursor: help; }
.cfg-info:hover { color: rgb(var(--v-theme-primary)); }
.cfg-tip { font-size: 12px; line-height: 1.55; white-space: pre-line; }
.row-item {
  display: flex;
  align-items: center;
  gap: 8px;
  margin-bottom: 10px;
}
.section-sep {
  margin-top: 22px;
  padding-top: 16px;
  border-top: 1px dashed rgba(var(--v-theme-on-surface), .14);
}
.hint-line {
  margin: -2px 0 6px 2px;
  font-size: 11.5px;
  color: rgba(var(--v-theme-on-surface), .55);
}
.forward-card,
.acl-chain {
  margin-bottom: 12px;
  padding: 12px;
  border: 1px solid rgba(var(--v-theme-on-surface), .12);
  border-radius: 10px;
  background: rgba(var(--v-theme-on-surface), .02);
}
.forward-row {
  display: grid;
  grid-template-columns: 104px minmax(0, 1.6fr) 118px 36px;
  gap: 10px;
  align-items: end;
}
.forward-row + .forward-row { margin-top: 10px; }
.forward-row--target { grid-template-columns: minmax(0, 1.6fr) 118px; padding-right: 46px; }
.vpn-grid {
  display: grid;
  grid-template-columns: repeat(3, minmax(0, 1fr)) 36px;
  gap: 10px;
  align-items: end;
}
.acl-chain-grid {
  display: grid;
  grid-template-columns: minmax(0, 1.2fr) 140px 140px 36px;
  gap: 10px;
  align-items: end;
}
.acl-rule-top {
  display: grid;
  grid-template-columns: 130px 130px minmax(0, 1fr) 36px;
  gap: 10px;
  align-items: end;
}
.acl-rule-cidrs {
  display: grid;
  grid-template-columns: 1fr 1fr;
  gap: 10px;
  margin-top: 10px;
}
.forward-del {
  margin-bottom: 2px;
}
.acl-rule {
  margin-top: 12px;
  padding-top: 12px;
  border-top: 1px dashed rgba(var(--v-theme-on-surface), .14);
}
.flags-grid {
  display: grid;
  grid-template-columns: repeat(3, minmax(0, 1fr));
  gap: 8px 18px;
  margin-top: 10px;
}
.flag-item {
  display: flex;
  flex-direction: column;
  gap: 2px;
  min-width: 0;
}
.config-preview {
  max-height: 220px;
  overflow: auto;
  padding: 10px 12px;
  border-radius: 8px;
  background: rgba(var(--v-theme-on-surface), .04);
  font-size: 11px;
  line-height: 1.55;
  white-space: pre-wrap;
}
.net-editor :deep(.v-field__input),
.net-editor :deep(.v-field input),
.net-editor :deep(textarea) {
  font-size: 13px;
  min-height: 32px;
  color: rgba(var(--v-theme-on-surface), .95);
}
.net-editor :deep(.v-field) {
  min-height: 38px;
}
.net-editor :deep(.v-field__input::placeholder),
.net-editor :deep(textarea::placeholder) {
  color: rgba(var(--v-theme-on-surface), .5);
  opacity: 1;
}
.net-editor :deep(.v-field-label) {
  color: rgba(var(--v-theme-on-surface), .75);
}
@media (max-width: 640px) {
  .cfg-grid,
  .flags-grid {
    grid-template-columns: 1fr;
  }
  .forward-row, .forward-row--target {
    grid-template-columns: 1fr 1fr;
    padding-right: 0;
  }
}
</style>
