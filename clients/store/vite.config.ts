import react from '@vitejs/plugin-react';
import { defineConfig } from 'vite';

// The build target under the web client's export, not a standalone site: `base: '/store/'` makes
// every asset URL relative to that prefix, and `outDir` drops the bundle straight into the
// directory the file server on :19992 already serves. A dedicated `emptyOutDir` guard is
// unnecessary — the directory contains only what this build writes.
export default defineConfig({
  base: '/store/',
  plugins: [react()],
  build: {
    outDir: '../../clients/web/out/store',
    emptyOutDir: true,
  },
});
