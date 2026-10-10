import { ExternalLink, Plus } from "lucide-react";
import type { Plan, Server, Site, SiteListItem } from "@/lib/client";
import { formatBytes, formatMb } from "@/lib/format";
import { AppLink } from "./AppLink";
import { EmptyState } from "./EmptyState";
import { ResponsiveTable, type Column } from "./ResponsiveTable";
import { StatusBadge, StatusDot } from "./StatusBadge";
import { buttonVariants } from "./ui/button";
import { Progress } from "./ui/progress";

const MB = 1024 * 1024;

/** `used / limit` with the Limits tab's bar; `—` until the first sample. */
function Usage({ used, limitMb, label }: { used: number | null | undefined; limitMb: number | undefined; label: string }) {
  const known = used !== null && used !== undefined;
  return (
    <span className="flex min-w-28 flex-col items-end gap-1">
      <span className="whitespace-nowrap tabular-nums">{`${formatBytes(used)} / ${formatMb(limitMb)}`}</span>
      {known && limitMb !== undefined && limitMb > 0 ? (
        <Progress value={used / MB} max={limitMb} label={`${label} usage`} className="w-24" />
      ) : null}
    </span>
  );
}

function SiteName({ site }: { site: Site }) {
  return (
    <div className="flex min-w-0 items-start gap-3">
      <StatusDot status={site.status} className="mt-1.5" />
      <div className="min-w-0">
        <AppLink
          href={`/sites/${site.id}`}
          className="block truncate font-semibold text-foreground hover:text-link"
        >
          {site.name}
        </AppLink>
        <a
          href={`https://${site.domain}`}
          target="_blank"
          rel="noreferrer"
          className="group inline-flex max-w-full items-center gap-1 text-xs text-muted-foreground hover:text-link"
        >
          <span className="truncate">{site.domain}</span>
          <ExternalLink className="size-3 shrink-0 opacity-60 group-hover:opacity-100" aria-hidden="true" />
        </a>
      </div>
    </div>
  );
}

/**
 * Sites overview (spec §6): status, name and domain, app/version, server,
 * plan and its memory/disk usage. The list carries each site's newest
 * metric sample, so memory and disk read `used / limit` with the plan's
 * allowance and a bar (`—` until the first sample). A table on desktop,
 * cards on phones.
 */
export function SitesTable({
  sites,
  plans,
  servers,
}: {
  sites: SiteListItem[];
  plans: Plan[];
  servers: Server[];
}) {
  if (sites.length === 0) {
    return (
      <EmptyState
        title="No sites yet."
        description="A site is one app version running on one of your servers, with its own domain, database and backups."
        action={
          <AppLink href="/sites/new" className={buttonVariants()}>
            <Plus aria-hidden="true" />
            Create your first site
          </AppLink>
        }
      />
    );
  }
  const planById = new Map(plans.map((p) => [p.id, p]));
  const serverById = new Map(servers.map((s) => [s.id, s]));

  const columns: Column<SiteListItem>[] = [
    {
      id: "name",
      header: "Name",
      cell: (site) => <SiteName site={site} />,
      className: "max-w-72",
    },
    {
      id: "app",
      header: "App",
      cell: (site) => (
        <span className="whitespace-nowrap font-mono text-xs">{`${site.app_id} ${site.app_version}`}</span>
      ),
    },
    {
      id: "server",
      header: "Server",
      cell: (site) => (
        <AppLink href={`/servers/${site.server_id}`} className="hover:text-link">
          {serverById.get(site.server_id)?.name ?? site.server_id}
        </AppLink>
      ),
    },
    {
      id: "plan",
      header: "Plan",
      cell: (site) => planById.get(site.plan_id)?.name ?? site.plan_id,
    },
    {
      id: "status",
      header: "Status",
      cell: (site) => (
        <span className="flex flex-col items-start gap-1">
          <StatusBadge status={site.status} />
          {site.failed_step !== null && site.failed_step !== undefined ? (
            <span className="text-xs text-destructive">failed at {site.failed_step}</span>
          ) : null}
        </span>
      ),
    },
    {
      id: "memory",
      header: "Memory",
      cell: (site) => (
        <Usage used={site.metrics?.memory_bytes} limitMb={planById.get(site.plan_id)?.limits.memory_mb} label="Memory" />
      ),
      className: "text-right",
      headClassName: "text-right",
    },
    {
      id: "disk",
      header: "Disk",
      cell: (site) => (
        <Usage used={site.metrics?.disk_bytes} limitMb={planById.get(site.plan_id)?.limits.disk_mb} label="Disk" />
      ),
      className: "text-right",
      headClassName: "text-right",
    },
  ];

  return (
    <ResponsiveTable
      label="Sites"
      rows={sites}
      columns={columns}
      getKey={(s) => s.id}
      cardTitle={(site) => <SiteName site={site} />}
    />
  );
}
