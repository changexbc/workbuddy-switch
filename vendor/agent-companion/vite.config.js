import {fileURLToPath} from 'node:url';
import tailwindcss from '@tailwindcss/vite';
import react from '@vitejs/plugin-react';
import { defineConfig } from 'vite';

// The public web demo is published on the repository's GitHub Pages subpath.
const DEMO_BASE = '/agent-companion/';

export default defineConfig(({mode}) => {
  // The demo entry is a build-time choice. `VITE_DEMO_MODE` becomes a literal
  // in both builds, so the production build keeps its two desktop entries and
  // a query parameter can never switch a real rail into the demo one.
  const demo = mode === 'demo';
  return {
    plugins: [react(), tailwindcss()],
    resolve: { alias: { '@': fileURLToPath(new URL('./src', import.meta.url)) } },
    define: { 'import.meta.env.VITE_DEMO_MODE': JSON.stringify(demo ? '1' : '') },
    base: demo ? DEMO_BASE : '/',
    // Browser previews are UI-only; native development uses the shared Rust service.
    build: {
      outDir: demo ? 'dist-demo' : 'dist',
      rollupOptions: {
        input: demo
          ? { index: 'index.html', demo: 'demo.html', desktop: 'desktop.html' }
          : { desktop: 'desktop.html', settings: 'desktop-settings.html' },
      },
    },
  };
});
