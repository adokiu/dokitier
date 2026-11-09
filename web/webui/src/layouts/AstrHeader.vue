<template>
  <v-app-bar elevation="0" height="40" class="top-header">
    <v-btn v-if="smAndDown" class="header-menu-btn ml-2" icon variant="text" @click="$emit('toggle-drawer')">
      <Menu :size="20" />
    </v-btn>
    <v-spacer />
    <div class="header-actions">
      <v-menu location="bottom end" offset="6">
        <template #activator="{ props: langProps }">
          <v-btn v-bind="langProps" icon variant="text" size="small">
            <v-icon size="18">mdi-translate</v-icon>
            <v-tooltip activator="parent" location="top">语言</v-tooltip>
          </v-btn>
        </template>
        <v-card rounded="lg" min-width="160" class="styled-menu-card">
          <v-list density="compact" class="pa-1">
            <v-list-item rounded="md" :active="locale === 'zh'" @click="setLocale('zh')">
              <template #prepend><img class="language-flag-img" src="https://flagcdn.com/cn.svg" alt="CN" loading="lazy" /></template>
              <v-list-item-title>简体中文</v-list-item-title>
            </v-list-item>
            <v-list-item rounded="md" :active="locale === 'en'" @click="setLocale('en')">
              <template #prepend><img class="language-flag-img" src="https://flagcdn.com/us.svg" alt="US" loading="lazy" /></template>
              <v-list-item-title>English</v-list-item-title>
            </v-list-item>
          </v-list>
        </v-card>
      </v-menu>
      <v-menu location="bottom end" offset="6">
        <template #activator="{ props: themeProps }">
          <v-btn v-bind="themeProps" icon variant="text" size="small">
            <v-icon size="18">{{ themeIcon }}</v-icon>
            <v-tooltip activator="parent" location="top">主题</v-tooltip>
          </v-btn>
        </template>
        <v-card rounded="lg" min-width="170" class="styled-menu-card">
          <v-list density="compact" class="pa-1">
            <v-list-item v-for="option in themeOptions" :key="option.mode" rounded="md" :active="themeMode === option.mode" @click="setThemeMode(option.mode)">
              <template #prepend><v-icon size="18" class="mr-2">{{ option.icon }}</v-icon></template>
              <v-list-item-title>{{ option.label }}</v-list-item-title>
            </v-list-item>
          </v-list>
        </v-card>
      </v-menu>
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
import { computed, onMounted, ref } from 'vue'
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

const themeMode = ref(localStorage.getItem('dokitier_theme_mode') || 'light')
const themeOptions = [
  { mode: 'light', icon: 'mdi-white-balance-sunny', label: '浅色模式' },
  { mode: 'dark', icon: 'mdi-weather-night', label: '深色模式' },
  { mode: 'system', icon: 'mdi-monitor', label: '跟随系统' },
]
const themeIcon = computed(() => themeOptions.find((o) => o.mode === themeMode.value)?.icon || 'mdi-white-balance-sunny')

function applyTheme(mode) {
  const dark = mode === 'dark' || (mode === 'system' && window.matchMedia('(prefers-color-scheme: dark)').matches)
  const name = dark ? 'PurpleThemeDark' : 'PurpleTheme'
  theme.change(name)
  localStorage.setItem('dokitier_theme', name)
}

function setThemeMode(mode) {
  themeMode.value = mode
  localStorage.setItem('dokitier_theme_mode', mode)
  applyTheme(mode)
}

onMounted(() => {
  applyTheme(themeMode.value)
  window.matchMedia('(prefers-color-scheme: dark)').addEventListener('change', () => {
    if (themeMode.value === 'system') applyTheme('system')
  })
})
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

.language-flag-img { width: 18px; height: 13px; border-radius: 2px; object-fit: cover; margin-right: 10px; }
</style>
