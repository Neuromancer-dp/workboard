import { defineConfig } from 'vitest/config';

export default defineConfig({
  clearScreen: false,
  server: { port: 1420, strictPort: true, host: '127.0.0.1' },
  build: {
    outDir: 'dist',
    emptyOutDir: true,
    target: 'safari15',
    chunkSizeWarningLimit: 1500,
  },
  test: { environment: 'jsdom', include: ['src/**/*.test.ts'] },
});
