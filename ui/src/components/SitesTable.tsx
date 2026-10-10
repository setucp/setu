import type { Plan, Server, Site } from "@/lib/client";
import { StatusBadge } from "./StatusBadge";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "./ui/table";

function formatMb(value: number | undefined): string {
  if (value === undefined) return "—";
  return value >= 1024 ? `${(value / 1024).toFixed(1)}G` : `${value}M`;
}

/** Sites overview table (spec §6): name, domain, app/version, server, plan, status, memory, disk. */
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
    return <p className="py-10 text-center text-sm text-muted-foreground">No sites yet.</p>;
  }
  const planById = new Map(plans.map((p) => [p.id, p]));
  const serverById = new Map(servers.map((s) => [s.id, s]));
  return (
    <Table>
      <TableHeader>
        <TableRow>
          <TableHead>Name</TableHead>
          <TableHead>Domain</TableHead>
          <TableHead>App</TableHead>
          <TableHead>Server</TableHead>
          <TableHead>Plan</TableHead>
          <TableHead>Status</TableHead>
          <TableHead>Memory</TableHead>
          <TableHead>Disk</TableHead>
        </TableRow>
      </TableHeader>
      <TableBody>
        {sites.map((site) => {
          const plan = planById.get(site.plan_id);
          return (
            <TableRow key={site.id}>
              <TableCell>
                <a
                  href={`/sites/${site.id}`}
                  className="font-medium text-primary underline-offset-2 hover:underline"
                >
                  {site.name}
                </a>
              </TableCell>
              <TableCell>{site.domain}</TableCell>
              <TableCell>
                {site.app_id} {site.app_version}
              </TableCell>
              <TableCell>{serverById.get(site.server_id)?.name ?? site.server_id}</TableCell>
              <TableCell>{plan?.name ?? site.plan_id}</TableCell>
              <TableCell>
                <StatusBadge status={site.status} />
                {site.failed_step !== null && site.failed_step !== undefined ? (
                  <span className="ml-2 text-xs text-muted-foreground">
                    failed at {site.failed_step}
                  </span>
                ) : null}
              </TableCell>
              <TableCell>{formatMb(plan?.limits.memory_mb)}</TableCell>
              <TableCell>{formatMb(plan?.limits.disk_mb)}</TableCell>
            </TableRow>
          );
        })}
      </TableBody>
    </Table>
  );
}
