import * as React from "react";
import { toast } from "sonner";
import { useDeleteSite, useSiteAction } from "@/lib/api";
import { ApiError, type Site } from "@/lib/client";
import { Button } from "./ui/button";
import { Input } from "./ui/input";
import { Modal, useConfirm } from "./ui/dialog";

/**
 * Stop / start / restart / retry / delete. Every action confirms and names the
 * site; delete additionally requires typing the site name, and navigates back
 * to the sites list once its job is accepted (202).
 */
export function SiteDanger({
  site,
  onJob,
  onDeleted,
}: {
  site: Site;
  onJob?: (jobId: string) => void;
  onDeleted?: (jobId: string) => void;
}) {
  const action = useSiteAction(site.id);
  const destroy = useDeleteSite(site.id);
  const confirm = useConfirm();
  const [deleteOpen, setDeleteOpen] = React.useState(false);
  const [typed, setTyped] = React.useState("");

  const busy = action.isPending || destroy.isPending;
  const error =
    action.error instanceof ApiError
      ? action.error
      : destroy.error instanceof ApiError
        ? destroy.error
        : null;

  function run(kind: "start" | "stop" | "restart" | "retry", label: string) {
    void (async () => {
      const ok = await confirm({
        title: `${label} ${site.name}?`,
        description: `This ${label.toLowerCase()}s the site ${site.name} (${site.domain}).`,
        confirmLabel: label,
        destructive: kind === "stop",
      });
      if (!ok) return;
      action.mutate(kind, {
        onSuccess: (res) => {
          toast.success(`${label} started for ${site.name}`);
          onJob?.(res.job.id);
        },
        onError: (err) => {
          toast.error(err instanceof ApiError ? err.message : `${label} failed`);
        },
      });
    })();
  }

  return (
    <div className="flex max-w-xl flex-col gap-4">
      <div className="flex flex-wrap gap-2">
        <Button type="button" variant="outline" disabled={busy} onClick={() => run("start", "Start")}>
          Start
        </Button>
        <Button type="button" variant="outline" disabled={busy} onClick={() => run("stop", "Stop")}>
          Stop
        </Button>
        <Button
          type="button"
          variant="outline"
          disabled={busy}
          onClick={() => run("restart", "Restart")}
        >
          Restart
        </Button>
        {site.status === "failed" ? (
          <Button
            type="button"
            variant="outline"
            disabled={busy}
            onClick={() => run("retry", "Retry")}
          >
            Retry from failed
          </Button>
        ) : null}
      </div>
      <div className="rounded-md border border-destructive/40 p-4">
        <h2 className="text-sm font-semibold text-destructive">Delete this site</h2>
        <p className="mt-1 text-sm text-muted-foreground">
          Stops {site.name}, removes its route, database, user and directory after
          a final backup, and deletes the row. This cannot be undone.
        </p>
        <Button
          type="button"
          variant="destructive"
          className="mt-3"
          disabled={busy}
          onClick={() => {
            setTyped("");
            setDeleteOpen(true);
          }}
        >
          Delete {site.name}
        </Button>
      </div>
      {error !== null ? (
        <p role="alert" className="text-sm text-destructive">
          {error.message}
        </p>
      ) : null}
      <Modal
        open={deleteOpen}
        onClose={() => setDeleteOpen(false)}
        title={`Delete ${site.name}?`}
        description={`Type ${site.name} to confirm. The site, its database and its files are removed after a final backup.`}
        footer={
          <>
            <Button variant="outline" onClick={() => setDeleteOpen(false)}>
              Cancel
            </Button>
            <Button
              variant="destructive"
              data-testid="confirm-accept"
              disabled={typed.trim() !== site.name || destroy.isPending}
              onClick={() => {
                destroy.mutate(undefined, {
                  onSuccess: (res) => {
                    toast.success(`Deleting ${site.name}`);
                    setDeleteOpen(false);
                    onDeleted?.(res.job.id);
                  },
                  onError: (err) => {
                    toast.error(
                      err instanceof ApiError ? err.message : "delete failed",
                    );
                  },
                });
              }}
            >
              {destroy.isPending ? "Deleting…" : `Delete ${site.name}`}
            </Button>
          </>
        }
      >
        <Input
          aria-label={`Type ${site.name} to confirm`}
          value={typed}
          onChange={(e) => setTyped(e.target.value)}
          placeholder={site.name}
        />
      </Modal>
    </div>
  );
}
