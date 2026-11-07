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
    VSnackbar: { elevation: 6, rounded: 'lg' },
    VTextField: { rounded: 'lg' },
    VTooltip: { location: 'top' },
  },
})
