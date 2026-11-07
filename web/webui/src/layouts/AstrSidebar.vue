<template>
  <v-navigation-drawer
    v-model="drawer"
    :rail="isRail"
    :permanent="!smAndDown"
    :temporary="smAndDown"
    :mobile-breakpoint="0"
    elevation="0"
    rail-width="56"
    width="245"
    location="left"
    floating
    app
    class="leftSidebar"
  >
    <div class="sidebar-container">
      <div class="dashboard-sidebar-brand" :class="{ collapsed: isRail }">
        <div v-if="!isRail" class="dashboard-sidebar-brand-title Outfit">
          <svg class="dashboard-sidebar-brand-logo" viewBox="0 0 24 24" aria-hidden="true" focusable="false" xmlns="http://www.w3.org/2000/svg">
            <g transform="matrix(0.048 0 0 0.048 1.8 0.16)"><g transform="translate(0.8 0.9)"><g transform="translate(0 32)">
              <path d="m246.3 328.1-17.8 41.2c-6.4 14.8-26.9 14.8-33.3 0l-17.8-41.2c-14.9-34.2-41.8-61.4-75.3-76.3l-48.8-21.6c-14.7-6.5-14.7-27.9 0-34.4l47.2-21c34.4-15.3 61.7-43.5 76.4-78.8l18-43.7c6.3-15.2 27.3-15.2 33.6 0l18 43.7c14.7 35.3 42 63.6 76.4 78.8l47.2 21c14.7 6.5 14.7 27.9 0 34.4l-48.8 21.6c-33.5 14.8-60.4 42.1-75.3 76.2z" fill="#2f86bd" transform="translate(0 35)" />
              <path d="m402.2 449.3-5.3 12.2c-3.5 7.9-14.4 7.9-17.9 0l-5.3-12.2c-8.4-19.3-23.6-34.6-42.4-43l-15.4-6.9c-7.9-3.5-7.9-14.9 0-18.4l14.5-6.5c19.4-8.6 34.8-24.5 43.1-44.5l5.4-13.1c3.4-8.1 14.6-8.1 18 0l5.4 13.1c8.3 19.9 23.7 35.8 43.1 44.5l14.5 6.5c7.9 3.5 7.9 14.9 0 18.4l-15.4 6.9c-19 8.3-34.1 23.7-42.5 43z" fill="#2f86bd" transform="matrix(0.95 0 0 0.95 22 -278)" />
            </g></g></g>
          </svg>
          <span class="dashboard-sidebar-brand-name">DokiTier</span>
        </div>
        <button v-else type="button" class="dashboard-sidebar-brand-toggle dashboard-sidebar-rail-btn" @click="toggle">
          <PanelLeft :size="20" />
        </button>
        <v-btn
          v-if="!isRail"
          class="dashboard-sidebar-brand-toggle"
          icon
          rounded="sm"
          variant="text"
          :ripple="false"
          @click="toggle"
        >
          <PanelLeft :size="20" class="dashboard-sidebar-panel-toggle-icon" />
        </v-btn>
      </div>

      <v-list :class="['dashboard-sidebar-list', 'listitem', 'flex-grow-1', { 'hidden-scrollbar': isRail }]">
        <div v-show="!isRail" class="sidebar-group-header">{{ t('nav.system') }}</div>
        <v-list-item
          v-for="item in navItems"
          :key="item.to"
          :to="item.to"
          rounded="lg"
          :ripple="false"
          color="secondary"
          :style="{ '--indent-padding': '0px' }"
          class="leftPadding dashboard-nav-item"
        >
          <template #prepend>
            <component :is="item.icon" :size="18" class="sidebar-lucide-icon hide-menu" />
          </template>
          <v-list-item-title class="dashboard-nav-item__title">{{ t(item.titleKey) }}</v-list-item-title>
          <v-tooltip v-if="isRail" activator="parent" location="right">{{ t(item.titleKey) }}</v-tooltip>
        </v-list-item>
      </v-list>

      <div class="sidebar-footer">
        <v-btn class="sidebar-footer-btn" :class="{ 'sidebar-footer-icon-btn': isRail }" variant="text" :icon="isRail" to="/settings">
          <Settings :size="20" />
          <span v-if="!isRail">{{ t('nav.settings') }}</span>
          <v-tooltip v-if="isRail" activator="parent" location="right">{{ t('nav.settings') }}</v-tooltip>
        </v-btn>
        <div v-if="!isRail" class="sidebar-footer-version">v2.7.0</div>
      </div>
    </div>
  </v-navigation-drawer>
