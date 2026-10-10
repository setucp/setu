import { usePlans } from "@/lib/api";
import { ApiError, type SiteDetail } from "@/lib/client";
import { formatBytes, formatSeconds } from "@/lib/format";
import { ErrorText } from "./ErrorText";
import { RelativeTime } from "./RelativeTime";
import { Card, CardDescription, CardHeader, CardTitle } from "./ui/card";
import { Progress } from "./ui/progress";
import { SkeletonRows } from "./ui/skeleton";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "./ui/table";

const MB = 1024 * 1024;

/**
 * The site's plan limits next to its current usage from the latest
 * metrics sample (spec §3 `site_metrics`), with a bar per resource. CPU
 * is limited as a share of one core and reported as cumulative CPU
 * seconds, so it gets no bar: the two are not a ratio.
 */
export function SiteLimits({ site }: { site: SiteDetail }) {
  const plans = usePlans();

  if (plans.isPending) return <SkeletonRows rows={3} label="Loading limits…" />;
  if (plans.error instanceof ApiError) return <ErrorText error={plans.error} />;
  const plan = (plans.data ?? []).find((p) => p.id === site.plan_id);
  if (plan === undefined) {
    return <p className="text-sm text-muted-foreground">Plan not found.</p>;
  }
  const m = site.metrics ?? null;

  function bar(used: number | null | undefined, limitMb: number | undefined, label: string) {
    if (used === null || used === undefined || limitMb === undefined || limitMb <= 0) {
      return <span className="text-xs text-muted-foreground">—</span>;
    }
    const pct = Math.round((used / (limitMb * MB)) * 100);
    return (
      <span className="flex items-center gap-3">
        <Progress value={used / MB} max={limitMb} label={`${label} usage`} className="w-full min-w-24" />
        <span className="w-10 shrink-0 text-right text-xs tabular-nums text-muted-foreground">
          {pct}%
        </span>
      </span>
    );
  }

  return (
    <Card>
      <CardHeader className="pb-4">
        <CardTitle>
          Plan <span className="font-semibold text-link">{plan.name}</span>
        </CardTitle>
        <CardDescription>
          {m === null ? (
            "No usage sample yet — the agent reports one every minute while the site runs."
          ) : (
            <>
              Usage sampled <RelativeTime iso={m.at} />; CPU time is cumulative.
            </>
          )}
        </CardDescription>
      </CardHeader>
      <div className="px-5 pb-5 sm:px-6 sm:pb-6">
        <Table aria-label="Limits">
          <TableHeader>
            <TableRow className="hover:bg-transparent">
              <TableHead>Resource</TableHead>
              <TableHead>Limit</TableHead>
              <TableHead>Current</TableHead>
              <TableHead className="w-2/5 max-sm:hidden">Usage</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            <TableRow>
              <TableCell className="font-medium">Memory</TableCell>
              <TableCell className="tabular-nums">{plan.limits.memory_mb ?? "—"} MB</TableCell>
              <TableCell className="tabular-nums">{m === null ? "—" : formatBytes(m.memory_bytes)}</TableCell>
              <TableCell className="max-sm:hidden">{bar(m?.memory_bytes, plan.limits.memory_mb, "Memory")}</TableCell>
            </TableRow>
            <TableRow>
              <TableCell className="font-medium">CPU</TableCell>
              <TableCell>{plan.limits.cpu_percent ?? "—"}% of one core</TableCell>
              <TableCell className="tabular-nums">{m === null ? "—" : formatSeconds(m.cpu_seconds)}</TableCell>
              <TableCell className="text-xs text-muted-foreground max-sm:hidden">
                Cumulative time, not a share
              </TableCell>
            </TableRow>
            <TableRow>
              <TableCell className="font-medium">Disk</TableCell>
              <TableCell className="tabular-nums">{plan.limits.disk_mb ?? "—"} MB</TableCell>
              <TableCell className="tabular-nums">{m === null ? "—" : formatBytes(m.disk_bytes)}</TableCell>
              <TableCell className="max-sm:hidden">{bar(m?.disk_bytes, plan.limits.disk_mb, "Disk")}</TableCell>
            </TableRow>
          </TableBody>
        </Table>
      </div>
    </Card>
  );
}
