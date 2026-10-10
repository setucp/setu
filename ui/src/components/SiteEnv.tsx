import * as React from "react";
import { Eye, Pencil, RefreshCw, X } from "lucide-react";
import { toast } from "sonner";
import { usePutSiteEnv, useRevealEnv, useSiteEnv } from "@/lib/api";
import { ApiError, type EnvEntry } from "@/lib/client";
import { CopyField } from "./CopyButton";
import { ErrorText } from "./ErrorText";
import { ResponsiveTable, type Column } from "./ResponsiveTable";
import { Alert } from "./ui/alert";
import { Badge } from "./ui/badge";
import { Button } from "./ui/button";
import { Field } from "./ui/field";
import { Input } from "./ui/input";
import { Modal } from "./ui/dialog";
import { SkeletonRows } from "./ui/skeleton";

function Masked() {
  return (
    <span className="inline-flex items-center">
      <span aria-hidden="true" className="tracking-[0.2em] text-muted-foreground">
        ••••••••
      </span>
      <span className="sr-only">Masked value</span>
    </span>
  );
}

/**
 * Masked env table. The list never carries a secret (`[MASKED_VALUE]`);
 * Reveal opens a dialog that asks for the operator's own password and
 * calls `POST /sites/{id}/env/{name}/reveal`, which audits every call.
 * Non-secret `value`/`prompt` variables are edited in place; saving PUTs
 * them (a restart job starts) and reports the job.
 */
export function SiteEnv({
  siteId,
  onSubmitted,
}: {
  siteId: string;
  onSubmitted?: (jobId: string) => void;
}) {
  const env = useSiteEnv(siteId);
  const put = usePutSiteEnv(siteId);
  const [armed, setArmed] = React.useState<Record<string, string>>({});
  const [revealing, setRevealing] = React.useState<EnvEntry | null>(null);

  if (env.isPending) return <SkeletonRows rows={5} label="Loading environment…" />;
  if (env.error instanceof ApiError) return <ErrorText error={env.error} />;
  const rows = env.data ?? [];
  const edited = Object.entries(armed).filter(([, v]) => v !== "");

  function arm(name: string) {
    setArmed((prev) => (prev[name] === undefined ? { ...prev, [name]: "" } : prev));
  }
  function disarm(name: string) {
    setArmed((prev) => {
      const next = { ...prev };
      delete next[name];
      return next;
    });
  }

  const columns: Column<EnvEntry>[] = [
    {
      id: "name",
      header: "Name",
      cell: (row) => (
        <span className="flex flex-wrap items-center gap-2">
          <code className="text-xs font-medium">{row.name}</code>
          {row.secret ? <Badge variant="secondary">secret</Badge> : null}
          {row.once ? <Badge variant="warning">once</Badge> : null}
        </span>
      ),
    },
    {
      id: "value",
      header: "Value",
      className: "w-full min-w-48 font-mono text-xs",
      cell: (row) =>
        armed[row.name] !== undefined ? (
          <Input
            aria-label={`New value for ${row.name}`}
            type={row.secret ? "password" : "text"}
            placeholder="Enter a new value"
            className="h-9 font-mono text-xs"
            autoFocus
            value={armed[row.name] ?? ""}
            onChange={(e) => setArmed((prev) => ({ ...prev, [row.name]: e.target.value }))}
          />
        ) : row.secret ? (
          <Masked />
        ) : (
          <span className="break-all">{row.value}</span>
        ),
    },
    {
      id: "actions",
      header: <span className="sr-only">Actions</span>,
      hideOnCard: true,
      className: "text-right",
      cell: (row) => rowActions(row),
    },
  ];

  function rowActions(row: EnvEntry) {
    if (armed[row.name] !== undefined) {
      return (
        <Button type="button" variant="ghost" size="sm" onClick={() => disarm(row.name)}>
          <X aria-hidden="true" />
          Cancel
        </Button>
      );
    }
    if (row.secret) {
      return (
        <span className="flex justify-end gap-1">
          <Button type="button" variant="outline" size="sm" onClick={() => setRevealing(row)}>
            <Eye aria-hidden="true" />
            Reveal
          </Button>
          <Button
            type="button"
            variant="ghost"
            size="sm"
            aria-label={`Replace ${row.name}`}
            onClick={() => arm(row.name)}
          >
            <RefreshCw aria-hidden="true" />
            Replace
          </Button>
        </span>
      );
    }
    return (
      <Button type="button" variant="outline" size="sm" onClick={() => arm(row.name)}>
        <Pencil aria-hidden="true" />
        Edit
      </Button>
    );
  }

  return (
    <div className="flex flex-col gap-4">
      <Alert title="Secrets stay masked">
        Reveal asks for your password and every reveal is audited. <code>once</code> values
        reached the app on its first boot only. Only <code>value</code> and <code>prompt</code>{" "}
        variables can be changed — the server rejects generated and derived ones.
      </Alert>
      <ResponsiveTable
        label="Environment"
        rows={rows}
        columns={columns}
        getKey={(r) => r.name}
        cardActions={(row) => rowActions(row)}
      />
      {edited.length > 0 ? (
        <div className="sticky bottom-4 z-10 flex flex-col gap-3 rounded-lg border bg-popover p-3 shadow-overlay sm:flex-row sm:items-center">
          <p className="flex-1 text-sm">
            {edited.length} unsaved {edited.length === 1 ? "change" : "changes"}. Saving restarts
            the site.
          </p>
          <ErrorText error={put.isError ? put.error : null} />
          <Button
            type="button"
            variant="outline"
            onClick={() => setArmed({})}
            disabled={put.isPending}
          >
            Discard
          </Button>
          <Button
            type="button"
            loading={put.isPending}
            onClick={() => {
              put.mutate(Object.fromEntries(edited), {
                onSuccess: (res) => {
                  toast.success("Environment updated — restarting");
                  setArmed({});
                  onSubmitted?.(res.job.id);
                },
                onError: (err) => {
                  toast.error(err instanceof ApiError ? err.message : "Update failed");
                },
              });
            }}
          >
            {put.isPending ? "Saving…" : `Save changes (${edited.length})`}
          </Button>
        </div>
      ) : null}
      <RevealDialog siteId={siteId} entry={revealing} onClose={() => setRevealing(null)} />
    </div>
  );
}

