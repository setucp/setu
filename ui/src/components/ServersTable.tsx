import type { Plan, Server, Site } from "@/lib/client";
import { formatMb } from "@/lib/format";
import { StatusBadge } from "./StatusBadge";
import { Badge } from "./ui/badge";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "./ui/table";

/**
 * Servers list: status (pending/online/offline), control-plane and cordon
 * markers, host, site count (client-side join over the sites list),
 * reserved plan memory against capacity, and the latest heartbeat's load
 * figures (1-minute load, free memory, free disk) when it has sent any.
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
    return <p className="py-10 text-center text-sm text-muted-foreground">No servers yet.</p>;
  }
  const planById = new Map(plans.map((p) => [p.id, p]));
  const sitesByServer = new Map<string, Site[]>();
  for (const site of sites) {
    const list = sitesByServer.get(site.server_id) ?? [];
    list.push(site);
    sitesByServer.set(site.server_id, list);
  }

  return (
    <Table>
      <TableHeader>
        <TableRow>
          <TableHead>Name</TableHead>
          <TableHead>Host</TableHead>
          <TableHead>Status</TableHead>
          <TableHead>Sites</TableHead>
          <TableHead>Memory reserved</TableHead>
          <TableHead>Load</TableHead>
          <TableHead>Free</TableHead>
        </TableRow>
      </TableHeader>
      <TableBody>
        {servers.map((server) => {
          const owned = sitesByServer.get(server.id) ?? [];
          const reserved = owned.reduce(
            (sum, s) => sum + (planById.get(s.plan_id)?.limits.memory_mb ?? 0),
            0,
          );
          return (
            <TableRow key={server.id}>
              <TableCell>
                <a
                  href={`/servers/${server.id}`}
                  className="font-medium text-primary underline-offset-2 hover:underline"
                >
                  {server.name}
                </a>
              </TableCell>
              <TableCell>{server.host ?? "—"}</TableCell>
              <TableCell>
                <span className="flex flex-wrap items-center gap-1">
                  <StatusBadge status={server.status} />
                  {server.is_control_plane ? <Badge variant="outline">control plane</Badge> : null}
                  {server.cordoned ? <Badge variant="secondary">cordoned</Badge> : null}
                </span>
              </TableCell>
              <TableCell>
                {owned.length} site{owned.length === 1 ? "" : "s"}
              </TableCell>
              <TableCell>
                {reserved > 0 || owned.length > 0 ? (
                  <span>
                    {formatMb(reserved)}
                    {server.memory_mb !== null && server.memory_mb !== undefined
                      ? ` of ${formatMb(server.memory_mb)}`
                      : null}
                  </span>
                ) : (
                  "—"
                )}
                {server.memory_mb !== null &&
                server.memory_mb !== undefined &&
                server.memory_mb > 0 ? (
                  <span
                    className="mt-1 block h-1.5 w-24 overflow-hidden rounded bg-muted"
                    role="img"
                    aria-label={`${reserved} of ${server.memory_mb} MB reserved`}
                  >
                    <span
                      className="block h-full rounded bg-primary"
                      style={{
                        width: `${Math.min(100, (reserved / server.memory_mb) * 100).toFixed(1)}%`,
                      }}
                    />
                  </span>
                ) : null}
              </TableCell>
              <TableCell>
                {server.load1 === null || server.load1 === undefined ? "—" : server.load1.toFixed(2)}
              </TableCell>
              <TableCell className="text-xs">
                {server.mem_free_mb === null || server.mem_free_mb === undefined ? (
                  "—"
                ) : (
                  <span className="block">{formatMb(server.mem_free_mb)} free memory</span>
                )}
                {server.disk_free_mb === null || server.disk_free_mb === undefined ? null : (
                  <span className="block">{formatMb(server.disk_free_mb)} free disk</span>
                )}
              </TableCell>
            </TableRow>
          );
        })}
      </TableBody>
    </Table>
  );
}
