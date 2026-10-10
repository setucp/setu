import * as React from "react";
import { Link, Outlet, useRouterState } from "@tanstack/react-router";
import { LogOut, Menu } from "lucide-react";
import { useLogout } from "@/lib/api";
import type { Operator } from "@/lib/client";
import { Logo } from "@/components/Mark";
import { Button } from "@/components/ui/button";
import { Sheet } from "@/components/ui/dialog";
import { Avatar } from "@/components/ui/avatar";
import { TooltipProvider } from "@/components/ui/tooltip";
import { Breadcrumbs } from "./Breadcrumbs";
import { NavList } from "./NavList";
import { OperatorMenu } from "./OperatorMenu";
import { StatusPill } from "./StatusPill";
import { ThemeToggle } from "./ThemeToggle";

/**
 * Signed-in layout. ≥1024 px: 240 px sidebar with labels. 768–1023 px:
 * a 64 px icon rail with tooltips. <768 px: no sidebar; the top bar has
 * the logo and a menu button opening the same navigation in a drawer.
 */
export function AppShell({ operator }: { operator: Pick<Operator, "email" | "role"> }) {
  const [drawer, setDrawer] = React.useState(false);
  const pathname = useRouterState({ select: (s) => s.location.pathname });
  const logout = useLogout();

  // Any navigation closes the drawer (including back/forward).
  React.useEffect(() => {
    setDrawer(false);
  }, [pathname]);

  return (
    <TooltipProvider delayDuration={200}>
      <div className="min-h-dvh bg-background">
        <a
          href="#main"
          className="sr-only focus:not-sr-only focus:fixed focus:left-3 focus:top-3 focus:z-[60] focus:rounded-md focus:bg-card focus:px-3 focus:py-2 focus:text-sm focus:shadow-overlay"
        >
          Skip to content
        </a>

        <aside
          aria-label="Sidebar"
          className="fixed inset-y-0 left-0 z-40 hidden w-16 flex-col border-r bg-surface md:flex lg:w-60"
        >
          <div className="flex h-14 items-center px-5 md:max-lg:justify-center md:max-lg:px-0">
            <Link to="/sites" aria-label="Setu home" className="rounded-md">
              <Logo className="md:max-lg:hidden" />
              <img src="/favicon.svg" alt="" className="hidden size-7 md:max-lg:block" />
            </Link>
          </div>
          <nav aria-label="Main" className="scrollbar-thin flex-1 overflow-y-auto px-3 py-3">
            <NavList collapsible />
          </nav>
          <div className="border-t p-3">
            <OperatorMenu operator={operator} />
          </div>
        </aside>

        <div className="flex min-h-dvh flex-col md:pl-16 lg:pl-60">
          <header className="relative sticky top-0 z-30 flex h-14 items-center gap-2 border-b bg-background/85 px-3 backdrop-blur-md supports-[backdrop-filter]:bg-background/70 sm:px-4 md:px-6">
            <Button
              variant="ghost"
              size="icon"
              className="md:hidden"
              aria-label="Open navigation"
              aria-expanded={drawer}
              onClick={() => setDrawer(true)}
            >
              <Menu aria-hidden="true" />
            </Button>
            <Link
              to="/sites"
              aria-label="Setu home"
              className="absolute left-1/2 -translate-x-1/2 rounded-md md:hidden"
            >
              <Logo />
            </Link>
            <Breadcrumbs className="hidden md:block" />
            <div className="ml-auto flex items-center gap-1.5 sm:gap-2">
              <StatusPill />
              <ThemeToggle />
              <OperatorMenu operator={operator} variant="avatar" className="md:hidden" />
            </div>
          </header>

          <main id="main" tabIndex={-1} className="flex-1 outline-hidden">
            <div className="mx-auto w-full max-w-[1200px] px-4 py-6 sm:px-6 md:py-8 lg:px-8">
              <Breadcrumbs className="mb-4 md:hidden" hideSingle />
              <Outlet />
            </div>
          </main>
        </div>

        <Sheet open={drawer} onClose={() => setDrawer(false)} label="Navigation">
          <div className="flex h-14 items-center px-5">
            <Logo />
          </div>
          <nav aria-label="Main" className="flex-1 overflow-y-auto px-3 py-2">
            <NavList onNavigate={() => setDrawer(false)} />
          </nav>
          <div className="flex items-center gap-3 border-t p-4">
            <Avatar name={operator.email} />
            <span className="min-w-0 flex-1">
              <span className="block truncate text-sm font-medium">{operator.email}</span>
              <span className="block text-xs capitalize text-muted-foreground">{operator.role}</span>
            </span>
            <Button
              variant="ghost"
              size="icon"
              aria-label="Log out"
              onClick={() => logout.mutate()}
            >
              <LogOut aria-hidden="true" />
            </Button>
          </div>
        </Sheet>
      </div>
    </TooltipProvider>
  );
}
