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
const emit = defineEmits(['select-city'])

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
  const bps = Number(link.traffic || 0)
  if (bps >= 4 * 1024 * 1024) return 'rgba(120, 255, 190, 0.98)'
  if (bps >= 1024 * 1024) return 'rgba(70, 225, 175, 0.92)'
  if (bps >= 128 * 1024) return 'rgba(66, 190, 210, 0.85)'
  if (bps >= 8 * 1024) return 'rgba(90, 150, 200, 0.62)'
  return 'rgba(150, 175, 205, 0.35)'
}

function arcStroke(link) {
  const bps = Math.max(1, Number(link.traffic || 0))
  // 1 KB/s -> thin, 10 MB/s -> thick, log-scaled
  return 0.18 + Math.min(1.15, Math.log10(bps / 1024 + 1) * 0.42)
}

// Half sphere sitting on the globe surface (globe radius is 100 units), with a
// hard upper bound so a single-node region cannot become a giant dot.
function domeRadius(city) {
  return Math.min(1.35, 0.5 + Math.log2((city.nodes || 1) + 1) * 0.34)
}

function createCityDome(city) {
  const radius = domeRadius(city)
  const geometry = new three.SphereGeometry(radius, 24, 12, 0, Math.PI * 2, 0, Math.PI / 2)
  const color = colorOf(city.group)
  const material = new three.MeshPhongMaterial({
    color,
    emissive: color,
    emissiveIntensity: 0.35,
    shininess: 26,
    transparent: true,
    opacity: 0.96,
  })
  const mesh = new three.Mesh(geometry, material)
  mesh.renderOrder = 2
  mesh.userData.city = city
  domeObjects.set(mesh, city)
  return mesh
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

let lastPointsSig = ''
let lastArcsSig = ''
let lastArcUpdate = 0
const domeObjects = new Map()
let clickStart = null

function updateData() {
  if (!globe) return
  const cities = (props.geo.cities || []).filter((c) => Number.isFinite(c.lat) && Number.isFinite(c.lon))
  const links = (props.geo.links || []).filter((l) => Number.isFinite(l.a_lat) && Number.isFinite(l.a_lon) && Number.isFinite(l.b_lat) && Number.isFinite(l.b_lon))
  // The globe must not rebuild its objects every second: points/labels only
  // change when the city set changes, arcs refresh at most every ~1.5s and
  // only when the traffic bucket changed.
  const pointsSig = cities.map((c) => `${c.city}:${c.lat.toFixed(3)},${c.lon.toFixed(3)}:${c.nodes}:${c.group}`).join('|')
  if (pointsSig !== lastPointsSig) {
    lastPointsSig = pointsSig
    domeObjects.clear()
    globe.customLayerData(cities).htmlElementsData(cities)
  }
  const arcsSig = links
    .map((l) => `${l.a_city}>${l.b_city}:${Math.round((l.traffic || 0) / 1024)}:${Math.round((l.latency_ms || 0) * 10)}`)
    .join('|')
  const now = Date.now()
  if (arcsSig !== lastArcsSig && now - lastArcUpdate > 1500) {
    lastArcsSig = arcsSig
    lastArcUpdate = now
    globe.arcsData(links)
  }
}

function createLabelElement(city) {
  const root = document.createElement('div')
  root.className = 'geo-city-label'
  const name = document.createElement('span')
  name.className = 'geo-city-label__name'
  name.textContent = city.city || ''
  const count = document.createElement('span')
  count.className = 'geo-city-label__count'
  count.textContent = String(city.nodes || 0)
  root.appendChild(name)
  root.appendChild(count)
  return root
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
    .customLayerData([])
    .customThreeObject((city) => createCityDome(city))
    .customThreeObjectUpdate((object, city) => {
      const coords = globe.getCoords(city.lat, city.lon, 0.005)
      Object.assign(object.position, coords)
      const normal = object.position.clone().normalize()
      object.quaternion.setFromUnitVectors(new three.Vector3(0, 1, 0), normal)
    })
    .customLayerLabel(pointLabel)
    .htmlElementsData([])
    .htmlLat('lat')
    .htmlLng('lon')
    .htmlAltitude(0.012)
    .htmlElement(createLabelElement)
    .htmlElementVisibilityModifier((el, visible) => {
      el.style.opacity = visible ? '1' : '0'
    })
    .htmlTransitionDuration(0)
    .arcsData([])
    .arcStartLat('a_lat')
    .arcStartLng('a_lon')
    .arcEndLat('b_lat')
    .arcEndLng('b_lon')
    .arcColor(arcColor)
    .arcStroke(arcStroke)
    .arcAltitudeAutoScale(0.38)
    // traffic is encoded in colour + width; arcs are solid so they never look
    // broken in the middle (dash length 1, no gap, no animation)
    .arcDashLength(1)
    .arcDashGap(0)
    .arcDashAnimateTime(0)
    .arcsTransitionDuration(0)

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

  const canvas = renderer.domElement
  canvas.addEventListener('pointerdown', (event) => {
    clickStart = { x: event.clientX, y: event.clientY }
  })
  canvas.addEventListener('click', (event) => {
    if (!clickStart) return
    const moved = Math.hypot(event.clientX - clickStart.x, event.clientY - clickStart.y)
    clickStart = null
    if (moved > 5) return
    const rect = canvas.getBoundingClientRect()
    const mouse = new three.Vector2(
      ((event.clientX - rect.left) / rect.width) * 2 - 1,
      -((event.clientY - rect.top) / rect.height) * 2 + 1,
    )
    const raycaster = new three.Raycaster()
    raycaster.setFromCamera(mouse, globe.camera())
    const hits = raycaster.intersectObjects([...domeObjects.keys()], false)
    if (hits.length) {
      const city = hits[0].object.userData.city
      if (city) emit('select-city', city)
    }
  })

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
.geo-globe-host :deep(.geo-city-label) {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  transform: translate(-50%, -140%);
  padding: 1px 6px;
  border-radius: 6px;
  background: rgba(10, 16, 24, 0.72);
  border: 1px solid rgba(120, 180, 230, 0.28);
  color: #e8f1fb;
  font: 600 12px/1.4 'Noto Sans', sans-serif;
  white-space: nowrap;
  pointer-events: none;
  text-shadow: 0 1px 2px rgba(0, 0, 0, 0.6);
  backdrop-filter: blur(2px);
}
.geo-globe-host :deep(.geo-city-label__count) {
  color: #7fe0b8;
  font-weight: 700;
}
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
