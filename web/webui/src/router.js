import { createRouter, createWebHashHistory } from 'vue-router'
import { api } from './api'

const routes = [
  { path: '/login', component: () => import('./views/LoginView.vue'), meta: { public: true } },
  {
    path: '/',
    component: () => import('./layouts/DefaultLayout.vue'),
    children: [
      { path: '', redirect: '/dashboard' },
      { path: 'dashboard', meta: { title: '仪表盘', titleKey: 'nav.dashboard' }, component: () => import('./views/DashboardView.vue') },
      { path: 'topology', meta: { title: '拓扑', titleKey: 'nav.topology' }, component: () => import('./views/TopologyView.vue') },
      { path: 'nodes', meta: { title: '节点', titleKey: 'nav.nodes' }, component: () => import('./views/NodesView.vue') },
      { path: 'links', meta: { title: '链路', titleKey: 'nav.links' }, component: () => import('./views/LinksView.vue') },
      { path: 'routes', meta: { title: '路由', titleKey: 'nav.routes' }, component: () => import('./views/RoutesView.vue') },
      { path: 'history', meta: { title: '历史指标', titleKey: 'nav.history' }, component: () => import('./views/HistoryView.vue') },
      { path: 'settings', meta: { title: '设置', titleKey: 'nav.settings' }, component: () => import('./views/SettingsView.vue') },
    ],
  },
]

const router = createRouter({ history: createWebHashHistory(), routes })

router.beforeEach(async (to) => {
  if (to.meta.public) return true
  try {
    await api.overview()
    return true
  } catch {
    return { path: '/login' }
  }
})

export default router
