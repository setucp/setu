import { cn } from "@/lib/utils";

/** Loading placeholder block; `aria-hidden` — pair with a status label. */
function Skeleton({ className }: { className?: string }) {
  return (
    <div aria-hidden="true" className={cn("animate-shimmer rounded-md bg-muted", className)} />
  );
}

/** A labelled loading region: skeleton rows plus a screen-reader status. */
function SkeletonRows({ rows = 4, label = "Loading…" }: { rows?: number; label?: string }) {
  return (
    <div role="status" aria-live="polite" className="flex flex-col gap-2">
      <span className="sr-only">{label}</span>
      <div className="overflow-hidden rounded-lg border bg-card">
        <div className="border-b bg-surface px-4 py-3">
          <Skeleton className="h-3 w-40" />
        </div>
        {Array.from({ length: rows }, (_, i) => (
          <div key={i} className="flex items-center gap-4 border-b px-4 py-3.5 last:border-0">
            <Skeleton className="size-2 rounded-full" />
            <Skeleton className="h-3.5 w-32" />
            <Skeleton className="ml-auto h-3.5 w-20" />
            <Skeleton className="hidden h-3.5 w-24 sm:block" />
          </div>
        ))}
      </div>
    </div>
  );
}

export { Skeleton, SkeletonRows };
