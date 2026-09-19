import react from '@vitejs/plugin-react';
import { defineConfig } from 'vitest/config';

// The Tauri host loads the dev server from a fixed port (tauri.conf.json
// devUrl), so a port fallback would silently serve a blank window.
export default defineConfig({
  plugins: [react()],
  server: { port: 1420, strictPort: true },
  test: {
    environment: 'jsdom',
    globals: true,
    setupFiles: 'src/test/setup.ts',
    include: ['src/**/*.test.{ts,tsx}'],
  },
});