/**
 * Password re-authentication, then the plaintext, shown in this dialog
 * only: closing it drops the value (nothing is cached), and every reveal
 * is a fresh, audited request.
 */
function RevealDialog({
  siteId,
  entry,
  onClose,
}: {
  siteId: string;
  entry: EnvEntry | null;
  onClose: () => void;
}) {
  const reveal = useRevealEnv(siteId);
  const [password, setPassword] = React.useState("");
  const name = entry?.name ?? "";
  const reset = reveal.reset;

  React.useEffect(() => {
    setPassword("");
    reset();
  }, [entry, reset]);

  const value = reveal.data?.value;

  function close() {
    setPassword("");
    reset();
    onClose();
  }

  return (
    <Modal
      open={entry !== null}
      onClose={close}
      title={`Reveal ${name}`}
      description="Confirm with your own password. Each reveal is recorded in the audit log."
      footer={
        value === undefined ? (
          <>
            <Button variant="outline" onClick={close}>
              Cancel
            </Button>
            <Button
              type="submit"
              form="reveal-form"
              disabled={password === ""}
              loading={reveal.isPending}
            >
              {reveal.isPending ? "Checking…" : "Reveal"}
            </Button>
          </>
        ) : (
          <Button onClick={close}>Done</Button>
        )
      }
    >
      <div className="flex flex-col gap-4">
        {entry?.once === true ? (
          <Alert variant="warning" title="A once value">
            <span className="font-medium text-foreground">{name}</span> reached the app on its
            first boot only, and was removed from the app&apos;s environment after the first
            successful health check. Setu keeps it sealed so you can read it here — for example,
            an initial admin password. Changing it in the app does not change it here.
          </Alert>
        ) : null}
        {value === undefined ? (
          <form
            id="reveal-form"
            className="flex flex-col gap-3"
            onSubmit={(e) => {
              e.preventDefault();
              if (entry === null || password === "") return;
              reveal.mutate({ name: entry.name, password }, { onSettled: () => setPassword("") });
            }}
          >
            <Field label="Your password">
              <Input
                type="password"
                autoComplete="current-password"
                autoFocus
                value={password}
                onChange={(e) => setPassword(e.target.value)}
              />
            </Field>
            {reveal.isError ? (
              <Alert variant="destructive">
                {reveal.error instanceof ApiError ? reveal.error.message : "Reveal failed"}
              </Alert>
            ) : null}
          </form>
        ) : (
          <div className="flex flex-col gap-2">
            <CopyField value={value} label="Value" />
            <p className="text-xs text-muted-foreground">
              Shown once — closing this dialog forgets it.
            </p>
          </div>
        )}
      </div>
    </Modal>
  );
}
