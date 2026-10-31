import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';

export default defineConfig({
  plugins: [react()],
  // Tauri 自己管理控制台输出，Vite 清屏会盖掉 Rust 侧日志。
  clearScreen: false,
  server: {
    host: '127.0.0.1',
    port: 5173,
    strictPort: true,
  },
  build: {
    target: 'chrome105',
    sourcemap: false,
  },
});
