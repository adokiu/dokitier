import { defineConfig } from 'vite'
import vue from '@vitejs/plugin-vue'
import { fileURLToPath, URL } from 'node:url'

export default defineConfig({
  plugins: [vue()],
  resolve: {
    alias: { '@': fileURLToPath(new URL('./src', import.meta.url)) },
  },
  server: {
    port: Number(process.env.PORT) || 5173,
    proxy: {
      '/api': process.env.DOK_CONSOLE_URL || 'http://127.0.0.1:13110',
      '/api/events': { target: process.env.DOK_CONSOLE_URL || 'ws://127.0.0.1:13110', ws: true }
    }
  },
  build: { outDir: 'dist' }
})
