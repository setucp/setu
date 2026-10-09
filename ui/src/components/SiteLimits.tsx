import { usePlans } from "@/lib/api";
import { ApiError } from "@/lib/client";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "./ui/table";

/**
 * Plan limits for one site. There is no usage endpoint in this version —
 * live usage arrives with the agent heartbeat (Task 15) — so this page shows
 * the plan's limits and says so instead of inventing numbers.
 */
export function SiteLimits({ planId }: { planId: string }) {
  const plans = usePlans();

  if (plans.isPending) {
    return <p className="text-sm text-muted-foreground">Loading…</p>;
  }
  if (plans.error instanceof ApiError) {
    return (
      <p role="alert" className="text-sm text-destructive">
        {plans.error.message}
      </p>
    );
  }
  const plan = (plans.data ?? []).find((p) => p.id === planId);
  if (plan === undefined) {
    return <p className="text-sm text-muted-foreground">Plan not found.</p>;
  }

  return (
    <div className="flex max-w-xl flex-col gap-3">
      <Table>
        <TableHeader>
          <TableRow>
            <TableHead>Limit</TableHead>
            <TableHead>Value</TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          <TableRow>
            <TableCell>Plan</TableCell>
            <TableCell>{plan.name}</TableCell>
          </TableRow>
          <TableRow>
            <TableCell>Memory</TableCell>
            <TableCell>{plan.limits.memory_mb ?? "—"} MB</TableCell>
          </TableRow>
          <TableRow>
            <TableCell>CPU</TableCell>
            <TableCell>{plan.limits.cpu_percent ?? "—"}% of one core</TableCell>
          </TableRow>
          <TableRow>
            <TableCell>Disk</TableCell>
            <TableCell>{plan.limits.disk_mb ?? "—"} MB</TableCell>
          </TableRow>
        </TableBody>
      </Table>
      <p className="text-sm text-muted-foreground">
        Live usage is not reported in this version — per-site metrics arrive with
        the agent heartbeat.
      </p>
    </div>
  );
}
