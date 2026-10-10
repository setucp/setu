import { cn } from "@/lib/utils";
import { Badge } from "./ui/badge";

type Variant = "success" | "warning" | "destructive" | "secondary";

/**
 * Site and server status colours (conventions): running/online green,
 * failed/offline red, provisioning/updating/pending amber, stopped gray.
 */
const variantByStatus: Record<string, Variant> = {
  running: "success",
  online: "success",
  provisioning: "warning",
  updating: "warning",
  pending: "warning",
  rejoining: "warning",
  failed: "destructive",
  offline: "destructive",
  stopped: "secondary",
};

/** Job status colours: done green, queued/running amber, failed red. */
export const jobVariant: Record<string, Variant> = {
  done: "success",
  queued: "warning",
  running: "warning",
  failed: "destructive",
};

/** Backup status colours: completed green, pending amber, failed red. */
export const backupVariant: Record<string, Variant> = {
  completed: "success",
  pending: "warning",
  failed: "destructive",
  deleted: "secondary",
};

const IN_FLIGHT = new Set(["provisioning", "updating", "pending", "rejoining", "queued", "running"]);

/** Coloured status chip with a leading dot; unknown statuses fall back to neutral. */
export function StatusBadge({
  status,
  variants = variantByStatus,
}: {
  status: string;
  variants?: Record<string, Variant>;
}) {
  return (
    <Badge variant={variants[status] ?? "secondary"} dot>
      {status}
    </Badge>
  );
}

const dotClass: Record<Variant, string> = {
  success: "bg-success",
  warning: "bg-warning",
  destructive: "bg-destructive",
  secondary: "bg-muted-foreground/60",
};

/** Bare status dot (tables, cards); pulses while work is in flight. */
export function StatusDot({
  status,
  variants = variantByStatus,
  className,
}: {
  status: string;
  variants?: Record<string, Variant>;
  className?: string;
}) {
  const v = variants[status] ?? "secondary";
  return (
    <span className={cn("relative inline-flex size-2 shrink-0", className)} aria-hidden="true">
      {IN_FLIGHT.has(status) ? (
        <span
          className={cn(
            "absolute inset-0 rounded-full opacity-60 motion-safe:animate-ping",
            dotClass[v],
          )}
        />
      ) : null}
      <span className={cn("relative inline-flex size-2 rounded-full", dotClass[v])} />
    </span>
  );
}
