import { defineConfig } from 'vite';

// In development, `npm run dev` proxies the API and WebSocket to the Rust server on :8080.
export default defineConfig({
  server: {
    proxy: {
      '/api': 'http://localhost:8080',
      '/ws': { target: 'ws://localhost:8080', ws: true },
    },
  },
});
