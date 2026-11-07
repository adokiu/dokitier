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
        <router-link v-if="!isRail" to="/dashboard" class="dashboard-sidebar-brand-title Outfit">
          <v-icon icon="mdi-lan-connect" size="22" color="primary" class="dashboard-sidebar-brand-logo" />
          <span class="dashboard-sidebar-brand-name">dokitier</span>
        </router-link>
        <button v-else type="button" class="dashboard-sidebar-brand-toggle dashboard-sidebar-rail-btn" @click="toggle">
          <span class="dashboard-sidebar-rail-icon-stack">
            <v-icon icon="mdi-lan-connect" size="23" color="primary" />
            <PanelLeft :size="16" class="dashboard-sidebar-panel-toggle-icon" />
          </span>
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
          :title="isRail ? undefined : t(item.titleKey)"
          rounded="lg"
          class="leftPadding dashboard-nav-item"
        >
          <template #prepend>
            <component :is="item.icon" :size="18" class="sidebar-nav-icon" />
          </template>
          <v-tooltip v-if="isRail" activator="parent" location="right">{{ t(item.titleKey) }}</v-tooltip>
        </v-list-item>
      </v-list>

      <div class="sidebar-footer">
        <v-btn class="sidebar-footer-btn" :class="{ 'sidebar-footer-icon-btn': isRail }" variant="text" :icon="isRail" to="/settings">
          <Settings :size="20" />
          <span v-if="!isRail">{{ t('nav.settings') }}</span>
          <v-tooltip v-if="isRail" activator="parent" location="right">{{ t('nav.settings') }}</v-tooltip>
        </v-btn>
        <div v-if="!isRail" class="sidebar-footer-version">dokitier v2.7.0</div>
      </div>
    </div>
  </v-navigation-drawer>
</template>

<script setup>
import { computed, markRaw, ref, watch } from 'vue'
import { useDisplay } from 'vuetify'
import { useI18n } from 'vue-i18n'
import {
  Activity, Compass, LayoutDashboard, Link2, PanelLeft, Route, Server, Settings,
} from '@lucide/vue'

const { t } = useI18n()
const { smAndDown } = useDisplay()
const drawer = ref(true)
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
.sidebar-container { display: flex; height: 100%; flex-direction: column; padding-top: 4px; box-sizing: border-box; }
.dashboard-sidebar-brand {
  display: flex; min-height: 40px; align-items: center; justify-content: space-between;
  gap: 10px; padding: 0 12px 0 20px;
}
.dashboard-sidebar-brand.collapsed { width: 56px; justify-content: center; padding: 0; }
.dashboard-sidebar-brand-title { display: flex; min-width: 0; align-items: center; gap: 8px; color: rgb(var(--v-theme-on-surface)); line-height: 1.05; }
.dashboard-sidebar-brand-logo { display: block; width: 22px; height: 22px; }
.dashboard-sidebar-brand-name { font-size: 18px; font-weight: 800; }
.dashboard-sidebar-brand-toggle {
  width: 36px; height: 36px; min-width: 36px; background: transparent !important;
  box-shadow: none !important; color: rgba(var(--v-theme-on-surface), 0.56);
}
.dashboard-sidebar-brand-toggle:hover { color: rgba(var(--v-theme-on-surface), 0.9); }
.dashboard-sidebar-rail-btn { position: relative; display: grid; place-items: center; border: 0; border-radius: 8px; cursor: pointer; }
.dashboard-sidebar-rail-icon-stack { position: relative; display: grid; place-items: center; }
.dashboard-sidebar-panel-toggle-icon { position: absolute; right: -8px; bottom: -5px; opacity: .55; }
.sidebar-group-header {
  display: flex; align-items: center; justify-content: space-between;
  padding: 16px 10px 8px; color: rgba(var(--v-theme-on-surface), 0.45);
  font-size: 11px; font-weight: 600; letter-spacing: .08em; line-height: 16px;
  text-transform: uppercase; user-select: none;
}
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
.leftSidebar :deep(.dashboard-nav-item .v-list-item__prepend) {
  min-width: 0;
  margin-inline-end: 12px;
}
.leftSidebar :deep(.dashboard-nav-item .v-list-item__spacer) { width: 0; }
.sidebar-nav-icon { opacity: .9; }
.sidebar-footer { display: flex; flex-direction: column; padding: 8px 16px 14px; flex-shrink: 0; }
.sidebar-footer-btn { justify-content: flex-start; min-height: 36px; padding-inline: 10px; border-radius: 8px; font-size: 14px; color: rgba(var(--v-theme-on-surface), .76); width: 100%; }
.sidebar-footer-version { margin-top: 2px; padding-inline: 10px; color: rgba(var(--v-theme-on-surface), .46); font-size: 11px; font-weight: 500; }

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
