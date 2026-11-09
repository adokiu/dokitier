<template>
  <Teleport to="body">
    <div
      v-if="visible"
      class="dok-hover-card"
      :style="{ left: `${x + 14}px`, top: `${y + 14}px` }"
    >
      <div v-if="$slots.title" class="dok-hover-card__title"><slot name="title" /></div>
      <div class="dok-hover-card__body"><slot /></div>
    </div>
  </Teleport>
</template>

<script setup>
defineProps({
  visible: { type: Boolean, default: false },
  x: { type: Number, default: 0 },
  y: { type: Number, default: 0 },
})
</script>

<style>
.dok-hover-card {
  position: fixed;
  z-index: 3000;
  min-width: 140px;
  max-width: 340px;
  padding: 10px 12px;
  border-radius: 12px;
  border: 1px solid rgba(var(--v-theme-on-surface), 0.12);
  background: rgb(var(--v-theme-surface));
  color: rgb(var(--v-theme-on-surface));
  box-shadow: 0 10px 28px rgba(16, 24, 40, 0.16);
  font-size: 12px;
  line-height: 1.55;
  pointer-events: none;
  white-space: normal;
  overflow-wrap: anywhere;
  word-break: break-word;
}
.v-theme--PurpleThemeDark .dok-hover-card {
  border-color: rgba(255, 255, 255, 0.14);
  box-shadow: 0 12px 30px rgba(0, 0, 0, 0.45);
}
.dok-hover-card__title {
  font-size: 12px;
  font-weight: 600;
  margin-bottom: 5px;
}
.dok-hover-card__body {
  display: flex;
  flex-direction: column;
  gap: 3px;
  color: rgba(var(--v-theme-on-surface), 0.7);
}
.dok-hover-card__row { display: flex; justify-content: space-between; gap: 14px; min-width: 0; }
.dok-hover-card__row span { flex: 0 0 auto; }
.dok-hover-card__row b { min-width: 0; text-align: right; overflow-wrap: anywhere; }
.dok-hover-card__row b { color: rgb(var(--v-theme-on-surface)); font-weight: 600; }
</style>
