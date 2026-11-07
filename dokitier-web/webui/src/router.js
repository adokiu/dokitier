import { createRouter, createWebHashHistory } from 'vue-router'
import { api } from './api'

const routes = [
  { path: '/login', component: () => import('./views/LoginView.vue'), meta: { public: true } },
  {
    path: '/',
    component: () => import('./layouts/DefaultLayout.vue'),
    children: [
      { path: '', redirect: '/dashboard' },
      { path: 'dashboard', meta: { title: '仪表盘' }, component: () => import('./views/DashboardView.vue') },
      { path: 'topology', meta: { title: '拓扑' }, component: () => import('./views/TopologyView.vue') },
      { path: 'nodes', meta: { title: '节点' }, component: () => import('./views/NodesView.vue') },
      { path: 'links', meta: { title: '链路' }, component: () => import('./views/LinksView.vue') },
      { path: 'routes', meta: { title: '路由' }, component: () => import('./views/RoutesView.vue') },
      { path: 'history', meta: { title: '历史指标' }, component: () => import('./views/HistoryView.vue') },
      { path: 'settings', meta: { title: '设置' }, component: () => import('./views/SettingsView.vue') },
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
