import { toast } from "sonner";
import { useAppVersions, useUpdateSite } from "@/lib/api";
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

/** Available versions with the current highlighted; update starts a job. */
export function SiteUpdates({
  siteId,
  appId,
  currentVersion,
  onJob,
}: {
  siteId: string;
  appId: string;
  currentVersion: string;
  onJob?: (jobId: string) => void;
}) {
  const versions = useAppVersions(appId);
  const update = useUpdateSite(siteId);
  const confirm = useConfirm();

  if (versions.isPending) {
    return <p className="text-sm text-muted-foreground">Loading…</p>;
  }
  if (versions.error instanceof ApiError) {
    return (
      <p role="alert" className="text-sm text-destructive">
        {versions.error.message}
      </p>
    );
  }
  const rows = versions.data ?? [];

  return (
    <div className="flex flex-col gap-3">
      {rows.length === 0 ? (
        <p className="text-sm text-muted-foreground">No versions published yet.</p>
      ) : (
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead>Version</TableHead>
              <TableHead>Channel</TableHead>
              <TableHead>Published</TableHead>
              <TableHead>
                <span className="sr-only">Actions</span>
              </TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {rows.map((v) => {
              const current = v.version === currentVersion;
              return (
                <TableRow key={v.id}>
                  <TableCell>
                    <span className="flex items-center gap-2 font-mono">
                      {v.version}
                      {current ? <Badge variant="success">current</Badge> : null}
                    </span>
                  </TableCell>
                  <TableCell>{v.channel}</TableCell>
                  <TableCell>{new Date(v.published_at).toLocaleDateString()}</TableCell>
                  <TableCell>
                    {current ? null : (
                      <Button
                        type="button"
                        variant="outline"
                        size="sm"
                        disabled={update.isPending}
                        onClick={() => {
                          void (async () => {
                            const ok = await confirm({
                              title: `Update to ${v.version}?`,
                              description: `A pre-update backup is taken first; if the new version fails its health check the site rolls back automatically.`,
                              confirmLabel: `Update to ${v.version}`,
                            });
                            if (!ok) return;
                            update.mutate(v.version, {
                              onSuccess: (res) => {
                                toast.success(`Updating to ${v.version}`);
                                onJob?.(res.job.id);
                              },
                              onError: (err) => {
                                toast.error(
                                  err instanceof ApiError ? err.message : "update failed",
                                );
                              },
                            });
                          })();
                        }}
                      >
                        Update to {v.version}
                      </Button>
                    )}
                  </TableCell>
                </TableRow>
              );
            })}
          </TableBody>
        </Table>
      )}
      {update.isError && update.error instanceof ApiError ? (
        <p role="alert" className="text-sm text-destructive">
          {update.error.message}
        </p>
      ) : null}
    </div>
  );
}
