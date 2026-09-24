import {fileURLToPath} from 'node:url';
import tailwindcss from '@tailwindcss/vite';
import react from '@vitejs/plugin-react';
import { defineConfig } from 'vite';
export default defineConfig({
  plugins: [react(), tailwindcss()],
  resolve: { alias: { '@': fileURLToPath(new URL('./src', import.meta.url)) } },
  // Browser previews are UI-only; native development uses the shared Rust service.
  build: { rollupOptions: { input: { desktop: 'desktop.html', settings: 'desktop-settings.html' } } },
});
