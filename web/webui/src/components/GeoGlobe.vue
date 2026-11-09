<template>
  <div ref="hostEl" class="geo-globe-host"></div>
</template>

<script setup>
// Real 3D globe (globe.gl / three) for the 地理弧线 tab.
// Coordinates are the console's real IP-geolocation results (city + lat/lon per
// node); this component only renders: one point per city, one arc per city pair.
import { onBeforeUnmount, onMounted, ref, watch } from 'vue'

const props = defineProps({
  geo: { type: Object, default: () => ({ cities: [], links: [] }) },
})

const EARTH_NIGHT = '/images/earth/earth-night.jpg'
const EARTH_DAY = '/images/earth/earth-blue-marble.jpg'
const EARTH_BUMP = '/images/earth/earth-topology.png'
const EARTH_WATER = '/images/earth/earth-water.png'
const PALETTE = ['#5b8ff9', '#5ad8a6', '#f6bd16', '#e8684a', '#9270ca', '#269a99', '#ff99c3', '#6dc8ec']

const hostEl = ref(null)
let globe = null
let GlobeCtor = null
let three = null
let controls = null
let globeMaterial = null
let specular = null
let resizeObserver = null
let themeObserver = null
let resumeTimer = null
let destroyed = false

function isDark() {
  return document.querySelector('.v-application')?.classList.contains('v-theme--PurpleThemeDark') ?? true
}

function groupColors() {
  const groups = [...new Set((props.geo.cities || []).map((c) => c.group || 'default'))].sort()
  const map = new Map()
  groups.forEach((g, i) => map.set(g, PALETTE[i % PALETTE.length]))
  return map
}

function colorOf(group, alpha = 1) {
  const hex = groupColors().get(group || 'default') || PALETTE[0]
  const n = parseInt(hex.slice(1), 16)
  return `rgba(${(n >> 16) & 255}, ${(n >> 8) & 255}, ${n & 255}, ${alpha})`
}

function pointLabel(city) {
  return `${city.city} · ${city.nodes} 台节点`
}

function arcActive(link) {
  return (link.traffic || 0) >= 102400
}

function arcColor(link) {
  return arcActive(link) ? 'rgba(80, 220, 160, 0.92)' : 'rgba(150, 175, 205, 0.42)'
}

function arcStroke(link) {
  return 0.22 + Math.min(0.85, Math.log10(Math.max(10, link.traffic || 10)) * 0.12)
}

function pointRadius(city) {
  return 0.45 + Math.min(1.1, Math.log2((city.nodes || 1) + 1) * 0.42)
}

function applyMaterial() {
  if (!globe || !globeMaterial) return
  const dark = isDark()
  globe.globeImageUrl(dark ? EARTH_NIGHT : EARTH_DAY)
  globeMaterial.bumpScale = dark ? 0.02 : 0.03
  globeMaterial.shininess = dark ? 8 : 14
  globeMaterial.emissive.set(dark ? 0x1e3a5f : 0x355c7d)
  globeMaterial.emissiveIntensity = dark ? 0.48 : 0.32
  globeMaterial.needsUpdate = true
  globe
    .atmosphereColor(dark ? '#38bdf8' : '#60a5fa')
    .atmosphereAltitude(dark ? 0.14 : 0.11)
}

function updateData() {
  if (!globe) return
  const cities = (props.geo.cities || []).filter((c) => Number.isFinite(c.lat) && Number.isFinite(c.lon))
  const links = (props.geo.links || []).filter((l) => Number.isFinite(l.a_lat) && Number.isFinite(l.a_lon) && Number.isFinite(l.b_lat) && Number.isFinite(l.b_lon))
  globe
    .pointsData(cities)
    .labelsData(cities)
    .arcsData(links)
}

function stopAutoRotate() {
  if (!controls) return
  controls.autoRotate = false
  if (resumeTimer) {
    clearTimeout(resumeTimer)
    resumeTimer = null
  }
}

function scheduleResume() {
  if (!controls) return
  if (resumeTimer) clearTimeout(resumeTimer)
  resumeTimer = setTimeout(() => {
    resumeTimer = null
    if (!destroyed && controls) controls.autoRotate = true
  }, 8000)
}

function attachControls() {
  controls = globe.controls()
  controls.autoRotate = true
  controls.autoRotateSpeed = 0.45
  controls.enableDamping = true
  controls.enableZoom = true
  controls.enablePan = false
  controls.rotateSpeed = 0.6
  controls.zoomSpeed = 0.9
  controls.minDistance = 130
  controls.maxDistance = 520
  controls.addEventListener('start', stopAutoRotate)
  controls.addEventListener('end', scheduleResume)
}

