import { toast } from "sonner";
import { useBackups, useRestoreBackup, useTakeBackup } from "@/lib/api";
import { ApiError } from "@/lib/client";
import { Badge } from "./ui/badge";
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

function formatBytes(value: number | null | undefined): string {
  if (value === null || value === undefined) return "—";
  if (value < 1024) return `${value} B`;
  if (value < 1024 * 1024) return `${(value / 1024).toFixed(1)} KiB`;
  return `${(value / (1024 * 1024)).toFixed(1)} MiB`;
}

/** Backup list with manual take + confirmed restore. Mutations report the job. */
export function SiteBackups({
  siteId,
  onJob,
}: {
  siteId: string;
  onJob?: (jobId: string) => void;
}) {
  const backups = useBackups(siteId);
  const take = useTakeBackup(siteId);
  const restore = useRestoreBackup(siteId);
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
        {take.isError && take.error instanceof ApiError ? (
          <p role="alert" className="text-sm text-destructive">
            {take.error.message}
          </p>
        ) : null}
      </div>
      {rows.length === 0 ? (
        <p className="text-sm text-muted-foreground">No backups yet.</p>
      ) : (
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead>ID</TableHead>
              <TableHead>Taken</TableHead>
              <TableHead>Kind</TableHead>
              <TableHead>Status</TableHead>
              <TableHead>Size</TableHead>
              <TableHead>
                <span className="sr-only">Actions</span>
              </TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {rows.map((b) => (
              <TableRow key={b.id}>
                <TableCell className="font-mono">{b.id}</TableCell>
                <TableCell>{new Date(b.taken_at).toLocaleString()}</TableCell>
                <TableCell>{b.kind}</TableCell>
                <TableCell>
                  <Badge variant={b.status === "completed" ? "success" : "secondary"}>
                    {b.status}
                  </Badge>
                </TableCell>
                <TableCell>{formatBytes(b.size_bytes)}</TableCell>
                <TableCell>
                  <Button
                    type="button"
                    variant="outline"
                    size="sm"
                    disabled={restore.isPending}
                    onClick={() => {
                      void (async () => {
                        const ok = await confirm({
                          title: `Restore backup ${b.id}?`,
                          description: `The site is stopped and its database and files are replaced with backup ${b.id} taken at ${new Date(b.taken_at).toLocaleString()}. Anything newer is lost.`,
                          confirmLabel: "Restore backup",
                          destructive: true,
                        });
                        if (!ok) return;
                        restore.mutate(b.id, {
                          onSuccess: (res) => {
                            toast.success("Restore started");
                            onJob?.(res.job.id);
                          },
                          onError: (err) => {
                            toast.error(
                              err instanceof ApiError ? err.message : "restore failed",
                            );
                          },
                        });
                      })();
                    }}
                  >
                    Restore
                  </Button>
                </TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      )}
      {restore.isError && restore.error instanceof ApiError ? (
        <p role="alert" className="text-sm text-destructive">
          {restore.error.message}
        </p>
      ) : null}
    </div>
  );
}
