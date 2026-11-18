import { defineConfig } from 'vite';

// In development, `npm run dev` proxies the API and WebSocket to the Rust server on :8080.
// A relative base makes the built page load its assets from wherever it is served,
// so it also works behind a reverse proxy under a path prefix such as /set-game.
export default defineConfig({
  base: './',
  server: {
    proxy: {
      '/api': 'http://localhost:8080',
      '/ws': { target: 'ws://localhost:8080', ws: true },
    },
  },
});
