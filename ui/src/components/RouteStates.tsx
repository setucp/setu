import type * as React from "react";
import { Home, RotateCcw, SearchX, TriangleAlert } from "lucide-react";
import { AppLink } from "./AppLink";
import { Button, buttonVariants } from "./ui/button";

function StateCard({
  icon,
  title,
  children,
  actions,
}: {
  icon: React.ReactNode;
  title: string;
  children: React.ReactNode;
  actions: React.ReactNode;
}) {
  return (
    <div className="flex min-h-[60dvh] items-center justify-center py-10">
      <div className="w-full max-w-md rounded-xl border bg-card p-8 text-center shadow-card">
        <div className="mx-auto mb-5 flex size-14 items-center justify-center rounded-xl bg-primary-subtle text-link ring-1 ring-primary/15 [&_svg]:size-7">
          {icon}
        </div>
        <h1 className="text-lg font-semibold tracking-tight">{title}</h1>
        <div className="mt-2 text-sm text-muted-foreground">{children}</div>
        <div className="mt-6 flex flex-col justify-center gap-2 sm:flex-row">{actions}</div>
      </div>
    </div>
  );
}

/** 404: an unknown path. */
export function NotFound() {
  return (
    <StateCard
      icon={<SearchX aria-hidden="true" />}
      title="Page not found"
      actions={
        <AppLink href="/sites" className={buttonVariants()}>
          <Home aria-hidden="true" />
          Back to sites
        </AppLink>
      }
    >
      <p>There is nothing at this address. It may have moved, or the link is mistyped.</p>
    </StateCard>
  );
}

/** Error boundary for a route that threw while rendering or loading. */
export function RouteError({ error, reset }: { error: unknown; reset?: () => void }) {
  const message = error instanceof Error ? error.message : "Unknown error";
  return (
    <StateCard
      icon={<TriangleAlert aria-hidden="true" />}
      title="Something went wrong"
      actions={
        <>
          {reset !== undefined ? (
            <Button variant="outline" onClick={reset}>
              <RotateCcw aria-hidden="true" />
              Try again
            </Button>
          ) : null}
          <AppLink href="/sites" className={buttonVariants()}>
            <Home aria-hidden="true" />
            Back to sites
          </AppLink>
        </>
      }
    >
      <p>This page hit an error it could not recover from.</p>
      <p className="mt-3 rounded-md bg-muted px-3 py-2 text-left font-mono text-xs break-words">
        {message}
      </p>
    </StateCard>
  );
}
