export function flagUrl(code) {
  const normalized = String(code || '').toLowerCase()
  if (!/^[a-z]{2}$/.test(normalized)) return ''
  return `https://flagcdn.com/${normalized}.svg`
}
