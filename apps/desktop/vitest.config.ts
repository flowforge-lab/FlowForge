import path from "node:path";
import { defineConfig } from "vitest/config";

// Unit tests only — the helpers under test are pure, so the default `node`
// environment is enough (no jsdom / testing-library). The `@` alias mirrors
// vite.config.ts and tsconfig's paths.
export default defineConfig({
  resolve: {
    alias: {
      "@": path.resolve(__dirname, "./src"),
    },
  },
  test: {
    environment: "node",
    include: ["src/**/*.test.ts"],
  },
});
