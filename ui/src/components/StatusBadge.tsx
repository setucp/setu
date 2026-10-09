import type { Site } from "@/lib/client";
import { Badge } from "./ui/badge";

const variantByStatus: Record<string, "success" | "warning" | "destructive" | "secondary"> = {
  running: "success",
  provisioning: "warning",
  failed: "destructive",
};

/** Coloured status chip; unknown statuses fall back to neutral. */
export function StatusBadge({ status }: { status: Site["status"] }) {
  return <Badge variant={variantByStatus[status] ?? "secondary"}>{status}</Badge>;
}
