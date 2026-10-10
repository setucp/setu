import * as React from "react";

export const PHONE_QUERY = "(max-width: 767px)";

function query(q: string): MediaQueryList | null {
  try {
    return typeof window !== "undefined" && typeof window.matchMedia === "function"
      ? window.matchMedia(q)
      : null;
  } catch {
    return null;
  }
}

/** Live `matchMedia` result; false where matchMedia is unavailable. */
export function useMediaQuery(q: string): boolean {
  return React.useSyncExternalStore(
    (onChange) => {
      const mql = query(q);
      mql?.addEventListener?.("change", onChange);
      return () => mql?.removeEventListener?.("change", onChange);
    },
    () => query(q)?.matches ?? false,
    () => false,
  );
}

/** Below the `md` breakpoint (768 px): phones. */
export function useIsPhone(): boolean {
  return useMediaQuery(PHONE_QUERY);
}
