<template>
  <v-data-table
    v-bind="$attrs"
    :headers="headers"
    :items="items"
    :items-per-page="20"
    :items-per-page-options="[10, 20, 50, 100]"
    class="dok-table"
  >
    <template v-for="(_, name) in $slots" #[name]="slotProps">
      <slot :name="name" v-bind="slotProps || {}" />
    </template>
  </v-data-table>
</template>

<script setup>
defineOptions({ inheritAttrs: false })

defineProps({
  headers: { type: Array, default: () => [] },
  items: { type: Array, default: () => [] },
})
</script>

<style scoped>
.dok-table :deep(.v-data-table__td) {
  font-size: 14px;
  line-height: 1.25;
}
.dok-table :deep(.v-data-table__th) {
  font-size: 13px;
}
.dok-table :deep(.v-data-table__td .text-caption),
.dok-table :deep(.v-data-table__td .mono) {
  font-size: 14px !important;
}
.dok-table :deep(.v-data-table__td .v-btn),
.dok-table :deep(.v-data-table__td .v-chip) {
  font-size: 14px;
}
</style>
