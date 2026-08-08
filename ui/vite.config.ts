/// <reference types="vitest/config" />
import { readFileSync } from "node:fs";
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Serve the dev server over TLS when a cert is supplied.
//
// This is not a preference — OIDC Authorization Code + PKCE needs
// `crypto.subtle`, and browsers expose Web Crypto only in a secure context.
// Over plain HTTP to anything but localhost it is absent, so sign-in cannot
// start at all. Point these at any cert the browser will accept:
//
//   VQUASAR_UI_TLS_CERT=/path/server.crt VQUASAR_UI_TLS_KEY=/path/server.key \
//     VQUASAR_CONTROL_URL=https://127.0.0.1:8080 npm run dev
//
// In production none of this applies: the control plane serves the built bundle
// over its own HTTPS listener, same origin as the API.
function devTls() {
  const cert = process.env.VQUASAR_UI_TLS_CERT;
  const key = process.env.VQUASAR_UI_TLS_KEY;
  if (!cert || !key) return undefined;
  return { cert: readFileSync(cert), key: readFileSync(key) };
}

// The UI is API-only (design section 34). In dev, proxy /api to the running
// control plane so the app is same-origin (no CORS). The production bundle is
// served by vquasar-control from the same origin.
//
// A production control plane serves HTTPS with a certificate from the fleet's
// own CA (design M12a), which Node will not trust by default. `secure: false`
// disables that check **for the dev proxy only** — it never reaches the built
// bundle, where the browser does the verifying and the console is same-origin
// anyway. Point VQUASAR_CONTROL_URL at the control plane you want:
//
//   VQUASAR_CONTROL_URL=https://127.0.0.1:8080 npm run dev
export default defineConfig({
  plugins: [react()],
  server: {
    // Listen on all interfaces so a remote browser can reach the dev server.
    host: true,
    port: 5173,
    https: devTls(),
    proxy: {
      "/api": {
        target: process.env.VQUASAR_CONTROL_URL ?? "https://127.0.0.1:8080",
        changeOrigin: true,
        // The control plane's cert is issued by the fleet CA; trusting it here
        // is a dev-loop convenience, not a production code path.
        secure: false,
        // Proxy the serial-console WebSocket upgrade too (section 25).
        ws: true,
      },
    },
  },
  // Component tests run in jsdom against the real components — no shallow
  // rendering and no mocked React. What is stubbed is `fetch` and nothing else,
  // so a test exercises the same code path a browser does right up to the wire.
  test: {
    environment: "jsdom",
    setupFiles: ["./src/test/setup.ts"],
    globals: true,
    // Excluded by default, but the UI has no other kind of test yet and a
    // stray dependency scan is a slow, confusing failure.
    include: ["src/**/*.test.{ts,tsx}"],
  },
  build: {
    outDir: "dist",
    // Never inline a font as a data: URI. Vite inlines small assets by default,
    // which would both violate the control plane's `font-src 'self'` policy and
    // force the browser to re-download the font bytes with the stylesheet on
    // every cache miss. Fonts are long-lived, hashed, separately cacheable
    // files.
    assetsInlineLimit: (filePath: string) =>
      /\.(woff2?|ttf|otf|eot)$/i.test(filePath) ? false : undefined,
    // Route the third-party runtime into its own chunk so a UI change does not
    // invalidate ~700 kB of vendor code in every operator's browser cache.
    rollupOptions: {
      output: {
        manualChunks: {
          react: ["react", "react-dom", "react-router-dom"],
          mui: ["@mui/material", "@emotion/react", "@emotion/styled"],
          query: ["@tanstack/react-query"],
          term: ["@xterm/xterm", "@xterm/addon-fit"],
          oidc: ["oidc-client-ts"],
        },
      },
    },
  },
});
