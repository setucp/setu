import { Archive, History, Trash2 } from "lucide-react";
import { toast } from "sonner";
import { useBackups, useDeleteBackup, useRestoreBackup, useTakeBackup } from "@/lib/api";
import { ApiError, type Backup } from "@/lib/client";
import { formatBytes } from "@/lib/format";
import { absoluteTime } from "@/lib/time";
import { EmptyState } from "./EmptyState";
import { ErrorText } from "./ErrorText";
import { SectionHeader } from "./PageHeader";
import { RelativeTime } from "./RelativeTime";
import { ResponsiveTable, type Column } from "./ResponsiveTable";
import { StatusBadge, backupVariant } from "./StatusBadge";
import { Badge } from "./ui/badge";
import { Button } from "./ui/button";
import { useConfirm } from "./ui/dialog";
import { SkeletonRows } from "./ui/skeleton";

const kindVariant: Record<string, "info" | "secondary" | "outline"> = {
  manual: "info",
  scheduled: "secondary",
  pre_update: "outline",
};

/**
 * Backups of one site: take one now, restore a completed one, delete one.
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

  function actions(b: Backup) {
    return (
      <span className="flex justify-end gap-1">
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
      cell: (b) => <Badge variant={kindVariant[b.kind] ?? "secondary"}>{b.kind}</Badge>,
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
    </div>
  );
}
