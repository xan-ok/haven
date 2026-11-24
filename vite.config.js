import { defineConfig } from "vite";
import vue from "@vitejs/plugin-vue";

// Tauri 会在开发时通过固定端口访问前端。
// 参考：https://v2.tauri.app/start/frontend/vite/
export default defineConfig({
  plugins: [vue()],
  // 避免 Tauri CLI 的输出被 Vite 清屏覆盖
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    watch: {
      // Rust 侧的改动由 Tauri 自己监听，避免 Vite 重复触发
      ignored: ["**/src-tauri/**"],
    },
  },
});
