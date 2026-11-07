<template>
  <div class="login-page">
    <v-card class="login-card" :class="{ 'is-visible': visible }">
      <div class="login-card__head">
        <div class="login-card__top">
          <div class="login-card__logo">
            <v-icon icon="mdi-lan-connect" size="64" color="primary" />
          </div>
          <v-btn icon variant="text" size="small" @click="toggleTheme">
            <Sun v-if="isDark" :size="18" />
            <Moon v-else :size="18" />
          </v-btn>
        </div>
        <div class="login-card__title">登录 dokitier</div>
        <div class="login-card__subtitle">节点控制台，请输入控制台密码</div>
      </div>

      <div class="login-card__body">
        <v-alert
          v-if="error"
          class="login-error"
          type="error"
          variant="tonal"
          density="compact"
          icon="mdi-alert-circle-outline"
        >{{ error }}</v-alert>

        <v-text-field
          v-model="username"
          label="管理员用户名"
          prepend-inner-icon="mdi-account-outline"
          autocomplete="username"
          density="comfortable"
          :disabled="loading"
          class="mb-3"
        />
        <v-text-field
          v-model="password"
          label="控制台密码"
          prepend-inner-icon="mdi-lock-outline"
          :type="showPassword ? 'text' : 'password'"
          :append-inner-icon="showPassword ? 'mdi-eye-off-outline' : 'mdi-eye-outline'"
          autocomplete="current-password"
          density="comfortable"
          :disabled="loading"
          @click:append-inner="showPassword = !showPassword"
          @keyup.enter="submit"
        />
        <v-btn
          class="login-btn mt-4"
          color="primary"
          variant="flat"
          block
          :loading="loading"
          @click="submit"
        >登录</v-btn>
      </div>

      <div class="login-card__footer">dokitier v2.7.0</div>
    </v-card>
  </div>
</template>

<script setup>
import { computed, onMounted, ref } from 'vue'
import { useRouter } from 'vue-router'
import { useTheme } from 'vuetify'
import { Moon, Sun } from '@lucide/vue'
import { api } from '@/api'

const username = ref('admin')
const password = ref('')
const error = ref('')
const loading = ref(false)
const visible = ref(false)
const showPassword = ref(false)
const router = useRouter()
const theme = useTheme()
const isDark = computed(() => theme.global.current.value.dark)

onMounted(() => setTimeout(() => { visible.value = true }, 80))

function toggleTheme() {
  theme.global.name.value = isDark.value ? 'PurpleTheme' : 'PurpleThemeDark'
}

async function submit() {
  if (loading.value) return
  if (!username.value || !password.value) {
    error.value = '请输入用户名和密码'
    return
  }
  loading.value = true
  error.value = ''
  try {
    await api.login(username.value, password.value)
    router.push('/dashboard')
  } catch (e) {
    error.value = e.message === 'unauthorized' ? '密码错误' : String(e.message || e)
  } finally {
    loading.value = false
  }
}
</script>
