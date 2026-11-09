import { createVuetify } from 'vuetify'
import { en, zhHans } from 'vuetify/locale'
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
  locale: {
    locale: localStorage.getItem('dokitier_locale') === 'en' ? 'en' : 'zhHans',
    fallback: 'en',
    messages: { en, zhHans },
  },
  defaults: {
    VCard: { rounded: 'lg' },
    VSelect: { menuProps: { attach: 'body' } },
    VAutocomplete: { menuProps: { attach: 'body' } },
    VCombobox: { menuProps: { attach: 'body' } },
    VMenu: { attach: 'body' },
    VSnackbar: { elevation: 6, rounded: 'lg' },
    VTextField: { rounded: 'lg' },
    VTooltip: { location: 'top' },
  },
})
