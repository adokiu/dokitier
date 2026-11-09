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
      // Local dev console: proxy API calls to the deployed console by default
      // (tier.adokiu.com), override with DOK_CONSOLE_URL=...
      '/api': {
        target: process.env.DOK_CONSOLE_URL || 'https://tier.adokiu.com',
        changeOrigin: true,
        secure: false
      },
      '/api/events': {
        target: process.env.DOK_CONSOLE_URL || 'https://tier.adokiu.com',
        changeOrigin: true,
        secure: false,
        ws: true
      }
    }
  },
  build: { outDir: 'dist' }
})
