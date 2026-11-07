<template>
  <div class="layout-shell">
    <AstrSidebar ref="sidebar" />
    <AstrHeader @toggle-drawer="toggleDrawer" />
    <v-main>
      <v-container fluid class="page-wrapper">
        <div class="page-content">
          <router-view />
        </div>
      </v-container>
    </v-main>
  </div>
</template>

<script setup>
import { ref } from 'vue'
import AstrSidebar from './AstrSidebar.vue'
import AstrHeader from './AstrHeader.vue'

const sidebar = ref(null)
function toggleDrawer() { sidebar.value?.toggle?.() }
</script>

<style>
.layout-shell { min-height: 100vh; background: rgb(var(--v-theme-surface)); }
.page-wrapper { min-height: calc(100vh - 40px); padding: 8px; background: rgb(var(--v-theme-containerBg)); }
.page-content { max-width: 1560px; margin: 0 auto; padding: 12px 16px 40px; }
</style>
