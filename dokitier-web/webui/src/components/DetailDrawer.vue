<template>
  <v-navigation-drawer v-model="model" location="right" temporary :width="width" class="detail-drawer">
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
  </v-navigation-drawer>
</template>

<script setup>
defineProps({
  title: { type: String, default: '' },
  subtitle: { type: String, default: '' },
  width: { type: [Number, String], default: 480 },
})
const model = defineModel({ type: Boolean, default: false })
</script>

<style scoped>
.detail-drawer { border-left: 1px solid var(--dok-border); }
.detail-drawer__head {
  position: sticky; top: 0; z-index: 2;
  display: flex; align-items: flex-start; gap: 12px;
  padding: 18px 20px 14px;
  background: rgb(var(--v-theme-surface));
  border-bottom: 1px solid var(--dok-border);
}
.detail-drawer__titles { flex: 1; min-width: 0; }
.detail-drawer__title { font-size: 17px; font-weight: 700; letter-spacing: -.02em; }
.detail-drawer__subtitle { margin-top: 4px; font-size: 11px; color: var(--dok-subtle); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.detail-drawer__body { padding: 18px 20px 28px; }
</style>
