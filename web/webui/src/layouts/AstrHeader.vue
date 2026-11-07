<template>
  <v-app-bar elevation="0" height="40" class="top-header">
    <v-btn v-if="smAndDown" class="header-menu-btn ml-2" icon variant="text" @click="$emit('toggle-drawer')">
      <Menu :size="20" />
    </v-btn>
    <v-spacer />
    <div class="header-actions">
      <v-btn size="small" variant="text" @click="toggleLang">{{ locale === 'zh' ? 'EN' : '中' }}</v-btn>
      <v-btn icon variant="text" size="small" @click="toggleTheme">
        <Sun v-if="isDark" :size="18" />
        <Moon v-else :size="18" />
        <v-tooltip activator="parent">{{ t('nav.settings') }}</v-tooltip>
      </v-btn>
      <v-menu location="bottom end" offset="6">
        <template #activator="{ props: menuProps }">
          <button v-bind="menuProps" type="button" class="header-user">
            <span class="header-user__avatar">D</span>
            <span class="header-user__name d-none d-sm-inline">admin</span>
            <ChevronDown :size="14" />
          </button>
        </template>
        <v-card rounded="lg" min-width="188" class="styled-menu-card">
          <v-list density="compact" class="pa-1">
            <v-list-item to="/settings" rounded="md"><v-list-item-title>{{ t('nav.settings') }}</v-list-item-title></v-list-item>
            <v-list-item rounded="md" @click="logout"><v-list-item-title>退出登录</v-list-item-title></v-list-item>
          </v-list>
        </v-card>
      </v-menu>
    </div>
  </v-app-bar>
</template>

<script setup>
import { computed } from 'vue'
import { useRouter } from 'vue-router'
import { useTheme } from 'vuetify'
import { useI18n } from 'vue-i18n'
import { ChevronDown, Menu, Moon, Sun } from '@lucide/vue'
import { setLocale } from '@/i18n'
import { api } from '@/api'

defineEmits(['toggle-drawer'])
const router = useRouter()
const theme = useTheme()
const { t, locale } = useI18n()
const isDark = computed(() => theme.global.current.value.dark)
const smAndDown = computed(() => window.innerWidth < 960)

function toggleLang() { setLocale(locale.value === 'zh' ? 'en' : 'zh') }
function toggleTheme() {
  theme.global.name.value = isDark.value ? 'PurpleTheme' : 'PurpleThemeDark'
  localStorage.setItem('dokitier_theme', theme.global.name.value)
}
async function logout() {
  try { await api.logout() } catch {}
  router.push('/login')
}
</script>

<style scoped>
.top-header {
  position: relative !important;
  top: 0 !important;
  left: 0 !important;
  width: 100% !important;
  border-bottom: 0 !important;
  box-shadow: none !important;
  background: rgb(var(--v-theme-surface)) !important;
  z-index: 1005;
}
.top-header :deep(.v-toolbar__content) { padding-inline: 24px !important; }
.app-header-context { display: flex; align-items: center; gap: 8px; height: 100%; min-width: 0; margin-left: 245px; }
.appbar-title { display: flex; align-items: center; gap: 9px; font-size: 12px; color: var(--dok-muted); }
.appbar-title__crumb { font-size: 11px; color: var(--dok-subtle); }
.appbar-title strong { color: rgb(var(--v-theme-on-surface)); font-weight: 600; font-size: 13px; }
.header-actions { display: flex; align-items: center; gap: 2px; }
.header-user { display: inline-flex; align-items: center; gap: 8px; padding: 3px 8px 3px 4px; border-radius: 999px; cursor: pointer; border: 0; background: transparent; color: inherit; }
.header-user:hover { background: rgba(var(--v-theme-on-surface), .05); }
.header-user__avatar { width: 24px; height: 24px; display: grid; place-items: center; border-radius: 50%; background: rgba(60, 150, 202, .12); color: rgb(var(--v-theme-primary)); font-size: 11px; font-weight: 700; }
.header-user__name { font-size: 12px; color: var(--dok-muted); }
@media (max-width: 959.98px) {
  .app-header-context { margin-left: 0; }
}
</style>
