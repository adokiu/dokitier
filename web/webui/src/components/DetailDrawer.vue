<template>
  <Teleport to="body">
    <Transition name="detail-scrim">
      <div v-if="model" class="detail-drawer__scrim" @click="model = false" />
    </Transition>
    <Transition name="detail-slide">
      <aside v-if="model" class="detail-drawer" :style="drawerStyle">
        <div class="detail-drawer__head">
          <div class="detail-drawer__titles">
            <div class="detail-drawer__title">{{ title }}</div>
            <div v-if="subtitle" class="detail-drawer__subtitle">{{ subtitle }}</div>
          </div>
          <v-btn icon variant="text" size="small" @click="model = false">
            <v-icon icon="mdi-close" />
          </v-btn>
        </div>
        <div class="detail-drawer__body">
          <slot />
        </div>
      </aside>
    </Transition>
  </Teleport>
</template>

<script setup>
import { computed } from 'vue'
import { useTheme } from 'vuetify'

const theme = useTheme()
const props = defineProps({
  title: { type: String, default: '' },
  subtitle: { type: String, default: '' },
  width: { type: [Number, String], default: 480 },
})
const model = defineModel({ type: Boolean, default: false })

const drawerStyle = computed(() => {
  const colors = theme.current.value.colors
  return {
    width: typeof props.width === 'number' ? `${props.width}px` : props.width,
    background: colors.surface,
    color: colors['on-surface'],
  }
})
</script>

<style scoped>
.detail-drawer {
  border-left: 1px solid var(--dok-border);
  position: fixed !important;
  top: 0 !important;
  bottom: 0 !important;
  right: 0 !important;
  height: 100vh !important;
  max-height: 100vh !important;
  z-index: 4000 !important;
}
.detail-drawer__scrim {
  position: fixed;
  inset: 0;
  z-index: 3900;
  background: rgba(0, 0, 0, .32);
}
.detail-slide-enter-active, .detail-slide-leave-active { transition: transform .22s ease; }
.detail-slide-enter-from, .detail-slide-leave-to { transform: translateX(100%); }
.detail-scrim-enter-active, .detail-scrim-leave-active { transition: opacity .2s ease; }
.detail-scrim-enter-from, .detail-scrim-leave-to { opacity: 0; }
.detail-drawer__head {
  position: sticky; top: 0; z-index: 2;
  display: flex; align-items: flex-start; gap: 12px;
  padding: 18px 20px 14px;
  background: rgb(var(--v-theme-surface));
}
.detail-drawer__titles { flex: 1; min-width: 0; }
.detail-drawer__title { font-size: 17px; font-weight: 700; letter-spacing: -.02em; }
.detail-drawer__subtitle {
  margin-top: 4px;
  font-size: 12px;
  font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
  color: rgba(var(--v-theme-on-surface), .62);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.detail-drawer {
  display: flex;
  flex-direction: column;
}
.detail-drawer__head { flex: 0 0 auto; }
.detail-drawer__body {
  flex: 1 1 auto;
  min-height: 0;
  overflow-y: auto;
  overscroll-behavior: contain;
  padding: 18px 20px 28px;
}
.detail-drawer__body::-webkit-scrollbar { width: 6px; }
.detail-drawer__body::-webkit-scrollbar-thumb { background: rgba(var(--v-theme-on-surface), .18); border-radius: 999px; }
</style>
