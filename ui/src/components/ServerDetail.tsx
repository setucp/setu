import * as React from "react";
import { Ban, CircleCheck, Cpu, Gauge, HeartPulse, KeyRound, Tag } from "lucide-react";
import { toast } from "sonner";
import { useCordonServer, useMe, useRotateServerToken } from "@/lib/api";
import { ApiError, type RotateTokenResponse, type Server, type Site } from "@/lib/client";
import { AppLink } from "./AppLink";
import { ErrorText } from "./ErrorText";
import { PageHeader } from "./PageHeader";
import { RelativeTime } from "./RelativeTime";
import { CapacityBar } from "./ServersTable";
import { StatCard } from "./StatCard";
import { StatusBadge, StatusDot } from "./StatusBadge";
import { Badge } from "./ui/badge";
import { Button } from "./ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "./ui/card";
import { CopyField } from "./CopyButton";
import { Modal, useConfirm } from "./ui/dialog";

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
  const me = useMe();
  const rotate = useRotateServerToken(server.id);
  const [issued, setIssued] = React.useState<RotateTokenResponse | null>(null);
  const confirm = useConfirm();
  const next = !server.cordoned;

  function rotateToken() {
    void (async () => {
      const ok = await confirm({
        title: `Rotate the token of ${server.name}?`,
        description: `A new join token is issued. The current token keeps working until you run setu agent join on ${server.name} with the new one.`,
        confirmLabel: "Issue a join token",
      });
      if (!ok) return;
      rotate.mutate(undefined, {
        onSuccess: (res) => setIssued(res),
        onError: (err) => toast.error(err instanceof ApiError ? err.message : "Rotation failed"),
      });
    })();
  }

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
          toast.error(err instanceof ApiError ? err.message : "Cordon failed");
        },
      });
    })();
  }

  return (
    <div className="flex flex-col gap-6">
      <PageHeader
        title={server.name}
        meta={
          <span className="flex flex-wrap items-center gap-1.5">
            <StatusBadge status={server.status} />
            {server.is_control_plane ? <Badge variant="info">control plane</Badge> : null}
            {server.cordoned ? <Badge variant="warning">cordoned</Badge> : null}
          </span>
        }
        description={<span className="font-mono text-xs">{describeValue(server.host)}</span>}
        actions={
          <span className="flex flex-wrap gap-2">
            {me.data?.role === "owner" ? (
              <Button type="button" variant="outline" loading={rotate.isPending} onClick={rotateToken}>
                <KeyRound aria-hidden="true" />
                Rotate token
              </Button>
            ) : null}
            <Button type="button" variant="outline" loading={cordon.isPending} onClick={toggle}>
              {cordon.isPending ? null : server.cordoned ? (
                <CircleCheck aria-hidden="true" />
              ) : (
                <Ban aria-hidden="true" />
              )}
              {server.cordoned ? "Uncordon" : "Cordon"}
            </Button>
          </span>
        }
      />
      <ErrorText error={cordon.isError ? cordon.error : null} />
      <Modal
        open={issued !== null}
        onClose={() => setIssued(null)}
        title="New join token"
        description="Shown once. Valid for one hour."
        footer={<Button onClick={() => setIssued(null)}>Done</Button>}
      >
        <div className="flex flex-col gap-3 text-sm">
          <CopyField value={issued?.join_token ?? ""} label="Join token" />
          <p>
            On {server.name}, as root: <code>{issued?.join_command}</code>, and paste the token
            when it asks (or set <code>SETU_JOIN_TOKEN</code>). The agent keeps its settings and
            restarts.
          </p>
        </div>
      </Modal>

      <dl className="grid grid-cols-2 gap-3 lg:grid-cols-4">
        <StatCard icon={<HeartPulse aria-hidden="true" />} label="Last heartbeat" value={<RelativeTime iso={server.last_seen_at} />} />
        <StatCard icon={<Gauge aria-hidden="true" />} label="Load (1 min)" value={describeValue(server.load1)} />
        <StatCard icon={<Cpu aria-hidden="true" />} label="CPU cores" value={describeValue(server.cpu_cores)} />
        <StatCard icon={<Tag aria-hidden="true" />} label="Agent version" value={<span className="font-mono text-base">{describeValue(server.agent_version)}</span>} />
      </dl>

      <div className="grid gap-6 lg:grid-cols-2">
        <Card>
          <CardHeader>
            <CardTitle>Capacity</CardTitle>
            <CardDescription>From the agent&apos;s latest heartbeat.</CardDescription>
          </CardHeader>
          <CardContent className="flex flex-col gap-4">
            {server.mem_free_mb === null || server.mem_free_mb === undefined ? (
              <p className="text-sm text-muted-foreground">No heartbeat figures yet.</p>
            ) : null}
            <CapacityBar label="Memory" total={server.memory_mb} free={server.mem_free_mb} />
            <CapacityBar label="Disk" total={server.disk_mb} free={server.disk_free_mb} />
            <dl className="mt-2 grid grid-cols-[120px_minmax(0,1fr)] gap-x-4 gap-y-2 text-sm">
              <dt className="text-muted-foreground">Memory</dt>
              <dd className="tabular-nums">
                {describeValue(server.memory_mb)}
                {server.memory_mb !== null && server.memory_mb !== undefined ? " MB total" : ""}
              </dd>
              <dt className="text-muted-foreground">Disk</dt>
              <dd className="tabular-nums">
                {describeValue(server.disk_mb)}
                {server.disk_mb !== null && server.disk_mb !== undefined ? " MB total" : ""}
              </dd>
              <dt className="text-muted-foreground">OS / arch</dt>
              <dd>
                {describeValue(server.os)} / {describeValue(server.arch)}
              </dd>
            </dl>
          </CardContent>
        </Card>

        <Card>
          <CardHeader>
            <CardTitle>Sites on this server ({owned.length})</CardTitle>
            <CardDescription>
              {server.cordoned
                ? "Cordoned: these keep running, no new sites are placed here."
                : "New sites may be placed here automatically."}
            </CardDescription>
          </CardHeader>
          <CardContent className="pt-2">
            {owned.length === 0 ? (
              <p className="rounded-md border border-dashed px-4 py-6 text-center text-sm text-muted-foreground">
                No sites placed here.
              </p>
            ) : (
              <ul className="divide-y">
                {owned.map((s) => (
                  <li key={s.id} className="flex items-center gap-3 py-2.5">
                    <StatusDot status={s.status} />
                    <div className="min-w-0 flex-1">
                      <AppLink href={`/sites/${s.id}`} className="block truncate text-sm font-medium hover:text-link">
                        {s.name}
                      </AppLink>
                      <span className="block truncate text-xs text-muted-foreground">{s.domain}</span>
                    </div>
                    <span className="font-mono text-xs text-muted-foreground">{s.app_version}</span>
                  </li>
                ))}
              </ul>
            )}
          </CardContent>
        </Card>
      </div>
    </div>
  );
}
