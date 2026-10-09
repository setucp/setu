import * as React from "react";
import { toast } from "sonner";
import { usePutSiteEnv, useSiteEnv } from "@/lib/api";
import { ApiError } from "@/lib/client";
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
 * Masked env table. Secret values are never returned as plaintext by the API
 * (`[MASKED_VALUE]`), so Reveal — behind an explicit confirm dialog — arms an
 * input for entering a *new* value rather than displaying the old one.
 * Saving PUTs the edited values (a restart job starts) and reports the job.
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
  const [confirmName, setConfirmName] = React.useState<string | null>(null);

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
        Secret values stay masked. Only <code>value</code> and <code>prompt</code>{" "}
        variables can be edited — generated and derived ones are rejected by the
        server.
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
                    <Button
                      type="button"
                      variant="outline"
                      size="sm"
                      onClick={() => setConfirmName(row.name)}
                    >
                      Reveal
                    </Button>
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
      <Modal
        open={confirmName !== null}
        onClose={() => setConfirmName(null)}
        title={`Reveal ${confirmName ?? ""}?`}
        description="The new value you enter will be visible on screen. The current secret value is never shown — it stays on the server."
        footer={
          <>
            <Button variant="outline" onClick={() => setConfirmName(null)}>
              Cancel
            </Button>
            <Button
              data-testid="confirm-accept"
              onClick={() => {
                if (confirmName !== null) arm(confirmName);
                setConfirmName(null);
              }}
            >
              Reveal
            </Button>
          </>
        }
      />
    </div>
  );
}
