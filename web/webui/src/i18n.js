import { createI18n } from 'vue-i18n'

const messages = {
  zh: {
    common: { refresh: '刷新', save: '保存', cancel: '取消', close: '关闭' },
    nav: {
      dashboard: '仪表盘', topology: '拓扑', nodes: '节点', links: '链路',
      routes: '路由', history: '历史指标', settings: '设置', system: '系统',
    },
    login: {
      title: '登录 dokitier', subtitle: '节点控制台，请输入管理员账号',
      username: '管理员用户名', password: '密码', submit: '登录',
      missing: '请输入用户名和密码', wrong: '用户名或密码错误',
    },
    nodes: {
      title: '节点', subtitle: '节点状态、公网出口与网络配置',
      total: '共 {n} 个节点', networkFilter: '网络筛选', networks: '网络',
      config: '配置', running: '运行中', configFile: '配置文件',
      history: '历史', download: '下载', saveApply: '保存并应用',
      reload: '重新加载', applied: '已下发并重启该网络实例',
      noVersions: '暂无版本',
      columns: { hostname: '主机名', ip: 'IP', version: '版本', network: '网络', lastSeen: '最后上报', actions: '操作' },
    },
  },
  en: {
    common: { refresh: 'Refresh', save: 'Save', cancel: 'Cancel', close: 'Close' },
    nav: {
      dashboard: 'Dashboard', topology: 'Topology', nodes: 'Nodes', links: 'Links',
      routes: 'Routes', history: 'History', settings: 'Settings', system: 'System',
    },
    login: {
      title: 'Sign in to dokitier', subtitle: 'Node console. Use the admin account.',
      username: 'Username', password: 'Password', submit: 'Sign in',
      missing: 'Enter username and password', wrong: 'Invalid username or password',
    },
    nodes: {
      title: 'Nodes', subtitle: 'Node status, public egress and network configs',
      total: '{n} nodes', networkFilter: 'Network', networks: 'Networks',
      config: 'Config', running: 'Running', configFile: 'Config file',
      history: 'History', download: 'Download', saveApply: 'Save & apply',
      reload: 'Reload', applied: 'Pushed and restarted this network instance',
      noVersions: 'No versions yet',
      columns: { hostname: 'Hostname', ip: 'IP', version: 'Version', network: 'Network', lastSeen: 'Last seen', actions: 'Actions' },
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
