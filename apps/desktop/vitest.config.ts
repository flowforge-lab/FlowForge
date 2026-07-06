import path from "node:path";
import react from "@vitejs/plugin-react";
import { defineConfig } from "vitest/config";

// Mostly pure helpers, so the default `node` environment is enough. A few `.tsx`
// tests render components to static markup (react-dom/server) — no jsdom needed,
// but the React plugin transforms their JSX. The `@` alias mirrors
// vite.config.ts and tsconfig's paths.
export default defineConfig({
  plugins: [react()],
  resolve: {
    alias: {
      "@": path.resolve(__dirname, "./src"),
    },
  },
  test: {
    environment: "node",
    // Polyfills an in-memory `localStorage` for Node v22+, whose experimental
    // global `localStorage` is `undefined` without `--localstorage-file` and
    // shadows jsdom's — breaking persisted zustand stores in tests. See
    // vitest.setup.ts for the full diagnosis.
    setupFiles: ["./vitest.setup.ts"],
    include: ["src/**/*.test.{ts,tsx}"],
  },
});
