import { defineConfig } from "vitest/config";
import type { ServerOptions } from "vite";
import { svelte } from "@sveltejs/vite-plugin-svelte";
import { fileURLToPath } from "node:url";

const host = process.env.TAURI_DEV_HOST;
const debug = !!process.env.TAURI_ENV_DEBUG;

const server: ServerOptions = {
  port: 1420,
  strictPort: true,
  host: host || false,
  watch: { ignored: ["**/src-tauri/**", "**/target/**"] },
};
if (host) server.hmr = { protocol: "ws", host, port: 1421 };

// https://v2.tauri.app/start/frontend/vite/
export default defineConfig({
  plugins: [svelte()],
  resolve: {
    alias: {
      $lib: fileURLToPath(new URL("./src/lib", import.meta.url)),
      $features: fileURLToPath(new URL("./src/features", import.meta.url)),
    },
  },
  clearScreen: false,
  server,
  envPrefix: ["VITE_", "TAURI_ENV_*"],
  build: {
    // WebView2 (Chromium) on Windows, WKWebView on macOS 12+.
    target: process.env.TAURI_ENV_PLATFORM === "windows" ? "chrome105" : "safari15",
    minify: !debug,
    sourcemap: debug,
  },
  test: {
    include: ["src/**/*.test.ts"],
    environment: "node",
  },
});
