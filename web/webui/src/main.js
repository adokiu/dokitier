import { createApp, watch } from 'vue'
import { createPinia } from 'pinia'
import App from './App.vue'
import router from './router'
import vuetify from './plugins/vuetify'
import { i18n } from './i18n'
import './styles/main.scss'

const app = createApp(App)
app.use(createPinia())
app.use(router)
app.use(vuetify)

function applyStoredTheme() {
  const mode = localStorage.getItem('dokitier_theme_mode') || 'light'
  const dark = mode === 'dark' || (mode === 'system' && window.matchMedia('(prefers-color-scheme: dark)').matches)
  const name = dark ? 'PurpleThemeDark' : 'PurpleTheme'
  vuetify.theme.change(name)
  localStorage.setItem('dokitier_theme', name)
}
applyStoredTheme()
window.matchMedia('(prefers-color-scheme: dark)').addEventListener('change', () => {
  if ((localStorage.getItem('dokitier_theme_mode') || 'light') === 'system') applyStoredTheme()
})
app.use(i18n)

// keep Vuetify's own strings (table pagination, etc.) in sync with app language
vuetify.locale.current.value = i18n.global.locale.value === 'en' ? 'en' : 'zhHans'
watch(i18n.global.locale, (locale) => {
  vuetify.locale.current.value = locale === 'en' ? 'en' : 'zhHans'
})

app.mount('#app')

// reload
