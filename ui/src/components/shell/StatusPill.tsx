import { useHealth } from "@/lib/api";
import { cn } from "@/lib/utils";

/** "Control plane healthy" from `GET /healthz`; the label hides on phones. */
export function StatusPill({ className }: { className?: string }) {
  const health = useHealth();
  const state = health.isPending
    ? { text: "Checking control plane…", dot: "bg-muted-foreground/60" }
    : health.isError || health.data?.status !== "ok"
      ? { text: "Control plane unreachable", dot: "bg-destructive" }
      : { text: "Control plane healthy", dot: "bg-success" };
  return (
    <span
      role="status"
      title={state.text}
      className={cn(
        "inline-flex h-7 items-center gap-2 rounded-full border bg-card px-2.5 text-xs font-medium text-muted-foreground",
        className,
      )}
    >
      <span className="relative flex size-2" aria-hidden="true">
        {state.dot === "bg-success" ? (
          <span className="absolute inset-0 rounded-full bg-success opacity-50 motion-safe:animate-ping [animation-duration:2.5s]" />
        ) : null}
        <span className={cn("relative size-2 rounded-full", state.dot)} />
      </span>
      <span className="max-sm:sr-only">{state.text}</span>
    </span>
  );
}
