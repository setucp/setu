import { ExternalLink, Plus } from "lucide-react";
import type { Plan, Server, Site } from "@/lib/client";
import { formatMb } from "@/lib/format";
import { AppLink } from "./AppLink";
import { EmptyState } from "./EmptyState";
import { ResponsiveTable, type Column } from "./ResponsiveTable";
import { StatusBadge, StatusDot } from "./StatusBadge";
import { buttonVariants } from "./ui/button";

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
 * plan and its memory/disk allowance. The list endpoint carries no
 * metrics, so memory and disk are the plan's limits; the site page shows
 * live usage. A table on desktop, cards on phones.
 */
export function SitesTable({
  sites,
  plans,
  servers,
}: {
  sites: Site[];
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

  const columns: Column<Site>[] = [
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
        <span className="tabular-nums">{formatMb(planById.get(site.plan_id)?.limits.memory_mb)}</span>
      ),
      className: "text-right",
      headClassName: "text-right",
    },
    {
      id: "disk",
      header: "Disk",
      cell: (site) => (
        <span className="tabular-nums">{formatMb(planById.get(site.plan_id)?.limits.disk_mb)}</span>
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
