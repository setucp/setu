import * as React from "react";
import { Archive, History, RotateCcw, Trash2 } from "lucide-react";
import { toast } from "sonner";
import {
  useBackups,
  useClearSiteReset,
  useDeleteBackup,
  useRestoreBackup,
  useSetSiteReset,
  useSiteReset,
  useTakeBackup,
} from "@/lib/api";
import { ApiError, type Backup } from "@/lib/client";
import { formatBytes } from "@/lib/format";
import { absoluteTime } from "@/lib/time";
import { EmptyState } from "./EmptyState";
import { ErrorText } from "./ErrorText";
import { SectionHeader } from "./PageHeader";
import { RelativeTime } from "./RelativeTime";
import { ResponsiveTable, type Column } from "./ResponsiveTable";
import { StatusBadge, backupVariant } from "./StatusBadge";
import { Alert } from "./ui/alert";
import { Badge } from "./ui/badge";
import { Button } from "./ui/button";
import { Modal, useConfirm } from "./ui/dialog";
import { Field } from "./ui/field";
import { SkeletonRows } from "./ui/skeleton";

const kindVariant: Record<string, "info" | "secondary" | "outline"> = {
  manual: "info",
  scheduled: "secondary",
  pre_update: "outline",
};

/** Reset intervals offered in the dialog, in minutes. */
const INTERVALS: [number, string][] = [
  [15, "every 15 minutes"],
  [30, "every 30 minutes"],
  [60, "hourly"],
  [360, "every 6 hours"],
  [1440, "daily"],
  [10080, "weekly"],
];

function intervalLabel(minutes: number): string {
  return INTERVALS.find(([m]) => m === minutes)?.[1] ?? `every ${minutes} minutes`;
}

/**
 * Backups of one site: take one now, restore a completed one, delete one,
 * or make one the site's reset point (restored on a schedule, 0.1.4).
 * Restore and delete confirm naming the site; each mutation reports its
 * job so the caller can open the live log.
 */
