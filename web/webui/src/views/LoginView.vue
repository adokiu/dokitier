<template>
  <div class="login-page-container">
    <v-card class="login-card" elevation="1">
      <v-card-title>
        <div class="d-flex justify-space-between align-center w-100">
          <svg width="80" height="80" viewBox="0 0 24 24" fill="none" xmlns="http://www.w3.org/2000/svg" aria-hidden="true">
            <g transform="matrix(0.048 0 0 0.048 1.8 0.16)"><g transform="translate(0.8 0.9)"><g transform="translate(0 32)">
              <path d="m246.3 328.1-17.8 41.2c-6.4 14.8-26.9 14.8-33.3 0l-17.8-41.2c-14.9-34.2-41.8-61.4-75.3-76.3l-48.8-21.6c-14.7-6.5-14.7-27.9 0-34.4l47.2-21c34.4-15.3 61.7-43.5 76.4-78.8l18-43.7c6.3-15.2 27.3-15.2 33.6 0l18 43.7c14.7 35.3 42 63.6 76.4 78.8l47.2 21c14.7 6.5 14.7 27.9 0 34.4l-48.8 21.6c-33.5 14.8-60.4 42.1-75.3 76.2z" fill="#2f86bd" transform="translate(0 35)"/>
              <path d="m402.2 449.3-5.3 12.2c-3.5 7.9-14.4 7.9-17.9 0l-5.3-12.2c-8.4-19.3-23.6-34.6-42.4-43l-15.4-6.9c-7.9-3.5-7.9-14.9 0-18.4l14.5-6.5c19.4-8.6 34.8-24.5 43.1-44.5l5.4-13.1c3.4-8.1 14.6-8.1 18 0l5.4 13.1c8.3 19.9 23.7 35.8 43.1 44.5l14.5 6.5c7.9 3.5 7.9 14.9 0 18.4l-15.4 6.9c-19 8.3-34.1 23.7-42.5 43z" fill="#2f86bd" transform="matrix(0.95 0 0 0.95 22 -278)"/>
            </g></g></g>
          </svg>
          <div class="d-flex align-center gap-1">
            <v-menu location="bottom center" offset="12">
              <template #activator="{ props: langProps }">
                <v-btn v-bind="langProps" class="language-switcher" icon variant="text" size="small">
                  <v-icon size="18" color="rgb(var(--v-theme-primary))">mdi-translate</v-icon>
                  <v-tooltip activator="parent" location="top">语言</v-tooltip>
                </v-btn>
              </template>
              <v-card class="styled-menu-card" elevation="8" rounded="lg" min-width="150">
                <v-list density="compact" class="pa-1">
                  <v-list-item class="styled-menu-item" rounded="md" @click="setLocale('zh')">
                    <template #prepend><img class="language-flag-img" src="https://flagcdn.com/cn.svg" alt="CN" loading="lazy" /></template>
                    <v-list-item-title>简体中文</v-list-item-title>
                  </v-list-item>
                  <v-list-item class="styled-menu-item" rounded="md" @click="setLocale('en')">
                    <template #prepend><img class="language-flag-img" src="https://flagcdn.com/us.svg" alt="US" loading="lazy" /></template>
                    <v-list-item-title>English</v-list-item-title>
                  </v-list-item>
                </v-list>
              </v-card>
            </v-menu>
            <v-divider vertical class="mx-1" style="height: 24px; opacity: .9; align-self: center; border-color: rgba(var(--v-theme-primary), .45)" />
            <v-menu open-on-click location="bottom center" offset="6">
              <template #activator="{ props: themeProps }">
                <v-btn v-bind="themeProps" class="theme-toggle-btn" icon variant="text" size="small">
                  <v-icon size="18" color="rgb(var(--v-theme-primary))">{{ themeIcon }}</v-icon>
                  <v-tooltip activator="parent" location="top">主题</v-tooltip>
                </v-btn>
              </template>
              <v-card class="styled-menu-card" elevation="8" rounded="lg" min-width="150">
                <v-list density="compact" class="pa-1">
                  <v-list-item v-for="option in themeOptions" :key="option.mode" class="styled-menu-item" rounded="md" @click="setThemeMode(option.mode)">
                    <template #prepend><v-icon size="16" class="mr-2" style="opacity:.85">{{ option.icon }}</v-icon></template>
                    <v-list-item-title>{{ option.label }}</v-list-item-title>
                  </v-list-item>
                </v-list>
              </v-card>
            </v-menu>
          </div>
        </div>
        <div class="ml-2" style="font-size: 26px">{{ t('login.title') }}</div>
        <div class="mt-2 ml-2" style="font-size: 14px; color: grey">{{ t('login.subtitle') }}</div>
      </v-card-title>

      <v-card-text>
        <div class="mt-4 login-form">
          <form autocomplete="on" @submit.prevent="submit">
            <v-text-field
              v-model="username"
              :label="t('login.username')"
              name="username"
              autocomplete="username"
              class="mb-6 input-field"
              required
              hide-details="auto"
              variant="outlined"
              prepend-inner-icon="mdi-account"
              :disabled="loading"
            />
            <v-text-field
              v-model="password"
              :label="t('login.password')"
              name="password"
              autocomplete="current-password"
              required
              variant="outlined"
              hide-details="auto"
              :append-inner-icon="showPassword ? 'mdi-eye' : 'mdi-eye-off'"
              :type="showPassword ? 'text' : 'password'"
              class="pwd-input"
              prepend-inner-icon="mdi-lock"
              :disabled="loading"
              @click:append-inner="showPassword = !showPassword"
            />
            <v-text-field
              v-if="totpEnabled"
              v-model="totp"
              label="动态验证码（启用 TOTP 时填写）"
              name="totp"
              inputmode="numeric"
              maxlength="6"
              variant="outlined"
              hide-details="auto"
              class="mt-3"
              prepend-inner-icon="mdi-shield-key"
              :disabled="loading"
            />
            <div v-if="error" class="mt-4">
              <v-alert color="error" variant="tonal" icon="mdi-alert-circle" border="start">{{ error }}</v-alert>
            </div>
            <v-btn
              type="submit"
              color="secondary"
              block
              class="login-btn mt-8"
              variant="flat"
              size="large"
              :loading="loading"
              :disabled="loading || !username || !password"
            >
              <span class="login-btn-text">{{ t('login.submit') }}</span>
            </v-btn>
          </form>
        </div>
      </v-card-text>

      <div class="login-version-info">
        <span class="login-version-item"><span class="login-version-label">WebUI</span>v2.7.0</span>
      </div>
    </v-card>
  </div>
