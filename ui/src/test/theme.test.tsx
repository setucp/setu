import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import { ThemeToggle } from "@/components/shell/ThemeToggle";
import { applyTheme, getStoredTheme, resolveTheme, THEME_STORAGE_KEY } from "@/lib/theme";
import { mockMatchMedia } from "./helpers";

describe("theme", () => {
  it("the toggle switches the html class and persists the choice", async () => {
    const user = userEvent.setup();
    render(<ThemeToggle />);
    expect(document.documentElement).not.toHaveClass("dark");

    await user.click(screen.getByRole("button", { name: /theme: system/i }));
    await user.click(await screen.findByRole("menuitemradio", { name: "Dark" }));
    expect(document.documentElement).toHaveClass("dark");
    expect(window.localStorage.getItem(THEME_STORAGE_KEY)).toBe("dark");
    expect(screen.getByRole("button", { name: /theme: dark/i })).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: /theme: dark/i }));
    await user.click(await screen.findByRole("menuitemradio", { name: "Light" }));
    expect(document.documentElement).not.toHaveClass("dark");
    expect(window.localStorage.getItem(THEME_STORAGE_KEY)).toBe("light");
  });

  it("system follows prefers-color-scheme", () => {
    const restore = mockMatchMedia(["(prefers-color-scheme: dark)"]);
    try {
      expect(getStoredTheme()).toBe("system");
      expect(resolveTheme("system")).toBe("dark");
      applyTheme("system");
      expect(document.documentElement).toHaveClass("dark");
    } finally {
      restore();
    }
  });

  it("ignores junk in storage", () => {
    window.localStorage.setItem(THEME_STORAGE_KEY, "purple");
    expect(getStoredTheme()).toBe("system");
  });
});
