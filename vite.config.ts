import { defineConfig } from "vite";

// Tauri v2 frontend (Vanilla TS). Kept intentionally minimal to keep RAM low.
const host = process.env.TAURI_DEV_HOST;

export default defineConfig({
  // Tauri serves the dev server on a fixed port; fail fast instead of hopping.
  clearScreen: false,
  server: {
    host: host || false,
    port: 1420,
    strictPort: true,
    hmr: host
      ? { protocol: "ws", host, port: 1421 }
      : undefined,
    watch: {
      // Don't watch the Rust backend from the frontend dev server.
      ignored: ["**/src-tauri/**"],
    },
  },
  build: {
    // Output consumed by tauri.conf.json -> build.frontendDist ("../dist").
    outDir: "dist",
    emptyOutDir: true,
    target: "esnext",
    minify: "esbuild",
  },
});