</template>

<script setup>
import { computed, ref } from 'vue'
import { useRouter } from 'vue-router'
import { useTheme } from 'vuetify'
import { useI18n } from 'vue-i18n'
import { setLocale } from '@/i18n'
import { onMounted } from 'vue'
import { api } from '@/api'

const username = ref('admin')
const password = ref('')
const totp = ref('')
const totpEnabled = ref(false)
const error = ref('')
const loading = ref(false)
const showPassword = ref(false)
const router = useRouter()
const theme = useTheme()
const { t, locale } = useI18n()
const themeMode = ref(localStorage.getItem('dokitier_theme_mode') || 'light')
const themeOptions = [
  { mode: 'light', icon: 'mdi-white-balance-sunny', label: '浅色模式' },
  { mode: 'dark', icon: 'mdi-weather-night', label: '深色模式' },
  { mode: 'system', icon: 'mdi-sync', label: '跟随系统' },
]
const themeIcon = computed(() => themeOptions.find((o) => o.mode === themeMode.value)?.icon || 'mdi-white-balance-sunny')
setThemeMode(themeMode.value)

function setThemeMode(mode) {
  themeMode.value = mode
  localStorage.setItem('dokitier_theme_mode', mode)
  const dark = mode === 'dark' || (mode === 'system' && window.matchMedia('(prefers-color-scheme: dark)').matches)
  const name = dark ? 'PurpleThemeDark' : 'PurpleTheme'
  theme.change(name)
  localStorage.setItem('dokitier_theme', name)
}

onMounted(async () => {
  try {
    const status = await api.authStatus()
    totpEnabled.value = status.totp_enabled === true
  } catch {}
})

async function submit() {
  if (loading.value) return
  if (!username.value || !password.value) {
    error.value = t('login.missing')
    return
  }
  loading.value = true
  error.value = ''
  try {
    await api.login(username.value, password.value, totp.value.trim() || undefined)
    router.push('/dashboard')
  } catch (e) {
    if (String(e.message || '').includes('totp')) totpEnabled.value = true
    error.value = e.message === 'unauthorized' ? t('login.wrong') : String(e.message || e)
  } finally {
    loading.value = false
  }
}
</script>

<style scoped>
.login-page-container {
  background-color: rgb(var(--v-theme-containerBg));
  position: relative;
  width: 100vw;
  height: 100vh;
  overflow: hidden;
  display: flex;
  justify-content: center;
  align-items: center;
}
.login-card {
  width: 400px !important;
  max-width: calc(100vw - 32px);
  padding: 8px;
}
.login-version-info {
  align-items: center;
  color: rgba(var(--v-theme-on-surface), 0.56);
  display: flex;
  flex-wrap: wrap;
  gap: 4px 8px;
  justify-content: center;
  line-height: 1.45;
  padding: 0 14px 10px;
  text-align: center;
  font-size: 12px;
}
.login-version-item { max-width: 100%; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.login-version-label { margin-right: 4px; }
.language-flag-img { width: 18px; height: 13px; border-radius: 2px; object-fit: cover; margin-right: 10px; }
.language-switcher { width: 32px !important; min-width: 32px !important; height: 32px !important; border-radius: 50% !important; margin: 0 4px; }
</style>
