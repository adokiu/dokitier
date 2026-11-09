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
  authStatus: () => request('/api/auth/status'),
  login: (username, password, totp) => request('/api/login', { method: 'POST', headers: jsonHeaders, body: JSON.stringify(totp ? { username, password, totp } : { username, password }) }),
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
  boardNetworks: () => request('/api/board/networks'),
  createNetwork: (body) => request('/api/board/networks/create', { method: 'POST', headers: jsonHeaders, body: JSON.stringify(body) }),
  boardLinks: () => request('/api/board/links'),
  boardRoutes: () => request('/api/board/routes'),
  boardHistory: (network, a, b, range) => request(`/api/board/history?network=${encodeURIComponent(network)}&a=${a}&b=${b}&range=${range}`),
  boardNodes: () => request('/api/board/nodes'),
  boardNode: (id) => request('/api/board/nodes/' + id),
  boardRoutingSettings: () => request('/api/board/settings/routing'),
  setBoardRouting: (params) => request('/api/board/settings/routing', { method: 'POST', headers: jsonHeaders, body: JSON.stringify(params) }),
  boardPerf: () => request('/api/board/system/perf'),
  boardBandwidthHistory: (range = '1h') => request(`/api/board/bandwidth/history?range=${range}`),
  boardSecurity: () => request('/api/board/security'),
  precheckConfig: (network, toml) => request('/api/board/precheck', { method: 'POST', headers: jsonHeaders, body: JSON.stringify({ network, toml }) }),
  boardDrift: () => request('/api/board/drift'),
  totpSetup: () => request('/api/board/security/totp/setup', { method: 'POST', headers: jsonHeaders, body: '{}' }),
  totpVerify: (code) => request('/api/board/security/totp/verify', { method: 'POST', headers: jsonHeaders, body: JSON.stringify({ code }) }),
  totpDisable: (code) => request('/api/board/security/totp/disable', { method: 'POST', headers: jsonHeaders, body: JSON.stringify({ code }) }),
  setBoardSecurity: (body) => request('/api/board/security', { method: 'POST', headers: jsonHeaders, body: JSON.stringify(body) }),
  account: () => request('/api/board/account'),
  updateAccount: (body) => request('/api/board/account', { method: 'POST', headers: jsonHeaders, body: JSON.stringify(body) }),
  boardAcl: () => request('/api/board/acl'),
  setBoardAcl: (chains) => request('/api/board/acl', { method: 'POST', headers: jsonHeaders, body: JSON.stringify({ chains }) }),
  boardGeoSetting: () => request('/api/board/settings/geo'),
  setBoardGeo: (provider) => request('/api/board/settings/geo', { method: 'POST', headers: jsonHeaders, body: JSON.stringify({ provider }) }),
  setNodeSettings: (id, body) => request(`/api/board/nodes/${id}/settings`, { method: 'POST', headers: jsonHeaders, body: JSON.stringify(body) }),
  setAlias: (id, alias) => request('/api/board/nodes/' + id + '/alias', { method: 'POST', headers: jsonHeaders, body: JSON.stringify({ alias }) }),
  boardCommand: (id, command, network) => request('/api/board/nodes/' + id + '/command', { method: 'POST', headers: jsonHeaders, body: JSON.stringify(network ? { command, network } : { command }) }),
  boardVersions: (id) => request('/api/board/nodes/' + id + '/versions'),
  boardVersion: (id, versionId) => request('/api/board/nodes/' + id + '/versions/' + versionId),
  saveConfig: (toml) => request('/api/config', { method: 'PUT', headers: jsonHeaders, body: JSON.stringify({ toml }) }),
}
