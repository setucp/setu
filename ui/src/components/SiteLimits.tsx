import { usePlans } from "@/lib/api";
import { ApiError, type SiteDetail } from "@/lib/client";
import { formatBytes, formatSeconds } from "@/lib/format";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "./ui/table";

/**
 * The site's plan limits next to its current usage from the latest
 * metrics sample (spec §3 `site_metrics`). CPU is limited as a share of
 * one core and reported as cumulative CPU seconds, so the two columns
 * are not a ratio.
 */
export function SiteLimits({ site }: { site: SiteDetail }) {
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
  const plan = (plans.data ?? []).find((p) => p.id === site.plan_id);
  if (plan === undefined) {
    return <p className="text-sm text-muted-foreground">Plan not found.</p>;
  }
  const m = site.metrics ?? null;

  return (
    <div className="flex max-w-xl flex-col gap-3">
      <p className="text-sm">
        Plan <span className="font-medium">{plan.name}</span>
      </p>
      <Table>
        <TableHeader>
          <TableRow>
            <TableHead>Resource</TableHead>
            <TableHead>Limit</TableHead>
            <TableHead>Current</TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          <TableRow>
            <TableCell>Memory</TableCell>
            <TableCell>{plan.limits.memory_mb ?? "—"} MB</TableCell>
            <TableCell>{m === null ? "—" : formatBytes(m.memory_bytes)}</TableCell>
          </TableRow>
          <TableRow>
            <TableCell>CPU</TableCell>
            <TableCell>{plan.limits.cpu_percent ?? "—"}% of one core</TableCell>
            <TableCell>{m === null ? "—" : formatSeconds(m.cpu_seconds)}</TableCell>
          </TableRow>
          <TableRow>
            <TableCell>Disk</TableCell>
            <TableCell>{plan.limits.disk_mb ?? "—"} MB</TableCell>
            <TableCell>{m === null ? "—" : formatBytes(m.disk_bytes)}</TableCell>
          </TableRow>
        </TableBody>
      </Table>
      <p className="text-xs text-muted-foreground">
        {m === null
          ? "No usage sample yet — the agent reports one every minute while the site runs."
          : `Usage sampled ${new Date(m.at).toLocaleString()}; CPU time is cumulative.`}
      </p>
    </div>
  );
}
