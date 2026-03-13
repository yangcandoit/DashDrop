import { defineConfig } from "vite";
import vue from "@vitejs/plugin-vue";

// @ts-expect-error process is a nodejs global injected by Vite during dev/build but missing in browser typings
const host = process.env.TAURI_DEV_HOST;
// @ts-expect-error process is a nodejs global injected by Vite during dev/build but missing in browser typings
const devPort = Number(process.env.DASHDROP_TAURI_DEV_PORT || "1420");
// @ts-expect-error process is a nodejs global injected by Vite during dev/build but missing in browser typings
const hmrPort = Number(process.env.DASHDROP_TAURI_HMR_PORT || "1421");

// https://vite.dev/config/
export default defineConfig(async () => ({
  plugins: [vue()],

  // Vite options tailored for Tauri development and only applied in `tauri dev` or `tauri build`
  //
  // 1. prevent Vite from obscuring rust errors
  clearScreen: false,
  // 2. tauri expects a fixed port, fail if that port is not available
  server: {
    port: devPort,
    strictPort: true,
    host: host || false,
    hmr: host
      ? {
        protocol: "ws",
        host,
        port: hmrPort,
      }
      : undefined,
    watch: {
      // 3. tell Vite to ignore watching `src-tauri`
      ignored: ["**/src-tauri/**"],
    },
  },
}));
