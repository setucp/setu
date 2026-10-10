import * as React from "react";
import { toast } from "sonner";
import { usePutSiteEnv, useRevealEnv, useSiteEnv } from "@/lib/api";
import { ApiError, type EnvEntry } from "@/lib/client";
import { CopyButton } from "./CopyButton";
import { Badge } from "./ui/badge";
import { Button } from "./ui/button";
import { Input } from "./ui/input";
import { Modal } from "./ui/dialog";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "./ui/table";

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

  if (env.isPending) {
    return <p className="text-sm text-muted-foreground">Loading…</p>;
  }
  if (env.error instanceof ApiError) {
    return (
      <p role="alert" className="text-sm text-destructive">
        {env.error.message}
      </p>
    );
  }
  const rows = env.data ?? [];
  const edited = Object.entries(armed).filter(([, v]) => v !== "");

  function arm(name: string) {
    setArmed((prev) => (prev[name] === undefined ? { ...prev, [name]: "" } : prev));
  }

  return (
    <div className="flex flex-col gap-3">
      <p className="text-sm text-muted-foreground">
        Secret values stay masked; Reveal asks for your password and is audited.{" "}
        <code>once</code> values reached the app on its first boot only. Only{" "}
        <code>value</code> and <code>prompt</code> variables can be changed — the
        server rejects generated and derived ones.
      </p>
      <Table>
        <TableHeader>
          <TableRow>
            <TableHead>Name</TableHead>
            <TableHead>Value</TableHead>
            <TableHead>Flags</TableHead>
            <TableHead>
              <span className="sr-only">Actions</span>
            </TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          {rows.map((row) => {
            const isArmed = armed[row.name] !== undefined;
            return (
              <TableRow key={row.name}>
                <TableCell className="font-mono">{row.name}</TableCell>
                <TableCell className="font-mono">
                  {isArmed ? (
                    <Input
                      aria-label={`New value for ${row.name}`}
                      type={row.secret ? "password" : "text"}
                      placeholder="Enter a new value"
                      value={armed[row.name] ?? ""}
                      onChange={(e) =>
                        setArmed((prev) => ({ ...prev, [row.name]: e.target.value }))
                      }
                    />
                  ) : (
                    <span>{row.secret ? "[MASKED_VALUE]" : row.value}</span>
                  )}
                </TableCell>
                <TableCell>
                  <span className="flex gap-1">
                    {row.secret ? <Badge variant="secondary">secret</Badge> : null}
                    {row.once ? <Badge variant="outline">once</Badge> : null}
                  </span>
                </TableCell>
                <TableCell>
                  {isArmed ? (
                    <Button
                      type="button"
                      variant="ghost"
                      size="sm"
                      onClick={() =>
                        setArmed((prev) => {
                          const next = { ...prev };
                          delete next[row.name];
                          return next;
                        })
                      }
                    >
                      Cancel
                    </Button>
                  ) : row.secret ? (
                    <span className="flex gap-1">
                      <Button
                        type="button"
                        variant="outline"
                        size="sm"
                        onClick={() => setRevealing(row)}
                      >
                        Reveal
                      </Button>
                      <Button
                        type="button"
                        variant="ghost"
                        size="sm"
                        aria-label={`Replace ${row.name}`}
                        onClick={() => arm(row.name)}
                      >
                        Replace
                      </Button>
                    </span>
                  ) : (
                    <Button
                      type="button"
                      variant="outline"
                      size="sm"
                      onClick={() => arm(row.name)}
                    >
                      Edit
                    </Button>
                  )}
                </TableCell>
              </TableRow>
            );
          })}
        </TableBody>
      </Table>
      {edited.length > 0 ? (
        <div className="flex items-center gap-2">
          <Button
            type="button"
            disabled={put.isPending}
            onClick={() => {
              put.mutate(Object.fromEntries(edited), {
                onSuccess: (res) => {
                  toast.success("Environment updated — restarting");
                  setArmed({});
                  onSubmitted?.(res.job.id);
                },
                onError: (err) => {
                  toast.error(err instanceof ApiError ? err.message : "update failed");
                },
              });
            }}
          >
            {put.isPending ? "Saving…" : `Save changes (${edited.length})`}
          </Button>
          {put.isError && put.error instanceof ApiError ? (
            <p role="alert" className="text-sm text-destructive">
              {put.error.message}
            </p>
          ) : null}
        </div>
      ) : null}
      <RevealDialog
        siteId={siteId}
        entry={revealing}
        onClose={() => setRevealing(null)}
      />
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
              disabled={password === "" || reveal.isPending}
            >
              {reveal.isPending ? "Checking…" : "Reveal"}
            </Button>
          </>
        ) : (
          <Button onClick={close}>Done</Button>
        )
      }
    >
      <div className="flex flex-col gap-3">
        {entry?.once === true ? (
          <p className="rounded-md border border-yellow-500/40 bg-yellow-500/10 p-3 text-sm">
            <span className="font-medium">{name}</span> is a <code>once</code> value:
            the app received it on its first boot only, and it was removed from the
            app's environment after the first successful health check. Setu keeps
            it sealed so you can read it here — for example, an initial admin
            password. Changing it in the app does not change it here.
          </p>
        ) : null}
        {value === undefined ? (
          <form
            id="reveal-form"
            className="flex flex-col gap-3"
            onSubmit={(e) => {
              e.preventDefault();
              if (entry === null || password === "") return;
              reveal.mutate(
                { name: entry.name, password },
                { onSettled: () => setPassword("") },
              );
            }}
          >
            <label className="flex flex-col gap-1.5 text-sm font-medium">
              Your password
              <Input
                type="password"
                autoComplete="current-password"
                autoFocus
                value={password}
                onChange={(e) => setPassword(e.target.value)}
              />
            </label>
            {reveal.isError ? (
              <p role="alert" className="text-sm text-destructive">
                {reveal.error instanceof ApiError ? reveal.error.message : "reveal failed"}
              </p>
            ) : null}
          </form>
        ) : (
          <>
            <code className="block overflow-x-auto rounded-md border bg-muted/40 p-3 font-mono text-sm break-all">
              {value}
            </code>
            <div className="flex items-center gap-2">
              <CopyButton text={value} label="Value" />
              <span className="text-xs text-muted-foreground">
                Shown once — closing this dialog forgets it.
              </span>
            </div>
          </>
        )}
      </div>
    </Modal>
  );
}
