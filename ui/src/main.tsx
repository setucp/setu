import { MutationCache, QueryCache, QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { createRouter, RouterProvider } from "@tanstack/react-router";
import React from "react";
import ReactDOM from "react-dom/client";
import { routeTree } from "./routeTree.gen";
import { api } from "@/lib/client";
import { redirectOnUnauthorised } from "@/lib/session";
import { NotFound, RouteError } from "@/components/RouteStates";
import { Mark } from "@/components/Mark";
import "@/styles/globals.css";

const queryClient = new QueryClient({
  queryCache: new QueryCache({
    onError: (error) => {
      redirectOnUnauthorised(error);
    },
  }),
  mutationCache: new MutationCache(),
  defaultOptions: {
    queries: {
      retry: 1,
      refetchOnWindowFocus: false,
    },
  },
});

const router = createRouter({
  routeTree,
  context: { queryClient },
  defaultPreload: "intent",
  defaultNotFoundComponent: NotFound,
  defaultErrorComponent: RouteError,
});

declare module "@tanstack/react-router" {
  interface Register {
    router: typeof router;
  }
}

function App() {
  const [ready, setReady] = React.useState(false);

  React.useEffect(() => {
    let cancelled = false;
    // Seed the `me` cache before mounting so the auth gate never flashes
    // the sign-in page for a signed-in operator.
    api
      .me()
      .then((operator) => {
        if (!cancelled) queryClient.setQueryData(["me"], operator);
      })
      .catch(() => {
        // anonymous — the gate redirects to /login
      })
      .finally(() => {
        if (!cancelled) setReady(true);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  if (!ready) {
    return (
      <div role="status" className="flex min-h-dvh flex-col items-center justify-center gap-3">
        <Mark className="size-10 motion-safe:animate-pulse" />
        <span className="text-sm text-muted-foreground">Loading Setu…</span>
      </div>
    );
  }

  return <RouterProvider router={router} context={{ queryClient }} />;
}

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <QueryClientProvider client={queryClient}>
      <App />
    </QueryClientProvider>
  </React.StrictMode>,
);
