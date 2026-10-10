import { Navigate, Outlet, createRootRouteWithContext, useRouterState } from "@tanstack/react-router";
import { Toaster } from "sonner";
import { useMe } from "@/lib/api";
import { useTheme } from "@/lib/theme";
import { AppShell } from "@/components/shell/AppShell";
import { NotFound, RouteError } from "@/components/RouteStates";
import { Mark } from "@/components/Mark";
import { ConfirmProvider } from "@/components/ui/dialog";
import type { RouterContextValue } from "@/router-context";

export const Route = createRootRouteWithContext<RouterContextValue>()({
  component: Root,
  notFoundComponent: NotFound,
  errorComponent: ({ error, reset }) => (
    <div className="mx-auto max-w-[1200px] px-4">
      <RouteError error={error} reset={reset} />
    </div>
  ),
});

function Root() {
  const pathname = useRouterState({ select: (s) => s.location.pathname });
  const { resolved } = useTheme();
  // `/login` and `/setup` are public: setup must work before any
  // operator (and therefore any session) exists.
  return (
    <>
      {pathname === "/login" || pathname === "/setup" ? <Outlet /> : <RequireAuth />}
      <Toaster
        theme={resolved}
        position="bottom-right"
        richColors
        closeButton
        toastOptions={{ className: "font-sans" }}
      />
    </>
  );
}

/** Full-screen splash while the session check runs. */
function Splash() {
  return (
    <div role="status" className="flex min-h-dvh flex-col items-center justify-center gap-3">
      <Mark className="size-10 motion-safe:animate-pulse" />
      <span className="text-sm text-muted-foreground">Loading Setu…</span>
    </div>
  );
}

/** Auth gate: anonymous visitors go to /login with the way back. */
function RequireAuth() {
  const me = useMe();
  const pathname = useRouterState({ select: (s) => s.location.pathname });

  if (me.isPending) return <Splash />;
  if (me.isError || me.data == null) {
    return <Navigate to="/login" search={{ redirect: pathname }} />;
  }
  return (
    <ConfirmProvider>
      <AppShell operator={me.data} />
    </ConfirmProvider>
  );
}
