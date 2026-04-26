// Vitest runtime setup. Imported by vitest.config.ts → setupFiles.
import "@testing-library/jest-dom/vitest";
import { afterEach, beforeAll } from "vitest";

// Vitest 2.x + jsdom 25 occasionally exposes an incomplete `localStorage`
// (the `--localstorage-file` warning at startup is a tell). Force-install
// a minimal in-memory Storage so tests can rely on `localStorage.{getItem,
// setItem,clear}` regardless of the backing implementation.
beforeAll(() => {
  const store = new Map<string, string>();
  const stub = {
    get length() {
      return store.size;
    },
    clear: () => {
      store.clear();
    },
    getItem: (k: string) => (store.has(k) ? store.get(k)! : null),
    key: (i: number) => Array.from(store.keys())[i] ?? null,
    removeItem: (k: string) => {
      store.delete(k);
    },
    setItem: (k: string, v: string) => {
      store.set(k, String(v));
    },
  } as Storage;

  // Replace whatever the env provided. Defining via globalThis works in
  // both jsdom (where window === globalThis) and bare-node test runs.
  Object.defineProperty(globalThis, "localStorage", {
    value: stub,
    writable: true,
    configurable: true,
  });
  if (typeof window !== "undefined") {
    Object.defineProperty(window, "localStorage", {
      value: stub,
      writable: true,
      configurable: true,
    });
  }
});

afterEach(() => {
  localStorage.clear();
});
