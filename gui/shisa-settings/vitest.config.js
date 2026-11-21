import { defineConfig } from "vitest/config";
import { svelte } from "@sveltejs/vite-plugin-svelte";

export default defineConfig(({ mode }) => ({
  plugins: [svelte()],
  resolve: {
    // Without this, Svelte resolves its server build and mount() is missing.
    conditions: mode === "test" ? ["browser"] : [],
  },
  test: {
    environment: "jsdom",
    include: ["tests/**/*.test.js"],
    setupFiles: ["tests/setup.js"],
  },
}));
