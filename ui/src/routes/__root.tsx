import { Link, Navigate, Outlet, createRootRouteWithContext, useRouterState } from "@tanstack/react-router";
import { Toaster } from "sonner";
import { useLogout, useMe } from "@/lib/api";
import type { RouterContextValue } from "@/router-context";

export const Route = createRootRouteWithContext<RouterContextValue>()({
  component: Root,
});

function Root() {
  const pathname = useRouterState({ select: (s) => s.location.pathname });
  return (
    <>
      {pathname === "/login" ? <Outlet /> : <RequireAuth />}
      <Toaster richColors closeButton />
    </>
  );
}

/** Auth gate: anonymous visitors go to /login with the way back. */
function RequireAuth() {
  const me = useMe();
  const pathname = useRouterState({ select: (s) => s.location.pathname });

  if (me.isPending) {
    return (
      <div className="flex min-h-dvh items-center justify-center">
        <span className="text-sm text-muted-foreground">Loading Setu…</span>
      </div>
    );
  }
  if (me.isError || me.data == null) {
    return <Navigate to="/login" search={{ redirect: pathname }} />;
  }
  return <Shell email={me.data.email} />;
}

function Shell({ email }: { email: string }) {
  const logout = useLogout();
  return (
    <div className="min-h-dvh">
      <header className="flex items-center gap-4 border-b px-5 py-3">
        <Link to="/sites" className="text-base font-semibold tracking-tight">
          Setu
        </Link>
        <nav className="flex items-center gap-3 text-sm">
          <Link
            to="/sites"
            className="text-muted-foreground hover:text-foreground [&.active]:text-foreground"
          >
            Sites
          </Link>
          <Link
            to="/sites/new"
            className="text-muted-foreground hover:text-foreground [&.active]:text-foreground"
          >
            New site
          </Link>
        </nav>
        <span className="ml-auto text-sm text-muted-foreground">{email}</span>
        <button
          type="button"
          onClick={() => logout.mutate()}
          className="text-sm text-muted-foreground underline-offset-2 hover:text-foreground hover:underline"
        >
          Sign out
        </button>
      </header>
      <main className="mx-auto max-w-5xl px-5 py-6">
        <Outlet />
      </main>
    </div>
  );
}
