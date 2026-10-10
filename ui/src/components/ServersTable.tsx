import { Server as ServerIcon } from "lucide-react";
import type { Plan, Server, Site } from "@/lib/client";
import { formatMb } from "@/lib/format";
import { AppLink } from "./AppLink";
import { EmptyState } from "./EmptyState";
import { ResponsiveTable, type Column } from "./ResponsiveTable";
import { StatusBadge, StatusDot } from "./StatusBadge";
import { Badge } from "./ui/badge";
import { Progress } from "./ui/progress";

function has(n: number | null | undefined): n is number {
  return n !== null && n !== undefined;
}

/** "Memory · 4.0G free" over a used/total bar; nothing when the agent has not reported. */
export function CapacityBar({
  label,
  total,
  free,
}: {
  label: string;
  total: number | null | undefined;
  free: number | null | undefined;
}) {
  if (!has(free)) return null;
  return (
    <div className="flex min-w-32 flex-col gap-1">
      <div className="flex items-baseline justify-between gap-2 text-xs">
        <span className="text-muted-foreground">{label}</span>
        <span className="tabular-nums">{`${formatMb(free)} free`}</span>
      </div>
      {has(total) && total > 0 ? (
        <Progress value={total - free} max={total} label={`${label} in use`} />
      ) : null}
    </div>
  );
}

function ServerName({ server }: { server: Server }) {
  return (
    <div className="flex min-w-0 items-start gap-3">
      <StatusDot status={server.status} className="mt-1.5" />
      <div className="min-w-0">
        <AppLink
          href={`/servers/${server.id}`}
          className="block truncate font-semibold text-foreground hover:text-link"
        >
          {server.name}
        </AppLink>
        <span className="block truncate font-mono text-xs text-muted-foreground">
          {server.host ?? "—"}
        </span>
      </div>
    </div>
  );
}

/**
 * Servers list: status (pending/online/offline), control-plane and cordon
 * markers, host, site count (client-side join over the sites list),
 * reserved plan memory against capacity, and the latest heartbeat's load
 * and free memory/disk as bars when it has sent any.
 */
export function ServersTable({
  servers,
  sites,
  plans,
}: {
  servers: Server[];
  sites: Site[];
  plans: Plan[];
}) {
  if (servers.length === 0) {
    return (
      <EmptyState
        icon={<ServerIcon className="size-6 text-link" aria-hidden="true" />}
        title="No servers yet."
        description="Add a server and run the one-line installer on it; its agent joins the panel by itself."
      />
    );
  }
  const planById = new Map(plans.map((p) => [p.id, p]));
  const sitesByServer = new Map<string, Site[]>();
  for (const site of sites) {
    const list = sitesByServer.get(site.server_id) ?? [];
    list.push(site);
    sitesByServer.set(site.server_id, list);
  }
  const reservedFor = (server: Server) =>
    (sitesByServer.get(server.id) ?? []).reduce(
      (sum, s) => sum + (planById.get(s.plan_id)?.limits.memory_mb ?? 0),
      0,
    );

  const columns: Column<Server>[] = [
    { id: "name", header: "Name", cell: (server) => <ServerName server={server} /> },
    {
      id: "status",
      header: "Status",
      cell: (server) => (
        <span className="flex flex-wrap items-center gap-1">
          <StatusBadge status={server.status} />
          {server.is_control_plane ? <Badge variant="info">control plane</Badge> : null}
          {server.cordoned ? <Badge variant="warning">cordoned</Badge> : null}
        </span>
      ),
    },
    {
      id: "sites",
      header: "Sites",
      cell: (server) => {
        const n = (sitesByServer.get(server.id) ?? []).length;
        return <span className="whitespace-nowrap tabular-nums">{`${n} site${n === 1 ? "" : "s"}`}</span>;
      },
    },
    {
      id: "reserved",
      header: "Reserved",
      cardLabel: "Memory reserved",
      cell: (server) => {
        const reserved = reservedFor(server);
        const owned = (sitesByServer.get(server.id) ?? []).length;
        if (reserved === 0 && owned === 0) return <span className="text-muted-foreground">—</span>;
        return (
          <div className="flex min-w-28 flex-col gap-1">
            <span className="text-xs tabular-nums">
              {`${formatMb(reserved)}${has(server.memory_mb) ? ` of ${formatMb(server.memory_mb)}` : ""}`}
            </span>
            {has(server.memory_mb) && server.memory_mb > 0 ? (
              <Progress
                value={reserved}
                max={server.memory_mb}
                tone="primary"
                label={`${reserved} of ${server.memory_mb} MB reserved`}
              />
            ) : null}
          </div>
        );
      },
    },
    {
      id: "load",
      header: "Load",
      cell: (server) => (
        <span className="tabular-nums">{has(server.load1) ? server.load1.toFixed(2) : "—"}</span>
      ),
    },
    {
      id: "capacity",
      header: "Capacity",
      className: "min-w-44",
      cell: (server) =>
        has(server.mem_free_mb) || has(server.disk_free_mb) ? (
          <div className="flex flex-col gap-2">
            <CapacityBar label="Memory" total={server.memory_mb} free={server.mem_free_mb} />
            <CapacityBar label="Disk" total={server.disk_mb} free={server.disk_free_mb} />
          </div>
        ) : (
          <span className="text-muted-foreground">—</span>
        ),
    },
  ];

  return (
    <ResponsiveTable
      label="Servers"
      rows={servers}
      columns={columns}
      getKey={(s) => s.id}
      cardTitle={(server) => <ServerName server={server} />}
    />
  );
}
