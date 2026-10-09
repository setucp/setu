import type { SiteDetail } from "@/lib/client";
import { formatBytes, formatSeconds } from "@/lib/format";
import { StatusBadge } from "./StatusBadge";

/**
 * Overview tab (spec §7): status, version, the site's URL, placement and
 * the latest 1-minute metrics sample (memory, CPU seconds, disk).
 */
export function SiteOverview({ site }: { site: SiteDetail }) {
  const m = site.metrics ?? null;
  return (
    <div className="flex flex-col gap-6">
      <dl className="grid max-w-xl grid-cols-[140px_1fr] gap-x-4 gap-y-2 text-sm">
        <dt className="text-muted-foreground">Status</dt>
        <dd>
          <StatusBadge status={site.status} />
        </dd>
        <dt className="text-muted-foreground">URL</dt>
        <dd>
          <a
            href={`https://${site.domain}`}
            target="_blank"
            rel="noreferrer"
            className="text-primary underline-offset-2 hover:underline"
          >
            {site.domain}
          </a>
        </dd>
        <dt className="text-muted-foreground">App</dt>
        <dd>
          {site.app_id} <span className="font-mono">{site.app_version}</span>
        </dd>
        <dt className="text-muted-foreground">Server</dt>
        <dd>
          <a
            href={`/servers/${site.server_id}`}
            className="font-mono text-primary underline-offset-2 hover:underline"
          >
            {site.server_id}
          </a>
        </dd>
        <dt className="text-muted-foreground">Plan</dt>
        <dd className="font-mono">{site.plan_id}</dd>
        <dt className="text-muted-foreground">Owner</dt>
        <dd>{site.owner_email}</dd>
        <dt className="text-muted-foreground">Database</dt>
        <dd className="font-mono">{site.db_name}</dd>
        <dt className="text-muted-foreground">Port</dt>
        <dd>{site.port}</dd>
        {site.failed_step !== null && site.failed_step !== undefined ? (
          <>
            <dt className="text-muted-foreground">Failed step</dt>
            <dd className="text-destructive">{site.failed_step}</dd>
          </>
        ) : null}
      </dl>
      <section className="flex flex-col gap-2">
        <h2 className="text-sm font-semibold">Usage</h2>
        {m === null ? (
          <p className="text-sm text-muted-foreground">
            No sample yet — the agent reports usage every minute while the site runs.
          </p>
        ) : (
          <>
            <dl className="grid max-w-xl grid-cols-3 gap-3">
              <Metric label="Memory" value={formatBytes(m.memory_bytes)} />
              <Metric label="CPU time" value={formatSeconds(m.cpu_seconds)} />
              <Metric label="Disk" value={formatBytes(m.disk_bytes)} />
            </dl>
            <p className="text-xs text-muted-foreground">
              Sampled {new Date(m.at).toLocaleString()}. CPU time is cumulative
              since the service started.
            </p>
          </>
        )}
      </section>
    </div>
  );
}

function Metric({ label, value }: { label: string; value: string }) {
  return (
    <div className="rounded-md border p-3">
      <dt className="text-xs text-muted-foreground">{label}</dt>
      <dd className="mt-1 text-lg font-semibold tabular-nums">{value}</dd>
    </div>
  );
}
