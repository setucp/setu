import * as React from "react";
import { Logo } from "./Mark";
import { ThemeToggle } from "./shell/ThemeToggle";

/**
 * Public pages (sign-in, first-run setup): a centred card on a soft
 * accent glow, logo above, theme toggle in the corner.
 */
export function AuthLayout({
  children,
  wide = false,
}: {
  children: React.ReactNode;
  wide?: boolean;
}) {
  return (
    <div className="relative flex min-h-dvh flex-col overflow-hidden bg-surface">
      <div
        aria-hidden="true"
        className="pointer-events-none absolute inset-0 bg-[radial-gradient(ellipse_70%_50%_at_50%_-10%,hsl(var(--primary)/0.16),transparent_70%)]"
      />
      <div
        aria-hidden="true"
        className="pointer-events-none absolute inset-0 bg-[linear-gradient(to_right,hsl(var(--border)/0.5)_1px,transparent_1px),linear-gradient(to_bottom,hsl(var(--border)/0.5)_1px,transparent_1px)] bg-[size:48px_48px] [mask-image:radial-gradient(ellipse_60%_45%_at_50%_0%,black,transparent)]"
      />
      <header className="relative flex items-center justify-end p-3 sm:p-4">
        <ThemeToggle />
      </header>
      <main className="relative flex flex-1 flex-col items-center px-4 pb-16 pt-2 sm:justify-center sm:pt-0">
        <div className={wide ? "w-full max-w-xl" : "w-full max-w-sm"}>
          <div className="mb-8 flex justify-center">
            <Logo className="[&_img]:h-7" />
          </div>
          {children}
        </div>
      </main>
      <footer className="relative pb-6 text-center text-xs text-muted-foreground">
        Setu — a control panel for single-binary web apps
      </footer>
    </div>
  );
}
