import type { Server, Site } from "@/lib/client";
import { StatusBadge } from "./StatusBadge";

function describeValue(value: unknown): string {
  if (value === null || value === undefined || value === "") return "—";
  return String(value);
}

/** Server detail: heartbeat info, capacity, and the sites placed on it. */
export function ServerDetail({ server, sites }: { server: Server; sites: Site[] }) {
  const owned = sites.filter((s) => s.server_id === server.id);
  return (
    <div className="flex flex-col gap-4">
      <div className="flex items-center gap-3">
        <h1 className="text-xl font-semibold tracking-tight">{server.name}</h1>
        <StatusBadge status={server.status} />
      </div>
      <dl className="grid max-w-xl grid-cols-[160px_1fr] gap-x-4 gap-y-2 text-sm">
        <dt className="text-muted-foreground">Host</dt>
        <dd>{describeValue(server.host)}</dd>
        <dt className="text-muted-foreground">Agent version</dt>
        <dd>{describeValue(server.agent_version)}</dd>
        <dt className="text-muted-foreground">OS / arch</dt>
        <dd>
          {describeValue(server.os)} / {describeValue(server.arch)}
        </dd>
        <dt className="text-muted-foreground">Last heartbeat</dt>
        <dd>
          {server.last_seen_at !== null && server.last_seen_at !== undefined
            ? new Date(server.last_seen_at).toLocaleString()
            : "—"}
        </dd>
        <dt className="text-muted-foreground">Load (1 min)</dt>
        <dd>{describeValue(server.load1)}</dd>
        <dt className="text-muted-foreground">CPU cores</dt>
        <dd>{describeValue(server.cpu_cores)}</dd>
        <dt className="text-muted-foreground">Memory</dt>
        <dd>
          {describeValue(server.memory_mb)}
          {server.mem_free_mb !== null && server.mem_free_mb !== undefined
            ? ` MB total, ${server.mem_free_mb} MB free`
            : server.memory_mb !== null && server.memory_mb !== undefined
              ? " MB total"
              : ""}
        </dd>
        <dt className="text-muted-foreground">Disk</dt>
        <dd>
          {describeValue(server.disk_mb)}
          {server.disk_free_mb !== null && server.disk_free_mb !== undefined
            ? ` MB total, ${server.disk_free_mb} MB free`
            : server.disk_mb !== null && server.disk_mb !== undefined
              ? " MB total"
              : ""}
        </dd>
      </dl>
      <section className="flex flex-col gap-2">
        <h2 className="text-sm font-semibold">
          Sites on this server ({owned.length})
        </h2>
        {owned.length === 0 ? (
          <p className="text-sm text-muted-foreground">No sites placed here.</p>
        ) : (
          <ul className="flex flex-col gap-1 text-sm">
            {owned.map((s) => (
              <li key={s.id}>
                <a
                  href={`/sites/${s.id}`}
                  className="text-primary underline-offset-2 hover:underline"
                >
                  {s.name}
                </a>{" "}
                <span className="text-muted-foreground">{s.domain}</span>
              </li>
            ))}
          </ul>
        )}
      </section>
    </div>
  );
}
