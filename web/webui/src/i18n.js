import { createI18n } from 'vue-i18n'

const messages = {
  zh: {
    common: { refresh: '刷新', save: '保存', cancel: '取消', close: '关闭', edit: '编辑', all: '全部', none: '暂无数据' },
    dashboard: { networks: '网络', routingWeights: '路由权重', nodesUnit: '{n} 节点', empty: '暂无网络', console: '控制台', online: '在线', nodeDistribution: '节点分布', viewAll: '查看全部' },
    links: { title: '链路', columns: { link: '链路', protocol: '协议', latency: '延迟', loss: '丢包', rx: '接收', tx: '发送', bandwidth: '带宽', actions: '操作' }, localOnly: '通过节点配置修改' },
    routes: { title: '路由', static: '静态路由', ospf: 'OSPF 路由', add: '新增静态路由', dest: '目标网段', nextHop: '下一跳', metric: 'Metric', note: '备注', enabled: '启用', disabled: '停用', actions: '操作' },
    history: { title: '历史指标', selectLink: '选择链路', latencyLoss: '延迟 / 丢包', bandwidth: '带宽使用', range24h: '24 小时', range7d: '7 天' },
    settings: { title: '设置' },
    nav: {
      dashboard: '仪表盘', topology: '拓扑', networks: '网络', nodes: '节点', links: '链路',
      routes: '路由', acl: '访问控制', history: '历史指标', settings: '系统设置', system: '系统',
    },
    login: {
      title: 'DokiTier Console', subtitle: '欢迎使用',
      username: '用户名', password: '密码', submit: '登录', hint: '如果这是首次登录，请在日志中查看默认密码。',
      missing: '请输入用户名和密码', wrong: '用户名或密码错误',
    },
    nodes: {
      title: '节点', subtitle: '节点状态、公网出口与网络配置',
      total: '共 {n} 个节点', networkFilter: '网络筛选', networks: '网络',
      config: '配置', running: '运行中', configFile: '配置文件',
      history: '历史', download: '下载', saveApply: '保存并应用',
      reload: '重新加载', applied: '已下发并重启该网络实例',
      noVersions: '暂无版本',
      columns: { status: '状态', name: '名称', hostname: '主机名', vpnIp: 'VPN IP', publicIp: '公网 IP', version: '版本', cpu: 'CPU', mem: '内存', network: '网络', actions: '操作' }, setAlias: '设置名称',
    },
  },
  en: {
    common: { refresh: 'Refresh', save: 'Save', cancel: 'Cancel', close: 'Close', edit: 'Edit', all: 'All', none: 'No data' },
    dashboard: { networks: 'Networks', routingWeights: 'Routing weights', nodesUnit: '{n} nodes', empty: 'No networks', console: 'Console', online: 'Online', nodeDistribution: 'Nodes', viewAll: 'View all' },
    links: { title: 'Links', columns: { link: 'Link', protocol: 'Protocol', latency: 'Latency', loss: 'Loss', rx: 'RX', tx: 'TX', bandwidth: 'Bandwidth', actions: 'Actions' }, localOnly: 'Edit via node config' },
    routes: { title: 'Routes', static: 'Static routes', ospf: 'OSPF routes', add: 'Add static route', dest: 'Destination CIDR', nextHop: 'Next hop', metric: 'Metric', note: 'Note', enabled: 'Enabled', disabled: 'Disabled', actions: 'Actions' },
    history: { title: 'History', selectLink: 'Select link', latencyLoss: 'Latency / loss', bandwidth: 'Bandwidth', range24h: '24 hours', range7d: '7 days' },
    settings: { title: 'Settings' },
    nav: {
      dashboard: 'Dashboard', topology: 'Topology', networks: 'Networks', nodes: 'Nodes', links: 'Links',
      routes: 'Routes', acl: 'ACL', history: 'History', settings: 'System settings', system: 'System',
    },
    login: {
      title: 'DokiTier Console', subtitle: 'Welcome',
      username: 'Username', password: 'Password', submit: 'Login', hint: 'If this is your first login, check the logs for the default password.',
      missing: 'Enter username and password', wrong: 'Invalid username or password',
    },
    nodes: {
      title: 'Nodes', subtitle: 'Node status, public egress and network configs',
      total: '{n} nodes', networkFilter: 'Network', networks: 'Networks',
      config: 'Config', running: 'Running', configFile: 'Config file',
      history: 'History', download: 'Download', saveApply: 'Save & apply',
      reload: 'Reload', applied: 'Pushed and restarted this network instance',
      noVersions: 'No versions yet',
      columns: { status: 'Status', name: 'Name', hostname: 'Hostname', vpnIp: 'VPN IP', publicIp: 'Public IP', version: 'Version', cpu: 'CPU', mem: 'Memory', network: 'Network', actions: 'Actions' }, setAlias: 'Set name',
    },
  },
}

export const i18n = createI18n({
  legacy: false,
  locale: localStorage.getItem('dokitier_locale') || 'zh',
  fallbackLocale: 'en',
  messages,
})

export function setLocale(locale) {
  i18n.global.locale.value = locale
  localStorage.setItem('dokitier_locale', locale)
}
