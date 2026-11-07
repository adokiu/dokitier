const jsonHeaders = { 'Content-Type': 'application/json' }

async function request(path, options = {}) {
  const res = await fetch(path, { credentials: 'include', ...options })
  if (res.status === 401) {
    if (!location.hash.startsWith('#/login')) location.hash = '#/login'
    throw new Error('unauthorized')
  }
  if (!res.ok) {
    const text = await res.text()
    throw new Error(text || `HTTP ${res.status}`)
  }
  const ct = res.headers.get('content-type') || ''
  return ct.includes('application/json') ? res.json() : res.text()
}

export const api = {
  login: (username, password) => request('/api/login', { method: 'POST', headers: jsonHeaders, body: JSON.stringify({ username, password }) }),
  logout: () => request('/api/logout', { method: 'POST' }),
  overview: () => request('/api/overview'),
  topology: () => request('/api/topology'),
  nodes: () => request('/api/nodes'),
  node: (id) => request(`/api/nodes/${id}`),
  links: () => request('/api/links'),
  updateLink: (a, b, body) => request(`/api/links/${a}/${b}`, { method: 'PUT', headers: jsonHeaders, body: JSON.stringify(body) }),
  routes: () => request('/api/routes'),
  createStaticRoute: (body) => request('/api/static-routes', { method: 'POST', headers: jsonHeaders, body: JSON.stringify(body) }),
  deleteStaticRoute: (id) => request(`/api/static-routes/${id}`, { method: 'DELETE' }),
  settings: () => request('/api/settings'),
  saveSettings: (body) => request('/api/settings', { method: 'PUT', headers: jsonHeaders, body: JSON.stringify(body) }),
  history: (a, b, range) => request(`/api/history?a=${a}&b=${b}&range=${range}`),
  sync: () => request('/api/sync'),
  getConfig: () => request('/api/config'),
  boardOverview: () => request('/api/board/overview'),
  boardTopology: () => request('/api/board/topology'),
  boardLinks: () => request('/api/board/links'),
  boardRoutes: () => request('/api/board/routes'),
  boardHistory: (network, a, b, range) => request(`/api/board/history?network=${encodeURIComponent(network)}&a=${a}&b=${b}&range=${range}`),
  boardNodes: () => request('/api/board/nodes'),
  boardNode: (id) => request('/api/board/nodes/' + id),
  boardCommand: (id, command, network) => request('/api/board/nodes/' + id + '/command', { method: 'POST', headers: jsonHeaders, body: JSON.stringify(network ? { command, network } : { command }) }),
  boardVersions: (id) => request('/api/board/nodes/' + id + '/versions'),
  boardVersion: (id, versionId) => request('/api/board/nodes/' + id + '/versions/' + versionId),
  saveConfig: (toml) => request('/api/config', { method: 'PUT', headers: jsonHeaders, body: JSON.stringify({ toml }) }),
}
