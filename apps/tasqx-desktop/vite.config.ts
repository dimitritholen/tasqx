import react from '@vitejs/plugin-react';
import { defineConfig } from 'vitest/config';

import { daemonBridge } from './vite-plugins/daemonBridge';

// The Tauri host loads the dev server from a fixed port (tauri.conf.json
// devUrl), so a port fallback would silently serve a blank window.
export default defineConfig({
  plugins: [react(), daemonBridge()],
  server: { port: 1420, strictPort: true },
  test: {
    environment: 'jsdom',
    globals: true,
    // The token tests import the stylesheets with ?raw; without this Vitest
    // hands back an empty string for anything CSS.
    css: true,
    setupFiles: 'src/test/setup.ts',
    // A screen test that mounts the whole app is CPU-bound, not waiting on a
    // timer: the first mount in a file plus role queries over a 50-row grid
    // with the real stylesheets cost ~2.5 s on an idle machine and passed 5 s
    // under a loaded full run (#1147). The default 5 s budget measured the
    // machine, not the code.
    testTimeout: 30_000,
    include: ['src/**/*.test.{ts,tsx}', 'vite-plugins/**/*.test.ts'],
  },
});
