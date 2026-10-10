import * as React from "react";

export type Theme = "light" | "dark" | "system";
export type ResolvedTheme = "light" | "dark";

export const THEME_STORAGE_KEY = "setu-theme";
const CHANGE_EVENT = "setu-theme-change";
const DARK_QUERY = "(prefers-color-scheme: dark)";

function isTheme(value: unknown): value is Theme {
  return value === "light" || value === "dark" || value === "system";
}

/** The saved choice; `system` when nothing (or nothing readable) is saved. */
export function getStoredTheme(): Theme {
  try {
    const saved = window.localStorage.getItem(THEME_STORAGE_KEY);
    return isTheme(saved) ? saved : "system";
  } catch {
    return "system";
  }
}

function systemPrefersDark(): boolean {
  try {
    return window.matchMedia?.(DARK_QUERY).matches ?? false;
  } catch {
    return false;
  }
}

export function resolveTheme(theme: Theme): ResolvedTheme {
  if (theme === "system") return systemPrefersDark() ? "dark" : "light";
  return theme;
}

/** Puts `.dark` on <html> (or takes it off). Same logic as index.html. */
export function applyTheme(theme: Theme): ResolvedTheme {
  const resolved = resolveTheme(theme);
  document.documentElement.classList.toggle("dark", resolved === "dark");
  return resolved;
}

/** Saves and applies `theme`, then tells every `useTheme` subscriber. */
export function setTheme(theme: Theme) {
  try {
    window.localStorage.setItem(THEME_STORAGE_KEY, theme);
  } catch {
    // Private mode or blocked storage: the choice lasts for this page only.
  }
  applyTheme(theme);
  window.dispatchEvent(new Event(CHANGE_EVENT));
}

function subscribe(onChange: () => void): () => void {
  const onThemeChange = () => onChange();
  const media = (() => {
    try {
      return window.matchMedia?.(DARK_QUERY) ?? null;
    } catch {
      return null;
    }
  })();
  // With `system` selected, follow the OS as it changes.
  const onSystem = () => {
    if (getStoredTheme() === "system") applyTheme("system");
    onChange();
  };
  window.addEventListener(CHANGE_EVENT, onThemeChange);
  window.addEventListener("storage", onThemeChange);
  media?.addEventListener?.("change", onSystem);
  return () => {
    window.removeEventListener(CHANGE_EVENT, onThemeChange);
    window.removeEventListener("storage", onThemeChange);
    media?.removeEventListener?.("change", onSystem);
  };
}

function snapshot(): string {
  const theme = getStoredTheme();
  return `${theme}:${resolveTheme(theme)}`;
}

/** Current choice and what it resolves to, kept live across tabs and OS changes. */
export function useTheme(): { theme: Theme; resolved: ResolvedTheme; setTheme: (t: Theme) => void } {
  const value = React.useSyncExternalStore(subscribe, snapshot, () => "system:light");
  const [theme, resolved] = value.split(":") as [Theme, ResolvedTheme];
  // Keep <html> in step if something else (a storage event) changed it.
  React.useEffect(() => {
    applyTheme(theme);
  }, [theme, resolved]);
  return { theme, resolved, setTheme };
}
