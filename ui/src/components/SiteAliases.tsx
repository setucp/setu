import * as React from "react";
import { Plus, Save, Trash2, Undo2 } from "lucide-react";
import { toast } from "sonner";
import { useAliases, useMe, useSetAliases } from "@/lib/api";
import { ApiError, type Alias, type AliasMode, type SiteDetail } from "@/lib/client";
import { asciiHost, normaliseHost, wwwName } from "@/lib/domain";
import { DomainCheck } from "./DomainCheck";
import { ErrorText } from "./ErrorText";
import { Alert } from "./ui/alert";
import { Button } from "./ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "./ui/card";
import { Field } from "./ui/field";
import { Input } from "./ui/input";
import { Select } from "./ui/select";
import { SkeletonRows } from "./ui/skeleton";

/** The server's limit (`setu_db::MAX_ALIASES`). */
export const MAX_ALIASES = 10;

/**
 * Why `typed` cannot be added to `rows`, or `null` when it can. Empty input
 * is not an error (nothing to add yet). Mirrors the server's checks in
 * `crates/api/src/rest/aliases.rs`; another site's name is only refused
 * there (409).
 */
export function aliasError(typed: string, rows: Alias[], siteDomain: string): string | null {
  if (normaliseHost(typed) === "") return null;
  const host = asciiHost(typed);
  if (host === null) return "A bare hostname, e.g. old.example.com.";
  if (host === siteDomain) return "That is the site's own domain.";
  if (rows.some((r) => r.host === host)) return "Already listed.";
  return null;
}

/** Same hosts with the same modes, in the same order. */
function sameList(a: Alias[], b: Alias[]): boolean {
  return a.length === b.length && a.every((x, i) => x.host === b[i]?.host && x.mode === b[i]?.mode);
}

/** A site may change its route only while it is running or stopped. */
function settled(status: string) {
  return status === "running" || status === "stopped";
}

/**
 * Extra hostnames of a site (`/sites/{id}/aliases`): each one either
 * redirects (308, path kept) to the site's domain or serves the site too.
 * Operators edit a draft of the whole list and save it at once, which
 * starts a route update job (reported through `onSubmitted`); the DNS
 * pre-check runs on a host before it is added, since Caddy can only get
 * its certificate once the name points at the server. Viewers see the list.
 */
