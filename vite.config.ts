import { resolve } from "node:path";
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

const host = process.env.TAURI_DEV_HOST;

export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    port: 5173,
    strictPort: true,
    host: host || false,
    hmr: host ? { protocol: "ws", host, port: 5174 } : undefined,
    watch: {
      // **只监听真正参与应用编译的目录**，其余一律不看。
      //
      // 起因是本机连续被这个 EBUSY 打死三次（`scripts/` 两次、`docs/` 一次）：
      // 编辑器与脚本会在被编辑的目录里留 `.xxx.<uuid>.tmpdir/….tmp` 临时文件，
      // Windows 的 watcher 撞上就抛 `EBUSY`，Vite 直接退出——
      // 表现为「刚才还好好的，dev server 忽然没了」，很容易误判成代码问题。
      //
      // 逐个补目录是治标（下一处还会冒出 `docs/`、`scripts/` 之外的新目录），
      // 所以改成白名单：只跟 `src` 与根级 html。改这两个之外的任何文件
      // 都不需要触发前端刷新。
      ignored: [
        "**/node_modules/**",
        "**/src-tauri/**",
        "**/scripts/**",
        "**/docs/**",
        "**/dist/**",
        "**/public/**",
        "**/.ui-smoke-out/**",
        "**/.git/**",
        "**/*.{db,db-wal,db-shm,png,jpg,svg,ico,woff,woff2,md}",
      ],
    },
  },
  envPrefix: ["VITE_", "TAURI_ENV_"],
  build: {
    target: process.env.TAURI_ENV_PLATFORM === "windows" ? "chrome105" : "safari13",
    minify: !process.env.TAURI_ENV_DEBUG ? "esbuild" : false,
    sourcemap: !!process.env.TAURI_ENV_DEBUG,
    rollupOptions: {
      // 主窗口与桌宠窗口是两个入口；桌宠窗口独立加载，避免拖入整站资源。
      input: {
        main: resolve(__dirname, "index.html"),
        pet: resolve(__dirname, "pet.html"),
      },
    },
  },
});