function resize() {
  if (!globe || !hostEl.value) return
  const rect = hostEl.value.getBoundingClientRect()
  globe.width(Math.max(320, Math.round(rect.width))).height(Math.max(320, Math.round(rect.height)))
}

onMounted(() => {
  void startGlobe()
})

async function startGlobe() {
  if (!hostEl.value || globe) return
  const [{ default: Globe }, THREE] = await Promise.all([import('globe.gl'), import('three')])
  if (destroyed || !hostEl.value) return
  GlobeCtor = Globe
  three = THREE
  const rect = hostEl.value.getBoundingClientRect()
  globe = new GlobeCtor(hostEl.value, { rendererConfig: { alpha: true, antialias: true } })
    .width(Math.max(320, Math.round(rect.width)))
    .height(Math.max(320, Math.round(rect.height)))
    .backgroundColor('rgba(0,0,0,0)')
    .showAtmosphere(true)
    .atmosphereColor('#38bdf8')
    .atmosphereAltitude(0.14)
    .globeImageUrl(isDark() ? EARTH_NIGHT : EARTH_DAY)
    .bumpImageUrl(EARTH_BUMP)
    .pointsData([])
    .pointLat('lat')
    .pointLng('lon')
    .pointAltitude(0.012)
    .pointRadius(pointRadius)
    .pointColor((city) => colorOf(city.group))
    .pointsMerge(false)
    .pointsTransitionDuration(600)
    .pointLabel(pointLabel)
    .labelsData([])
    .labelLat('lat')
    .labelLng('lon')
    .labelText((city) => `${city.city} ${city.nodes}`)
    .labelSize(0.62)
    .labelDotRadius(0)
    .labelAltitude(0.014)
    .labelColor(() => (isDark() ? 'rgba(219, 234, 254, 0.95)' : 'rgba(15, 32, 48, 0.95)'))
    .labelResolution(2)
    .arcsData([])
    .arcStartLat('a_lat')
    .arcStartLng('a_lon')
    .arcEndLat('b_lat')
    .arcEndLng('b_lon')
    .arcColor(arcColor)
    .arcStroke(arcStroke)
    .arcAltitudeAutoScale(0.38)
    .arcDashLength((link) => (arcActive(link) ? 0.32 : 0.5))
    .arcDashGap(0.5)
    .arcDashAnimateTime((link) => (arcActive(link) ? 2200 : 0))
    .arcsTransitionDuration(500)

  const renderer = globe.renderer()
  renderer.setClearColor(0x000000, 0)
  renderer.domElement.style.background = 'transparent'

  globeMaterial = globe.globeMaterial()
  if ('shininess' in globeMaterial) {
    specular = new three.TextureLoader().load(EARTH_WATER, () => {
      if (!globeMaterial) return
      globeMaterial.specularMap = specular
      globeMaterial.needsUpdate = true
    })
    globeMaterial.specular = new three.Color(isDark() ? 0x64748b : 0x475569)
  }

  const frontLight = new three.DirectionalLight(0xffffff, 0.9)
  frontLight.position.set(1.2, 1.1, 1.6)
  const fillLight = new three.DirectionalLight(0xdbeafe, 0.55)
  fillLight.position.set(-1.2, 0.2, 1.2)
  const rearLight = new three.DirectionalLight(0xe0f2fe, 0.45)
  rearLight.position.set(-1.0, -0.8, -1.2)
  globe.lights([new three.AmbientLight(0xffffff, 1.8), frontLight, fillLight, rearLight])

  applyMaterial()
  attachControls()
  globe.pointOfView({ lat: 30, lng: 110, altitude: 2.1 }, 0)
  updateData()

  resizeObserver = new ResizeObserver(() => resize())
  resizeObserver.observe(hostEl.value)

  themeObserver = new MutationObserver(() => applyMaterial())
  themeObserver.observe(document.body, { attributes: true, attributeFilter: ['class'] })
}

watch(() => props.geo, updateData, { deep: true })

onBeforeUnmount(() => {
  destroyed = true
  if (resumeTimer) clearTimeout(resumeTimer)
  resizeObserver?.disconnect()
  themeObserver?.disconnect()
  if (controls) {
    controls.removeEventListener('start', stopAutoRotate)
    controls.removeEventListener('end', scheduleResume)
  }
  specular?.dispose()
  specular = null
  if (globe) {
    globe._destructor()
    globe = null
  }
  if (hostEl.value) hostEl.value.replaceChildren()
})
</script>

<style scoped>
.geo-globe-host {
  position: absolute;
  inset: 0;
  overflow: hidden;
  cursor: grab;
  touch-action: none;
}
.geo-globe-host:active {
  cursor: grabbing;
}
.geo-globe-host :deep(canvas) {
  background: transparent !important;
  outline: none;
}
</style>
