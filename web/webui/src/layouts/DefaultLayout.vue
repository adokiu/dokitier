<template>
  <AstrSidebar ref="sidebar" />
  <AstrHeader @toggle-drawer="toggleDrawer" />
  <v-main>
    <v-container fluid class="page-wrapper">
      <div class="page-content">
        <router-view />
      </div>
    </v-container>
  </v-main>
</template>

<script setup>
import { ref } from 'vue'
import AstrSidebar from './AstrSidebar.vue'
import AstrHeader from './AstrHeader.vue'

const sidebar = ref(null)
function toggleDrawer() { sidebar.value?.toggle?.() }
</script>

<style>
.v-application, .v-application__wrap { height: 100dvh; min-height: 0; overflow: hidden; }
.v-main {
  height: calc(100dvh - 40px) !important;
  padding-top: 0 !important;
  margin: 0;
  overflow: hidden !important;
  --v-layout-top: 0px !important;
  background: rgb(var(--v-theme-surface));
}
.page-wrapper {
  height: calc(100dvh - 40px - 6px);
  width: calc(100% - 6px);
  min-height: 0;
  margin: 0;
  padding: 0 !important;
  overflow: hidden;
  border: 1px solid rgba(var(--v-theme-on-surface), 0.1);
  border-radius: 12px;
  background: rgb(var(--v-theme-containerBg));
}
.page-content {
  height: 100%;
  width: 100%;
  padding: 8px;
  overflow-x: hidden;
  overflow-y: auto;
  scrollbar-width: none;
}
.page-content::-webkit-scrollbar { width: 0; background: transparent; }
@media (max-width: 600px) {
  .page-wrapper {
    height: 100dvh;
    width: 100%;
    border: 0;
    border-radius: 0;
  }
}
</style>
