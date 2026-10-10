import "@testing-library/jest-dom/vitest";
import { afterEach } from "vitest";

// jsdom ships no matchMedia: provide a minimal stand-in that always
// reports "does not match" (desktop layout, light system theme). Tests
// that need a phone or a dark OS replace it with `mockMatchMedia`.
if (typeof window !== "undefined" && window.matchMedia === undefined) {
  Object.defineProperty(window, "matchMedia", {
    writable: true,
    configurable: true,
    value: (query: string) => ({
      matches: false,
      media: query,
      onchange: null,
      addEventListener: () => undefined,
      removeEventListener: () => undefined,
      addListener: () => undefined,
      removeListener: () => undefined,
      dispatchEvent: () => false,
    }),
  });
}

// Radix popovers measure their anchor and capture the pointer; jsdom has
// neither ResizeObserver nor pointer capture.
if (typeof window !== "undefined") {
  if (window.ResizeObserver === undefined) {
    window.ResizeObserver = class {
      observe() {}
      unobserve() {}
      disconnect() {}
    } as unknown as typeof ResizeObserver;
  }
  const proto = window.HTMLElement.prototype as unknown as Record<string, unknown>;
  proto.hasPointerCapture ??= () => false;
  proto.setPointerCapture ??= () => undefined;
  proto.releasePointerCapture ??= () => undefined;
  proto.scrollIntoView ??= () => undefined;
  // The router restores scroll on navigation; jsdom does not implement it.
  window.scrollTo = (() => undefined) as typeof window.scrollTo;
}

afterEach(() => {
  try {
    window.localStorage.clear();
  } catch {
    // ignore
  }
  document.documentElement.classList.remove("dark");
});
