import { ExternalLink } from "lucide-react";
import type { SiteDetail } from "@/lib/client";
import { formatBytes, formatSeconds } from "@/lib/format";
import { AppLink } from "./AppLink";
import { RelativeTime } from "./RelativeTime";
import { StatusBadge } from "./StatusBadge";
import { Alert } from "./ui/alert";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "./ui/card";

function Row({ term, children }: { term: string; children: React.ReactNode }) {
  return (
    <div className="grid grid-cols-[120px_minmax(0,1fr)] gap-4 py-2.5 text-sm sm:grid-cols-[160px_minmax(0,1fr)]">
      <dt className="text-muted-foreground">{term}</dt>
      <dd className="min-w-0 break-words">{children}</dd>
    </div>
  );
}

/**
 * Overview tab (spec §7): status, version, the site's URL, placement and
 * the latest 1-minute metrics sample (memory, CPU seconds, disk).
 */
export function SiteOverview({ site }: { site: SiteDetail }) {
  const m = site.metrics ?? null;
  const failed = site.failed_step !== null && site.failed_step !== undefined;
  return (
    <div className="grid gap-6 lg:grid-cols-[minmax(0,3fr)_minmax(0,2fr)]">
      <Card>
        <CardHeader>
          <CardTitle>Details</CardTitle>
        </CardHeader>
        <CardContent className="pt-2">
          {failed ? (
            <Alert variant="destructive" className="mb-3" title="Provisioning failed">
              The job stopped at step <code>{site.failed_step}</code>. Retry it from the Danger
              tab once the cause is fixed.
            </Alert>
          ) : null}
          <dl className="divide-y">
            <Row term="Status">
              <StatusBadge status={site.status} />
            </Row>
            <Row term="URL">
              <a
                href={`https://${site.domain}`}
                target="_blank"
                rel="noreferrer"
                className="inline-flex items-center gap-1 text-link hover:underline"
              >
                {site.domain}
                <ExternalLink className="size-3.5" aria-hidden="true" />
              </a>
              {site.admin_url ? (
                <a
                  href={site.admin_url}
                  target="_blank"
                  rel="noreferrer"
                  className="ml-3 inline-flex items-center gap-1 text-link hover:underline"
                >
                  Admin
                  <ExternalLink className="size-3.5" aria-hidden="true" />
                </a>
              ) : null}
            </Row>
            <Row term="App">
              {site.app_id} <span className="font-mono text-xs">{site.app_version}</span>
            </Row>
            <Row term="Server">
              <AppLink href={`/servers/${site.server_id}`} className="font-mono text-xs text-link hover:underline">
                {site.server_id}
              </AppLink>
            </Row>
            <Row term="Plan">
              <span className="font-mono text-xs">{site.plan_id}</span>
            </Row>
            <Row term="Owner">{site.owner_email}</Row>
            <Row term="Database">
              <span className="font-mono text-xs">{site.db_name}</span>
            </Row>
            <Row term="Port">
              <span className="font-mono text-xs">{site.port}</span>
            </Row>
            <Row term="Created">
              <RelativeTime iso={site.created_at} />
            </Row>
          </dl>
        </CardContent>
      </Card>
      <Card className="self-start">
        <CardHeader>
          <CardTitle>Usage</CardTitle>
          <CardDescription>Latest one-minute sample from the agent.</CardDescription>
        </CardHeader>
        <CardContent>
          {m === null ? (
            <p className="rounded-md border border-dashed px-4 py-6 text-center text-sm text-muted-foreground">
              No sample yet — the agent reports usage every minute while the site runs.
            </p>
          ) : (
            <>
              <dl className="grid grid-cols-3 gap-3">
                <Metric label="Memory" value={formatBytes(m.memory_bytes)} />
                <Metric label="CPU time" value={formatSeconds(m.cpu_seconds)} />
                <Metric label="Disk" value={formatBytes(m.disk_bytes)} />
              </dl>
              <p className="mt-3 text-xs text-muted-foreground">
                Sampled <RelativeTime iso={m.at} />. CPU time is cumulative since the service
                started.
              </p>
            </>
          )}
        </CardContent>
      </Card>
    </div>
  );
}

function Metric({ label, value }: { label: string; value: string }) {
  return (
    <div className="rounded-md bg-muted/60 p-3">
      <dt className="text-xs text-muted-foreground">{label}</dt>
      <dd className="mt-1 text-base font-semibold tabular-nums">{value}</dd>
    </div>
  );
}
