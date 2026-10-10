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

/** Coloured status chip; unknown statuses fall back to neutral. */
export function StatusBadge({
  status,
  variants = variantByStatus,
}: {
  status: string;
  variants?: Record<string, Variant>;
}) {
  return <Badge variant={variants[status] ?? "secondary"}>{status}</Badge>;
}
