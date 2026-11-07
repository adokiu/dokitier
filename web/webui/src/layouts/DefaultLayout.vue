<template>
  <div
    class="admin-shell"
    :class="{ 'is-rail': rail, 'is-mobile': smAndDown, 'is-open': mobileOpen }"
    :style="{ '--admin-sidebar-width': isRail ? '56px' : '245px' }"
  >
    <header class="admin-chrome">
      <div class="admin-chrome__brand">
        <button v-if="smAndDown" type="button" class="admin-menu-btn" @click="toggleRail">
          <Menu :size="20" />
        </button>
        <router-link to="/dashboard" class="admin-brand-link">
          <v-icon icon="mdi-lan-connect" size="24" color="primary" />
          <span v-if="!isRail" class="admin-brand-name">dokitier</span>
        </router-link>
        <button v-if="!smAndDown" type="button" class="admin-menu-btn admin-menu-btn--desktop" @click="toggleRail">
          <PanelLeft :size="18" />
          <v-tooltip activator="parent" location="bottom">{{ isRail ? '展开侧边栏' : '收起侧边栏' }}</v-tooltip>
        </button>
      </div>

      <div class="admin-chrome__context">
        <div class="appbar-title">
          <span class="appbar-title__crumb">dokitier</span>
          <ChevronRight :size="13" class="appbar-title__chevron" />
          <strong>{{ pageTitle }}</strong>
        </div>
        <v-spacer />
        <v-btn size="small" variant="text" @click="toggleLang">{{ locale === 'zh' ? 'EN' : '中' }}</v-btn>
        <v-btn icon variant="text" size="small" @click="toggleTheme">
          <Sun v-if="isDark" :size="18" />
          <Moon v-else :size="18" />
          <v-tooltip activator="parent">切换主题</v-tooltip>
        </v-btn>
        <v-menu location="bottom end" offset="6">
          <template #activator="{ props: menuProps }">
            <button v-bind="menuProps" type="button" class="header-user">
              <span class="header-user__avatar">D</span>
              <span class="header-user__name d-none d-sm-inline">admin</span>
              <ChevronDown :size="14" />
            </button>
          </template>
          <v-card rounded="lg" min-width="188">
            <v-list density="compact" class="pa-1">
              <v-list-item to="/settings" rounded="md">
                <template #prepend><v-icon icon="mdi-tune-variant" size="17" class="mr-2" /></template>
                <v-list-item-title>设置</v-list-item-title>
              </v-list-item>
              <v-list-item rounded="md" @click="logout">
                <template #prepend><v-icon icon="mdi-logout" size="17" class="mr-2" /></template>
                <v-list-item-title>退出登录</v-list-item-title>
              </v-list-item>
            </v-list>
          </v-card>
        </v-menu>
      </div>
    </header>

    <aside class="admin-sidebar">
      <div class="admin-nav">
        <div class="admin-nav__label">{{ isRail ? '' : t('nav.system') }}</div>
        <v-list nav density="compact" class="py-0">
          <v-list-item
            v-for="item in navItems"
            :key="item.to"
            :to="item.to"
            :title="isRail ? undefined : t(item.titleKey)"
            rounded="lg"
          >
            <template #prepend>
              <component :is="item.icon" :size="18" class="admin-nav-icon" />
            </template>
            <v-tooltip v-if="isRail" activator="parent" location="right">{{ t(item.titleKey) }}</v-tooltip>
          </v-list-item>
        </v-list>
      </div>
      <div class="admin-sidebar__footer">
        <v-btn v-if="!isRail" variant="text" size="small" block @click="logout">
          <LogOut :size="16" class="mr-2" />退出登录
        </v-btn>
        <v-btn v-else variant="text" size="small" icon @click="logout">
          <LogOut :size="18" />
          <v-tooltip activator="parent" location="right">退出登录</v-tooltip>
        </v-btn>
        <div v-if="!isRail" class="admin-sidebar__version">dokitier v2.7.0</div>
      </div>
    </aside>

    <main class="admin-main">
      <div class="page-content">
        <router-view />
      </div>
    </main>

    <div v-if="smAndDown && mobileOpen" class="admin-backdrop" @click="mobileOpen = false" />
  </div>
</template>

<script setup>
import { computed, markRaw, ref, watch } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { useI18n } from 'vue-i18n'
import { setLocale } from '@/i18n'
import { useDisplay, useTheme } from 'vuetify'
import {
  Activity,
  ChevronDown,
  ChevronRight,
  Compass,
  LayoutDashboard,
  Link2,
  LogOut,
  Menu,
  Moon,
  PanelLeft,
  Route,
  Server,
  Settings,
  Sun,
} from '@lucide/vue'
import { api } from '@/api'

const route = useRoute()
const { t, locale } = useI18n()
const router = useRouter()
const theme = useTheme()
const { smAndDown } = useDisplay()

const rail = ref(localStorage.getItem('dokitier-sidebar-rail') === '1')
const mobileOpen = ref(false)

const navItems = [
  { titleKey: 'nav.dashboard', icon: markRaw(LayoutDashboard), to: '/dashboard' },
  { titleKey: 'nav.topology', icon: markRaw(Compass), to: '/topology' },
  { titleKey: 'nav.nodes', icon: markRaw(Server), to: '/nodes' },
  { titleKey: 'nav.links', icon: markRaw(Link2), to: '/links' },
  { titleKey: 'nav.routes', icon: markRaw(Route), to: '/routes' },
  { titleKey: 'nav.history', icon: markRaw(Activity), to: '/history' },
  { titleKey: 'nav.settings', icon: markRaw(Settings), to: '/settings' },
]

const pageTitle = computed(() => route.meta.title || '控制台')
const isRail = computed(() => rail.value && !smAndDown.value)
const isDark = computed(() => theme.global.current.value.dark)

watch(smAndDown, (mobile) => { if (!mobile) mobileOpen.value = false })
watch(() => route.fullPath, () => { mobileOpen.value = false })
watch(rail, (value) => localStorage.setItem('dokitier-sidebar-rail', value ? '1' : '0'))

function toggleRail() {
  if (smAndDown.value) mobileOpen.value = !mobileOpen.value
  else rail.value = !rail.value
}

function toggleLang() {
  setLocale(locale.value === 'zh' ? 'en' : 'zh')
}

function toggleTheme() {
  theme.global.name.value = isDark.value ? 'PurpleTheme' : 'PurpleThemeDark'
}

async function logout() {
  try { await api.logout() } catch {}
  router.push('/login')
}
</script>
