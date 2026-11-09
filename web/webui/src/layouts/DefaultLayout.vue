<template>
  <AstrSidebar ref="sidebar" />
  <AstrHeader @toggle-drawer="toggleDrawer" />
  <v-main>
    <v-container fluid class="page-wrapper">
      <div class="page-content">
        <router-view v-slot="{ Component, route }">
          <transition name="page-fade" mode="out-in">
            <component :is="Component" :key="route.path" />
          </transition>
        </router-view>
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
.page-fade-enter-active { transition: opacity .2s ease, transform .2s cubic-bezier(.25,.8,.25,1); }
.page-fade-leave-active { transition: opacity .12s ease; }
.page-fade-enter-from { opacity: 0; transform: translateY(8px); }
.page-fade-leave-to { opacity: 0; }
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
  padding: 8px 24px;
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
