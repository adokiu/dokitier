<template>
  <div class="cfg-field">
    <div class="cfg-label-row">
      <span class="cfg-label">{{ label }}<span v-if="required" class="cfg-required">*</span></span>
      <v-tooltip v-if="hint" location="top" max-width="380" open-delay="120">
        <template #activator="{ props }">
          <v-icon v-bind="props" icon="mdi-information-outline" size="14" class="cfg-info" />
        </template>
        <div class="cfg-tip" v-html="hint" />
      </v-tooltip>
    </div>
    <slot />
  </div>
</template>

<script setup>
defineProps({
  label: { type: String, default: '' },
  hint: { type: String, default: '' },
  required: { type: Boolean, default: false },
})
</script>

<style scoped>
.cfg-field {
  display: flex;
  flex-direction: column;
  gap: 5px;
  min-width: 0;
}
.cfg-required { color: rgb(var(--v-theme-error)); margin-left: 2px; }
.cfg-label-row {
  display: flex;
  align-items: center;
  gap: 5px;
  min-height: 16px;
}
.cfg-label {
  font-size: 12px;
  font-weight: 500;
  letter-spacing: .01em;
  color: rgba(var(--v-theme-on-surface), .82);
}
.cfg-info {
  color: rgba(var(--v-theme-on-surface), .5);
  cursor: help;
}
.cfg-info:hover {
  color: rgb(var(--v-theme-primary));
}
.cfg-tip {
  font-size: 12px;
  line-height: 1.55;
  white-space: pre-line;
}
</style>