</template>

<script setup>
import { computed, markRaw, ref, watch } from 'vue'
import { useRoute } from 'vue-router'
import { useDisplay } from 'vuetify'
import { useI18n } from 'vue-i18n'
import {
  Activity, Compass, LayoutDashboard, Link2, PanelLeft, Route, Server, Settings,
} from '@lucide/vue'

const { t } = useI18n()
const { smAndDown } = useDisplay()
const route = useRoute()
const drawer = ref(!smAndDown.value)
const rail = ref(localStorage.getItem('dokitier-sidebar-rail') === '1')
const isRail = computed(() => rail.value && !smAndDown.value)

const navItems = [
  { titleKey: 'nav.dashboard', icon: markRaw(LayoutDashboard), to: '/dashboard' },
  { titleKey: 'nav.topology', icon: markRaw(Compass), to: '/topology' },
  { titleKey: 'nav.nodes', icon: markRaw(Server), to: '/nodes' },
  { titleKey: 'nav.links', icon: markRaw(Link2), to: '/links' },
  { titleKey: 'nav.routes', icon: markRaw(Route), to: '/routes' },
  { titleKey: 'nav.history', icon: markRaw(Activity), to: '/history' },
]

watch(rail, (value) => localStorage.setItem('dokitier-sidebar-rail', value ? '1' : '0'))
watch(smAndDown, (mobile) => { if (mobile) drawer.value = false })
watch(() => route.fullPath, () => { if (smAndDown.value) drawer.value = false })

function toggle() {
  if (smAndDown.value) drawer.value = !drawer.value
  else rail.value = !rail.value
}
</script>

<style scoped>
.leftSidebar {
  top: 0 !important;
  height: 100dvh !important;
  border-right: 0 !important;
  background: rgb(var(--v-theme-surface)) !important;
  box-shadow: none !important;
  z-index: 1007 !important;
  user-select: none;
}
.leftSidebar :deep(.v-navigation-drawer__content) { display: flex; height: 100%; flex-direction: column; }
.leftSidebar :deep(.v-navigation-drawer__border) { display: none !important; }
.sidebar-container { display: flex; height: 100%; flex-direction: column; padding-top: 4px; box-sizing: border-box; }
.dashboard-sidebar-brand {
  display: flex; min-height: 40px; align-items: center; justify-content: space-between;
  gap: 10px; padding: 0 16px 2px 24px;
}
.dashboard-sidebar-brand.collapsed { width: 56px; justify-content: center; padding: 0; }
.dashboard-sidebar-brand-title {
  display: flex; min-width: 0; align-items: center; gap: 8px;
  color: rgb(var(--v-theme-on-surface)); line-height: 1.05;
  text-decoration: none !important;
}
.dashboard-sidebar-brand-title:hover,
.dashboard-sidebar-brand-title:focus,
.dashboard-sidebar-brand-title:visited { text-decoration: none !important; }
.dashboard-sidebar-brand-logo { display: block; width: 22px; height: 22px; flex: 0 0 22px; }
.dashboard-sidebar-brand-name { font-size: 18px; font-weight: 800; text-decoration: none !important; }
.dashboard-sidebar-brand-toggle {
  width: 36px; height: 36px; min-width: 36px; background: transparent !important;
  box-shadow: none !important; color: rgba(var(--v-theme-on-surface), 0.56);
}
.dashboard-sidebar-brand-toggle:hover { color: rgba(var(--v-theme-on-surface), 0.9); }
.dashboard-sidebar-rail-btn { position: relative; display: grid; place-items: center; border: 0; border-radius: 8px; cursor: pointer; }
.dashboard-sidebar-rail-icon-stack { display: grid; width: 24px; height: 24px; place-items: center; }
.dashboard-sidebar-rail-icon-stack > * { grid-area: 1 / 1; }
.dashboard-sidebar-panel-toggle-icon { opacity: 1; }
.sidebar-group-header:first-child { padding-top: 4px; }
.sidebar-group-header {
  display: flex; align-items: center; justify-content: space-between;
  padding: 16px 10px 8px; color: rgba(var(--v-theme-on-surface), 0.45);
  font-size: 11px; font-weight: 600; letter-spacing: .08em; line-height: 16px;
  text-transform: uppercase; user-select: none;
}
.dashboard-sidebar-list { flex: 1 1 auto; overflow-y: auto; padding: 2px 16px 12px !important; }
.listitem { overflow-y: auto; }
.leftSidebar :deep(.dashboard-nav-item) {
  min-height: 36px !important;
  margin-bottom: 2px !important;
  padding-inline-start: 10px !important;
  border-radius: 8px !important;
  color: rgba(var(--v-theme-on-surface), 0.76) !important;
}
.leftSidebar :deep(.dashboard-nav-item:hover) {
  background: rgba(var(--v-theme-on-surface), 0.05) !important;
  color: rgb(var(--v-theme-on-surface)) !important;
}
.leftSidebar :deep(.dashboard-nav-item.v-list-item--active) {
  background: rgba(var(--v-theme-primary), 0.08) !important;
  color: rgb(var(--v-theme-primary)) !important;
}
.v-theme--PurpleThemeDark .leftSidebar :deep(.dashboard-nav-item:hover),
.v-theme--PurpleThemeDark .leftSidebar :deep(.dashboard-nav-item.v-list-item--active) {
  color: rgb(var(--v-theme-primary)) !important;
}
.leftSidebar :deep(.dashboard-nav-item__title) {
  font-size: 14px;
  font-weight: 500;
  line-height: 20px;
  letter-spacing: normal !important;
}
.leftSidebar :deep(.dashboard-nav-item .v-list-item-title) {
  font-size: 14px;
  font-weight: 500;
  line-height: 20px;
}
.leftSidebar :deep(.dashboard-nav-item .v-list-item__prepend) {
  display: flex;
  align-items: center;
  min-width: 0;
  margin-inline-end: 12px;
}
.leftSidebar :deep(.dashboard-nav-item .v-list-item__spacer) { width: 0; }
.sidebar-lucide-icon { flex: 0 0 auto; color: currentcolor; stroke-width: 2; }
.sidebar-footer { display: flex; flex-direction: column; padding: 8px 16px 14px; flex-shrink: 0; }
.sidebar-footer-btn {
  justify-content: flex-start; width: 100%;
  min-height: 36px; padding-inline: 10px; margin-bottom: 0 !important; border-radius: 8px;
  font-size: 14px; font-weight: 500; letter-spacing: 0; text-transform: none;
  color: rgba(var(--v-theme-on-surface), .76);
}
.sidebar-footer-btn :deep(.v-btn__content) { justify-content: flex-start; gap: 10px; }
.sidebar-footer-btn:hover { background: rgba(var(--v-theme-on-surface), .08) !important; color: rgb(var(--v-theme-on-surface)); }
.sidebar-footer-btn.v-btn--active { background: rgba(var(--v-theme-primary), .09) !important; color: rgb(var(--v-theme-primary)) !important; }
.sidebar-footer-version { margin-top: 2px !important; padding-inline: 10px; color: rgba(var(--v-theme-on-surface), .46); font-size: 11px; font-weight: 500; }

