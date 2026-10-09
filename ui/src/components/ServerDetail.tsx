import { toast } from "sonner";
import { useCordonServer } from "@/lib/api";
import { ApiError, type Server, type Site } from "@/lib/client";
import { StatusBadge } from "./StatusBadge";
import { Badge } from "./ui/badge";
import { Button } from "./ui/button";
import { useConfirm } from "./ui/dialog";

function describeValue(value: unknown): string {
  if (value === null || value === undefined || value === "") return "—";
  return String(value);
}

/**
 * Server detail: heartbeat info, capacity, the sites placed on it, and a
 * cordon toggle (a cordoned server keeps its sites but gets no new ones),
 * confirmed with the server's name either way.
 */
export function ServerDetail({ server, sites }: { server: Server; sites: Site[] }) {
  const owned = sites.filter((s) => s.server_id === server.id);
  const cordon = useCordonServer(server.id);
  const confirm = useConfirm();
  const next = !server.cordoned;

  function toggle() {
    void (async () => {
      const ok = await confirm({
        title: next ? `Cordon ${server.name}?` : `Uncordon ${server.name}?`,
        description: next
          ? `No new sites will be placed on ${server.name}. Its ${owned.length} existing site(s) keep running.`
          : `${server.name} becomes eligible for new sites again.`,
        confirmLabel: next ? `Cordon ${server.name}` : `Uncordon ${server.name}`,
        destructive: next,
      });
      if (!ok) return;
      cordon.mutate(next, {
        onSuccess: (s) => {
          toast.success(s.cordoned ? `${s.name} cordoned` : `${s.name} uncordoned`);
        },
        onError: (err) => {
          toast.error(err instanceof ApiError ? err.message : "cordon failed");
        },
      });
    })();
  }

  return (
    <div className="flex flex-col gap-4">
      <div className="flex flex-wrap items-center gap-3">
        <h1 className="text-xl font-semibold tracking-tight">{server.name}</h1>
        <StatusBadge status={server.status} />
        {server.is_control_plane ? <Badge variant="outline">control plane</Badge> : null}
        {server.cordoned ? <Badge variant="secondary">cordoned</Badge> : null}
        <Button
          type="button"
          variant="outline"
          size="sm"
          className="ml-auto"
          disabled={cordon.isPending}
          onClick={toggle}
        >
          {server.cordoned ? "Uncordon" : "Cordon"}
        </Button>
      </div>
      {cordon.isError && cordon.error instanceof ApiError ? (
        <p role="alert" className="text-sm text-destructive">
          {cordon.error.message}
        </p>
      ) : null}
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
