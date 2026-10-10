import * as React from "react";
import { Play, RotateCw, RefreshCcw, Square, Trash2 } from "lucide-react";
import { toast } from "sonner";
import { useDeleteSite, useSiteAction } from "@/lib/api";
import { ApiError, type Site } from "@/lib/client";
import { ErrorText } from "./ErrorText";
import { Button } from "./ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "./ui/card";
import { Field } from "./ui/field";
import { Input } from "./ui/input";
import { Modal, useConfirm } from "./ui/dialog";

type Kind = "start" | "stop" | "restart" | "retry";

const VERB: Record<Kind, string> = {
  start: "starts",
  stop: "stops",
  restart: "restarts",
  retry: "retries the failed job for",
};

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

  function run(kind: Kind, label: string) {
    void (async () => {
      const ok = await confirm({
        title: `${label} ${site.name}?`,
        description: `This ${VERB[kind]} the site ${site.name} (${site.domain}).`,
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
    <div className="flex flex-col gap-6">
      <Card>
        <CardHeader>
          <CardTitle>Lifecycle</CardTitle>
          <CardDescription>
            Start, stop or restart the site&apos;s service. Each action runs as a job.
          </CardDescription>
        </CardHeader>
        <CardContent className="grid grid-cols-2 gap-2 sm:flex sm:flex-wrap">
          <Button type="button" variant="outline" disabled={busy} onClick={() => run("start", "Start")}>
            <Play aria-hidden="true" />
            Start
          </Button>
          <Button type="button" variant="outline" disabled={busy} onClick={() => run("stop", "Stop")}>
            <Square aria-hidden="true" />
            Stop
          </Button>
          <Button type="button" variant="outline" disabled={busy} onClick={() => run("restart", "Restart")}>
            <RotateCw aria-hidden="true" />
            Restart
          </Button>
          {site.status === "failed" ? (
            <Button type="button" variant="outline" disabled={busy} onClick={() => run("retry", "Retry")}>
              <RefreshCcw aria-hidden="true" />
              Retry from failed
            </Button>
          ) : null}
        </CardContent>
      </Card>

      <section
        aria-labelledby="delete-site-title"
        className="overflow-hidden rounded-lg border border-destructive/40 bg-card shadow-card"
      >
        <div className="flex flex-col gap-4 p-5 sm:flex-row sm:items-center sm:p-6">
          <div className="min-w-0 flex-1">
            <h2 id="delete-site-title" className="text-base font-semibold text-destructive">
              Delete this site
            </h2>
            <p className="mt-1 text-sm text-muted-foreground">
              Stops {site.name}, removes its route, database, user and directory after a final
              backup, and deletes the row. This cannot be undone.
            </p>
          </div>
          <Button
            type="button"
            variant="destructive"
            disabled={busy}
            onClick={() => {
              setTyped("");
              setDeleteOpen(true);
            }}
          >
            <Trash2 aria-hidden="true" />
            Delete {site.name}
          </Button>
        </div>
      </section>
      <ErrorText error={error} />
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
              disabled={typed.trim() !== site.name}
              loading={destroy.isPending}
              onClick={() => {
                destroy.mutate(undefined, {
                  onSuccess: (res) => {
                    toast.success(`Deleting ${site.name}`);
                    setDeleteOpen(false);
                    onDeleted?.(res.job.id);
                  },
                  onError: (err) => {
                    toast.error(err instanceof ApiError ? err.message : "Delete failed");
                  },
                });
              }}
            >
              {destroy.isPending ? "Deleting…" : `Delete ${site.name}`}
            </Button>
          </>
        }
      >
        <Field label={`Type ${site.name} to confirm`}>
          <Input
            value={typed}
            onChange={(e) => setTyped(e.target.value)}
            placeholder={site.name}
            autoComplete="off"
            autoFocus
          />
        </Field>
      </Modal>
    </div>
  );
}
