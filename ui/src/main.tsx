import { MutationCache, QueryCache, QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { createRouter, RouterProvider } from "@tanstack/react-router";
import React from "react";
import ReactDOM from "react-dom/client";
import { routeTree } from "./routeTree.gen";
import { api, ApiError } from "@/lib/client";
import "@/styles/globals.css";

const queryClient = new QueryClient({
  queryCache: new QueryCache({
    onError: (error) => {
      // An expired session surfaces as 401s everywhere: send the visitor
      // back to sign in once, with the way back.
      if (error instanceof ApiError && error.status === 401) {
        const back = window.location.pathname;
        if (back !== "/login") {
          window.location.assign(`/login?redirect=${encodeURIComponent(back)}`);
        }
      }
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
      <div className="flex min-h-dvh items-center justify-center">
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
