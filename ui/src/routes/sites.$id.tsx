import { Link, Outlet, createFileRoute, useNavigate } from "@tanstack/react-router";
import {
  Archive,
  ArrowUpCircle,

  ExternalLink,
  HardDrive,
  MemoryStick,
  Network,
  Play,
  RotateCw,
  Square,
  Tag,
  Activity,
} from "lucide-react";
import { toast } from "sonner";
import { PageHeader } from "@/components/PageHeader";
import { StatCard } from "@/components/StatCard";
import { StatusBadge } from "@/components/StatusBadge";
import { ErrorText } from "@/components/ErrorText";
import { Button, buttonVariants } from "@/components/ui/button";
import { useConfirm } from "@/components/ui/dialog";
import { Skeleton } from "@/components/ui/skeleton";
import { tabListClass, tabTriggerClass } from "@/components/ui/tabs";
import { useSite, useSiteAction, useTakeBackup } from "@/lib/api";
import { ApiError, type SiteDetail } from "@/lib/client";
import { formatBytes } from "@/lib/format";
import { cn } from "@/lib/utils";

export const Route = createFileRoute("/sites/$id")({
  component: SiteDetailLayout,
});

const TABS = [
  { to: "/sites/$id" as const, label: "Overview", exact: true },
  { to: "/sites/$id/logs" as const, label: "Logs", exact: false },
  { to: "/sites/$id/env" as const, label: "Env", exact: false },
  { to: "/sites/$id/backups" as const, label: "Backups", exact: false },
  { to: "/sites/$id/updates" as const, label: "Updates", exact: false },
  { to: "/sites/$id/limits" as const, label: "Limits", exact: false },
  { to: "/sites/$id/danger" as const, label: "Danger", exact: false },
];

/** Restart / Update / Backup / Stop (or Start), each confirmed by name. */
function ActionBar({ site }: { site: SiteDetail }) {
  const action = useSiteAction(site.id);
  const backup = useTakeBackup(site.id);
  const confirm = useConfirm();
  const navigate = useNavigate();
  const stopped = site.status === "stopped";
  const busy = action.isPending || backup.isPending;

  function openJob(jobId: string) {
    void navigate({ to: "/jobs/$id", params: { id: jobId } });
  }

  function run(kind: "start" | "stop" | "restart", label: string, verb: string) {
    void (async () => {
      const ok = await confirm({
        title: `${label} ${site.name}?`,
        description: `This ${verb} ${site.name} (${site.domain}).`,
        confirmLabel: label,
        destructive: kind === "stop",
      });
      if (!ok) return;
      action.mutate(kind, {
        onSuccess: (res) => {
          toast.success(`${label} started for ${site.name}`);
          openJob(res.job.id);
        },
        onError: (err) => toast.error(err instanceof ApiError ? err.message : `${label} failed`),
      });
    })();
  }

  function takeBackup() {
    void (async () => {
      const ok = await confirm({
        title: `Back up ${site.name} now?`,
        description: `Takes a manual backup of ${site.name}'s database and files. The site keeps running.`,
        confirmLabel: "Take backup",
      });
      if (!ok) return;
      backup.mutate(undefined, {
        onSuccess: (res) => {
          toast.success(`Backing up ${site.name}`);
          openJob(res.job.id);
        },
        onError: (err) => toast.error(err instanceof ApiError ? err.message : "Backup failed"),
      });
    })();
  }

  return (
    <>
      <Button variant="outline" disabled={busy} onClick={() => run("restart", "Restart", "restarts")}>
        <RotateCw aria-hidden="true" />
        Restart
      </Button>
      <Link
        to="/sites/$id/updates"
        params={{ id: site.id }}
        className={buttonVariants({ variant: "outline" })}
      >
        <ArrowUpCircle aria-hidden="true" />
        Update
      </Link>
      <Button variant="outline" disabled={busy} onClick={takeBackup}>
        <Archive aria-hidden="true" />
        Backup
      </Button>
      {stopped ? (
        <Button disabled={busy} onClick={() => run("start", "Start", "starts")}>
          <Play aria-hidden="true" />
          Start
        </Button>
      ) : (
        <Button variant="outline" disabled={busy} onClick={() => run("stop", "Stop", "stops")}>
          <Square aria-hidden="true" />
          Stop
        </Button>
      )}
    </>
  );
}

/** Site detail shell: header, actions, stat row and tab navigation; each tab is a child route. */
function SiteDetailLayout() {
  const { id } = Route.useParams();
  const site = useSite(id);

  if (site.isPending) {
    return (
      <div role="status" className="flex flex-col gap-6">
        <span className="sr-only">Loading site…</span>
        <Skeleton className="h-8 w-56" />
        <div className="grid grid-cols-2 gap-3 sm:grid-cols-3 lg:grid-cols-5">
          {Array.from({ length: 5 }, (_, i) => (
            <Skeleton key={i} className="h-20" />
          ))}
        </div>
        <Skeleton className="h-10 w-full" />
      </div>
    );
  }
  if (site.error instanceof ApiError) return <ErrorText error={site.error} />;
  const s = site.data;
  if (s === undefined) return null;
  const m = s.metrics ?? null;

  return (
    <div className="flex flex-col gap-6">
      <PageHeader
        title={s.name}
        meta={<StatusBadge status={s.status} />}
        description={
          <span className="flex flex-wrap items-center gap-x-3 gap-y-1">
            <a
              href={`https://${s.domain}`}
              target="_blank"
              rel="noreferrer"
              className="inline-flex items-center gap-1 text-link hover:underline"
            >
              {s.domain}
              <ExternalLink className="size-3.5" aria-hidden="true" />
            </a>
            <span className="font-mono text-xs">
              {s.app_id} {s.app_version}
            </span>
          </span>
        }
        actions={<ActionBar site={s} />}
      />

      <dl className="grid grid-cols-2 gap-3 sm:grid-cols-3 lg:grid-cols-5">
        <StatCard icon={<Activity aria-hidden="true" />} label="Status" value={<span className="capitalize">{s.status}</span>} hint={s.failed_step ? `failed at ${s.failed_step}` : undefined} />
        <StatCard icon={<Tag aria-hidden="true" />} label="Version" value={<span className="font-mono text-base">{s.app_version}</span>} hint={s.app_id} />
        <StatCard icon={<MemoryStick aria-hidden="true" />} label="Memory" value={m === null ? "—" : formatBytes(m.memory_bytes)} hint={m === null ? "No sample yet" : "Last minute"} />
        <StatCard icon={<HardDrive aria-hidden="true" />} label="Disk" value={m === null ? "—" : formatBytes(m.disk_bytes)} hint={m === null ? "No sample yet" : undefined} />
        <StatCard icon={<Network aria-hidden="true" />} label="Port" value={<span className="font-mono text-base">{s.port}</span>} hint="Loopback only" className="col-span-2 sm:col-span-1" />
      </dl>

      <nav aria-label="Site sections" className={tabListClass}>
        {TABS.map((tab) => (
          <Link
            key={tab.label}
            to={tab.to}
            params={{ id }}
            activeOptions={tab.exact ? { exact: true } : undefined}
            className={cn(
              tabTriggerClass,
              tab.label === "Danger" &&
                "text-destructive/80 hover:text-destructive data-[status=active]:text-destructive data-[status=active]:after:bg-destructive",
            )}
          >
            {tab.label}
          </Link>
        ))}
      </nav>
      <div>
        <Outlet />
      </div>
    </div>
  );
}

