import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// The UI is API-only (design section 34). In dev, proxy /api to the running
// control plane so the app is same-origin (no CORS). The production bundle is
// served by ch-control from the same origin.
export default defineConfig({
  plugins: [react()],
  server: {
    port: 5173,
    proxy: {
      "/api": {
        target: process.env.CH_CONTROL_URL ?? "http://127.0.0.1:8080",
        changeOrigin: true,
      },
    },
  },
  build: {
    outDir: "dist",
  },
});
