import { toast } from "sonner";
import { useBackups, useDeleteBackup, useRestoreBackup, useTakeBackup } from "@/lib/api";
import { ApiError, type Backup } from "@/lib/client";
import { formatBytes } from "@/lib/format";
import { StatusBadge, backupVariant } from "./StatusBadge";
import { Button } from "./ui/button";
import { useConfirm } from "./ui/dialog";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "./ui/table";

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

  if (backups.isPending) {
    return <p className="text-sm text-muted-foreground">Loading…</p>;
  }
  if (backups.error instanceof ApiError) {
    return (
      <p role="alert" className="text-sm text-destructive">
        {backups.error.message}
      </p>
    );
  }
  const rows = backups.data ?? [];
  const busy = restore.isPending || remove.isPending;
  const error = [take.error, restore.error, remove.error].find(
    (e): e is ApiError => e instanceof ApiError,
  );

  function onRestore(b: Backup) {
    void (async () => {
      const when = new Date(b.taken_at).toLocaleString();
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
          toast.error(err instanceof ApiError ? err.message : "restore failed");
        },
      });
    })();
  }

  function onDelete(b: Backup) {
    void (async () => {
      const when = new Date(b.taken_at).toLocaleString();
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
          toast.error(err instanceof ApiError ? err.message : "delete failed");
        },
      });
    })();
  }

  return (
    <div className="flex flex-col gap-3">
      <div className="flex items-center gap-2">
        <Button
          type="button"
          disabled={take.isPending}
          onClick={() => {
            take.mutate(undefined, {
              onSuccess: (res) => {
                toast.success("Backup started");
                onJob?.(res.job.id);
              },
              onError: (err) => {
                toast.error(err instanceof ApiError ? err.message : "backup failed");
              },
            });
          }}
        >
          {take.isPending ? "Starting…" : "Take backup"}
        </Button>
      </div>
      {error !== undefined ? (
        <p role="alert" className="text-sm text-destructive">
          {error.message}
        </p>
      ) : null}
      {rows.length === 0 ? (
        <p className="text-sm text-muted-foreground">No backups yet.</p>
      ) : (
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead>Kind</TableHead>
              <TableHead>Taken</TableHead>
              <TableHead>Size</TableHead>
              <TableHead>Status</TableHead>
              <TableHead>
                <span className="sr-only">Actions</span>
              </TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {rows.map((b) => (
              <TableRow key={b.id}>
                <TableCell>{b.kind}</TableCell>
                <TableCell>{new Date(b.taken_at).toLocaleString()}</TableCell>
                <TableCell>{formatBytes(b.size_bytes)}</TableCell>
                <TableCell>
                  <StatusBadge status={b.status} variants={backupVariant} />
                </TableCell>
                <TableCell>
                  <span className="flex justify-end gap-1">
                    {b.status === "completed" ? (
                      <Button
                        type="button"
                        variant="outline"
                        size="sm"
                        disabled={busy}
                        onClick={() => onRestore(b)}
                      >
                        Restore
                      </Button>
                    ) : null}
                    <Button
                      type="button"
                      variant="ghost"
                      size="sm"
                      className="text-destructive"
                      disabled={busy}
                      onClick={() => onDelete(b)}
                    >
                      Delete
                    </Button>
                  </span>
                </TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      )}
    </div>
  );
}