/* rail */
.leftSidebar.v-navigation-drawer--rail .sidebar-container { padding-top: 4px; }
.leftSidebar.v-navigation-drawer--rail .dashboard-sidebar-list { padding: 2px 10px 12px !important; }
.leftSidebar.v-navigation-drawer--rail .listitem :deep(.v-list-item) {
  width: 36px; min-width: 36px; max-width: 36px; min-height: 36px;
  margin: 0 auto 2px !important; padding: 0 !important;
  display: flex !important; align-items: center !important; justify-content: center !important;
}
.leftSidebar.v-navigation-drawer--rail .listitem :deep(.v-list-item__prepend) {
  margin: 0 !important; width: 100%; min-width: 0; justify-content: center; padding: 0 !important;
}
.leftSidebar.v-navigation-drawer--rail .listitem :deep(.v-list-item__spacer),
.leftSidebar.v-navigation-drawer--rail .listitem :deep(.v-list-item__append),
.leftSidebar.v-navigation-drawer--rail .listitem :deep(.v-list-item__content) { display: none; }
.leftSidebar.v-navigation-drawer--rail .listitem :deep(.v-list-item .v-icon) { margin: 0 !important; }
.leftSidebar.v-navigation-drawer--rail .sidebar-footer { width: 100%; padding: 12px 10px 18px !important; gap: 8px; }
.leftSidebar.v-navigation-drawer--rail .sidebar-footer-btn { width: 56px !important; min-width: 56px !important; max-width: 56px !important; height: 52px !important; border-radius: 14px !important; margin: 0 auto !important; padding: 0 !important; justify-content: center !important; }
.outfit { font-family: Outfit, sans-serif; }
</style>