export function SiteAliases({
  site,
  onSubmitted,
}: {
  site: SiteDetail;
  onSubmitted?: (jobId: string) => void;
}) {
  const aliases = useAliases(site.id);
  const me = useMe();
  const set = useSetAliases(site.id);
  const [draft, setDraft] = React.useState<Alias[] | null>(null);
  const [typed, setTyped] = React.useState("");
  const [mode, setMode] = React.useState<AliasMode>("redirect");

  if (aliases.isError) return <ErrorText error={aliases.error} />;
  if (aliases.data === undefined) return <SkeletonRows rows={2} label="Loading hostnames…" />;

  const saved = aliases.data;
  const rows = draft ?? saved;
  const dirty = draft !== null && !sameList(draft, saved);
  const canEdit = me.data !== undefined && me.data.role !== "viewer";
  const busy = !settled(site.status);
  const full = rows.length >= MAX_ALIASES;
  // The stored (ASCII) form, so a saved list compares equal to the draft.
  const host = asciiHost(typed) ?? normaliseHost(typed);
  const typedErr = aliasError(typed, rows, site.domain);
  const canAdd = host !== "" && typedErr === null && !full;

  function add(e: React.FormEvent) {
    e.preventDefault();
    if (!canAdd) return;
    setDraft([...rows, { host, mode }]);
    setTyped("");
  }

  function save() {
    if (!dirty || busy) return;
    set.mutate(rows, {
      onSuccess: (res) => {
        setDraft(null);
        toast.success(
          res.aliases.length === 0
            ? `${site.name} answers on ${site.domain} only`
            : `Hostnames saved for ${site.name}`,
        );
        onSubmitted?.(res.job.id);
      },
      onError: (err) => {
        toast.error(err instanceof ApiError ? err.message : "Saving the hostnames failed");
      },
    });
  }

  return (
    <Card>
      <CardHeader>
        <CardTitle>Extra hostnames</CardTitle>
        <CardDescription>
          Other names for {site.domain}: each one redirects to it (308, the path is kept) or serves
          the site as well. Point a name&apos;s DNS at the server first; Caddy gets its certificate
          once the route has it. At most {MAX_ALIASES}.
        </CardDescription>
      </CardHeader>
      <CardContent className="flex flex-col gap-4">
        {rows.length === 0 ? (
          <p className="rounded-md border border-dashed px-4 py-5 text-center text-sm text-muted-foreground">
            No extra hostnames: the site answers on {site.domain}.
          </p>
        ) : (
          <ul aria-label="Hostnames" className="divide-y rounded-md border">
            {rows.map((a, i) => (
              <li
                key={a.host}
                className="flex flex-wrap items-center gap-x-3 gap-y-2 px-3 py-2.5 text-sm"
              >
                <span className="min-w-0 flex-1 font-mono text-xs break-all">
                  {a.host}
                  {site.www_redirect && a.host === wwwName(site.domain) ? (
                    <span className="block font-sans text-muted-foreground">
                      Replaces the automatic www redirect.
                    </span>
                  ) : null}
                </span>
                {canEdit ? (
                  <>
                    <span className="w-48">
                      <Select
                        aria-label={`How ${a.host} answers`}
                        value={a.mode}
                        onChange={(e) =>
                          setDraft(
                            rows.map((r, j) =>
                              j === i ? { ...r, mode: e.target.value as AliasMode } : r,
                            ),
                          )
                        }
                      >
                        <option value="redirect">Redirects</option>
                        <option value="serve">Serves the site</option>
                      </Select>
                    </span>
                    <Button
                      type="button"
                      variant="ghost"
                      size="sm"
                      aria-label={`Remove ${a.host}`}
                      onClick={() => setDraft(rows.filter((_, j) => j !== i))}
                    >
                      <Trash2 aria-hidden="true" />
                    </Button>
                  </>
                ) : (
                  <span className="text-muted-foreground">
                    {a.mode === "redirect" ? "redirects" : "serves the site"}
                  </span>
                )}
              </li>
            ))}
          </ul>
        )}

        {canEdit ? (
          <>
            <form className="flex flex-col gap-3" onSubmit={add} aria-label="Add a hostname">
              <div className="grid gap-3 sm:grid-cols-[minmax(0,1fr)_12rem_auto] sm:items-start">
                <Field
                  label="Hostname"
                  error={typedErr}
                  hint={full ? `The site has the maximum of ${MAX_ALIASES}.` : undefined}
                >
                  <Input
                    autoComplete="off"
                    spellCheck={false}
                    placeholder="old.example.com"
                    value={typed}
                    disabled={full}
                    onChange={(e) => setTyped(e.target.value)}
                  />
                </Field>
                <Field label="Answers with">
                  <Select value={mode} onChange={(e) => setMode(e.target.value as AliasMode)}>
                    <option value="redirect">A redirect</option>
                    <option value="serve">The site</option>
                  </Select>
                </Field>
                <Button type="submit" variant="outline" disabled={!canAdd} className="sm:mt-6">
                  <Plus aria-hidden="true" />
                  Add
                </Button>
              </div>
              <DomainCheck
                domain={canAdd ? host : null}
                serverId={site.server_id}
                context="alias"
              />
            </form>
            {busy ? (
              <Alert variant="warning">
                The site is {site.status}; hostnames can change once it is running or stopped.
              </Alert>
            ) : null}
            <div className="flex flex-wrap items-center gap-2 border-t pt-4">
              <Button
                type="button"
                disabled={!dirty || busy}
                loading={set.isPending}
                onClick={save}
              >
                <Save aria-hidden="true" />
                Save hostnames
              </Button>
              {dirty ? (
                <Button
                  type="button"
                  variant="outline"
                  disabled={set.isPending}
                  onClick={() => setDraft(null)}
                >
                  <Undo2 aria-hidden="true" />
                  Discard changes
                </Button>
              ) : null}
              <span className="text-xs text-muted-foreground">
                {dirty
                  ? "Not saved yet."
                  : "Saving updates the site's route; the app does not restart."}
              </span>
            </div>
          </>
        ) : null}
      </CardContent>
    </Card>
  );
}
