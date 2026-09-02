import { defineConfig } from 'vite';
import vue from '@vitejs/plugin-vue';

export default defineConfig({
  plugins: [vue()],
  server: {
    host: '0.0.0.0',
    port: 5173,
    proxy: {
      '/api': {
        target: 'http://127.0.0.1:17431',
        changeOrigin: true,
      },
      '/ws': {
        target: 'ws://127.0.0.1:17431',
        ws: true,
      },
    },
  },
  preview: {
    host: '0.0.0.0',
    port: 5173,
    proxy: {
      '/api': {
        target: 'http://127.0.0.1:17431',
        changeOrigin: true,
      },
      '/ws': {
        target: 'ws://127.0.0.1:17431',
        ws: true,
      },
    },
  },
});