export function SiteBackups({
  siteId,
  siteName,
  onJob,
}: {
  siteId: string;
  siteName: string;
  onJob?: (jobId: string) => void;
}) {
  const backups = useBackups(siteId);
  const take = useTakeBackup(siteId);
  const restore = useRestoreBackup(siteId);
  const remove = useDeleteBackup(siteId);
  const reset = useSiteReset(siteId);
  const setReset = useSetSiteReset(siteId);
  const clearReset = useClearSiteReset(siteId);
  const [resetFrom, setResetFrom] = React.useState<Backup | null>(null);
  const confirm = useConfirm();

  if (backups.isPending) return <SkeletonRows label="Loading backups…" />;
  if (backups.error instanceof ApiError) return <ErrorText error={backups.error} />;
  const rows = backups.data ?? [];
  const busy = restore.isPending || remove.isPending;
  const error = [take.error, restore.error, remove.error].find(
    (e): e is ApiError => e instanceof ApiError,
  );

  function onRestore(b: Backup) {
    void (async () => {
      const when = absoluteTime(b.taken_at);
      const ok = await confirm({
        title: `Restore ${siteName} from this backup?`,
        description: `${siteName} is stopped and its database and files are replaced with the ${b.kind} backup taken ${when}. Anything newer is lost.`,
        confirmLabel: `Restore ${siteName}`,
        destructive: true,
      });
      if (!ok) return;
      restore.mutate(b.id, {
        onSuccess: (res) => {
          toast.success(`Restoring ${siteName}`);
          onJob?.(res.job.id);
        },
        onError: (err) => {
          toast.error(err instanceof ApiError ? err.message : "Restore failed");
        },
      });
    })();
  }

  function onDelete(b: Backup) {
    void (async () => {
      const when = absoluteTime(b.taken_at);
      const ok = await confirm({
        title: `Delete backup of ${siteName}?`,
        description: `The ${b.kind} backup of ${siteName} taken ${when} is removed from the server. This cannot be undone.`,
        confirmLabel: "Delete backup",
        destructive: true,
      });
      if (!ok) return;
      remove.mutate(b.id, {
        onSuccess: (res) => {
          toast.success("Deleting backup");
          onJob?.(res.job.id);
        },
        onError: (err) => {
          toast.error(err instanceof ApiError ? err.message : "Delete failed");
        },
      });
    })();
  }

  const schedule = reset.data?.reset ?? null;

  function onStopReset() {
    void (async () => {
      const ok = await confirm({
        title: `Stop resetting ${siteName}?`,
        description: "The site keeps its current data and is no longer restored on a schedule.",
        confirmLabel: "Stop resets",
      });
      if (!ok) return;
      clearReset.mutate(undefined, {
        onSuccess: () => toast.success("Scheduled reset stopped"),
        onError: (err) => toast.error(err instanceof ApiError ? err.message : "Stop failed"),
      });
    })();
  }

  function actions(b: Backup) {
    const isPoint = schedule?.backup_id === b.id;
    return (
      <span className="flex flex-wrap justify-end gap-1">
        {b.status === "completed" && !isPoint ? (
          <Button type="button" variant="ghost" size="sm" onClick={() => setResetFrom(b)}>
            <RotateCcw aria-hidden="true" />
            Use as reset point
          </Button>
        ) : null}
        {b.status === "completed" ? (
          <Button type="button" variant="outline" size="sm" disabled={busy} onClick={() => onRestore(b)}>
            <History aria-hidden="true" />
            Restore
          </Button>
        ) : null}
        <Button
          type="button"
          variant="ghost-destructive"
          size="sm"
          disabled={busy}
          onClick={() => onDelete(b)}
        >
          <Trash2 aria-hidden="true" />
          Delete
        </Button>
      </span>
    );
  }

  const columns: Column<Backup>[] = [
    {
      id: "kind",
      header: "Kind",
      cell: (b) => (
        <span className="flex flex-wrap items-center gap-1">
          <Badge variant={kindVariant[b.kind] ?? "secondary"}>{b.kind}</Badge>
          {schedule?.backup_id === b.id ? <Badge variant="warning">reset point</Badge> : null}
        </span>
      ),
    },
    { id: "taken", header: "Taken", cell: (b) => <RelativeTime iso={b.taken_at} /> },
    {
      id: "size",
      header: "Size",
      cell: (b) => <span className="tabular-nums">{formatBytes(b.size_bytes)}</span>,
    },
    {
      id: "status",
      header: "Status",
      cell: (b) => <StatusBadge status={b.status} variants={backupVariant} />,
    },
    {
      id: "actions",
      header: <span className="sr-only">Actions</span>,
      hideOnCard: true,
      className: "text-right",
      cell: actions,
    },
  ];

  const takeButton = (
    <Button
      type="button"
      loading={take.isPending}
      onClick={() => {
        take.mutate(undefined, {
          onSuccess: (res) => {
            toast.success("Backup started");
            onJob?.(res.job.id);
          },
          onError: (err) => {
            toast.error(err instanceof ApiError ? err.message : "Backup failed");
          },
        });
      }}
    >
      {take.isPending ? null : <Archive aria-hidden="true" />}
      {take.isPending ? "Starting…" : "Take backup"}
    </Button>
  );

  return (
    <div className="flex flex-col gap-4">
      <SectionHeader
        title="Backups"
        description="Database and files, taken nightly, before every update, and on demand."
        actions={rows.length > 0 ? takeButton : undefined}
      />
      <ErrorText error={error} />
      {schedule !== null ? (
        <Alert title="Scheduled reset">
          <span className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
            <span>
              {siteName} is restored from its reset point {intervalLabel(schedule.every_minutes)};
              next at {absoluteTime(schedule.next_reset_at)}.
            </span>
            <Button
              type="button"
              variant="outline"
              size="sm"
              loading={clearReset.isPending}
              onClick={onStopReset}
            >
              Stop resets
            </Button>
          </span>
        </Alert>
      ) : null}
      {rows.length === 0 ? (
        <EmptyState
          icon={<Archive className="size-6 text-link" aria-hidden="true" />}
          title="No backups yet."
          description={`Take the first backup of ${siteName} now; nightly backups start on their own.`}
          action={takeButton}
        />
      ) : (
        <ResponsiveTable
          label="Backups"
          rows={rows}
          columns={columns}
          getKey={(b) => b.id}
          cardActions={actions}
        />
      )}
      <ResetDialog
        backup={resetFrom}
        siteName={siteName}
        pending={setReset.isPending}
        onClose={() => setResetFrom(null)}
        onSet={(every) => {
          if (resetFrom === null) return;
          setReset.mutate(
            { backup_id: resetFrom.id, every_minutes: every },
            {
              onSuccess: () => {
                toast.success(`${siteName} resets ${intervalLabel(every)}`);
                setResetFrom(null);
              },
              onError: (err) =>
                toast.error(err instanceof ApiError ? err.message : "Could not set the reset"),
            },
          );
        }}
      />
    </div>
  );
}

/** Choose how often the site is restored from `backup`. */
function ResetDialog({
  backup,
  siteName,
  pending,
  onClose,
  onSet,
}: {
  backup: Backup | null;
  siteName: string;
  pending: boolean;
  onClose: () => void;
  onSet: (everyMinutes: number) => void;
}) {
  const [every, setEvery] = React.useState(60);
  React.useEffect(() => {
    if (backup !== null) setEvery(60);
  }, [backup]);
  return (
    <Modal
      open={backup !== null}
      onClose={onClose}
      title={`Reset ${siteName} on a schedule`}
      description={
        backup === null
          ? undefined
          : `${siteName} is restored from the ${backup.kind} backup taken ${absoluteTime(backup.taken_at)}. Everything written since is lost at each reset.`
      }
      footer={
        <>
          <Button variant="outline" onClick={onClose}>
            Cancel
          </Button>
          <Button loading={pending} onClick={() => onSet(every)}>
            {pending ? "Saving…" : "Start resets"}
          </Button>
        </>
      }
    >
      <Field label="How often">
        <select
          className="h-9 w-full rounded-md border border-input bg-background px-3 text-sm"
          value={every}
          onChange={(e) => setEvery(Number(e.target.value))}
        >
          {INTERVALS.map(([m, label]) => (
            <option key={m} value={m}>
              {label}
            </option>
          ))}
        </select>
      </Field>
      <p className="mt-3 text-xs text-muted-foreground">
        The first reset is one interval from now. No safety backup is taken before a reset, and
        this backup is kept until you stop the resets.
      </p>
    </Modal>
  );
}
