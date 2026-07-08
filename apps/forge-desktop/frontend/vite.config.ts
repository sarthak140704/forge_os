import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import { fileURLToPath, URL } from "node:url";

// Tauri expects a fixed dev port and no auto-clear-console so it can
// interleave build output with backend logs.
export default defineConfig({
  plugins: [react()],
  resolve: {
    alias: { "@": fileURLToPath(new URL("./src", import.meta.url)) },
  },
  clearScreen: false,
  server: {
    port: 5173,
    strictPort: true,
    host: "127.0.0.1",
  },
  envPrefix: ["VITE_", "TAURI_"],
  build: {
    target: "esnext",
    sourcemap: true,
    // monaco is huge — split it into its own chunk so first paint isn't blocked.
    rollupOptions: {
      output: {
        manualChunks: {
          monaco: ["monaco-editor"],
          xyflow: ["@xyflow/react"],
        },
      },
    },
  },
});
