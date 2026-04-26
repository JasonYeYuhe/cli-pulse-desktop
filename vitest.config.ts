/// <reference types="vitest" />
import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";

// Vitest config kept separate from `vite.config.ts` so the production
// build path (which is async + tauri-specific) doesn't have to bend
// around test infrastructure. Run with `npm test` (vitest run, single
// pass for CI / pre-push hook) or `npm run test:watch` (interactive).

export default defineConfig({
  plugins: [react()],
  test: {
    environment: "jsdom",
    globals: true,
    setupFiles: ["./src/test/setup.ts"],
    include: ["src/**/*.{test,spec}.{ts,tsx}"],
    exclude: ["node_modules/**", "dist/**", "src-tauri/**"],
    coverage: {
      // Optional — `npm test -- --coverage` to get a summary.
      provider: "v8",
      reporter: ["text", "html"],
      include: ["src/**/*.{ts,tsx}"],
      exclude: ["src/test/**", "src/**/*.test.{ts,tsx}"],
    },
  },
});
