import { createVuetify } from 'vuetify'
import * as components from 'vuetify/components'
import * as directives from 'vuetify/directives'
import { PurpleTheme, PurpleThemeDark } from '@/theme'

export default createVuetify({
  components,
  directives,
  theme: {
    defaultTheme: localStorage.getItem('dokitier_theme') || 'PurpleTheme',
    themes: { PurpleTheme, PurpleThemeDark },
  },
  defaults: {
    VCard: { rounded: 'lg' },
    VBtn: { rounded: 'lg' },
    VTextField: { rounded: 'lg', variant: 'outlined', density: 'comfortable' },
    VSelect: { rounded: 'lg', variant: 'outlined', density: 'comfortable' },
    VSnackbar: { rounded: 'lg', elevation: 6 },
  },
})
