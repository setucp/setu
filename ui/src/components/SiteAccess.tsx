import * as React from "react";
import { LockKeyhole, LockKeyholeOpen, RotateCw, Trash2, TriangleAlert } from "lucide-react";
import { toast } from "sonner";
import { useBasicAuth, useMe, useSetBasicAuth } from "@/lib/api";
import { ApiError, type SiteDetail } from "@/lib/client";
import { AppLink } from "./AppLink";
import { ErrorText } from "./ErrorText";
import { RelativeTime } from "./RelativeTime";
import { Alert } from "./ui/alert";
import { Button } from "./ui/button";
import { Card, CardDescription, CardHeader, CardTitle } from "./ui/card";
import { useConfirm } from "./ui/dialog";
import { Field } from "./ui/field";
import { Input } from "./ui/input";
import { SkeletonRows } from "./ui/skeleton";

/** The server's rules (`crates/api/src/rest/basic_auth.rs::validate`). */
const USERNAME_RE = /^[A-Za-z0-9._-]{1,64}$/;
// Rust's `char::is_control` is the Unicode Cc category.
// eslint-disable-next-line no-control-regex
const CONTROL_RE = /[\u0000-\u001f\u007f-\u009f]/;
const MIN_PASSWORD = 8;
const MAX_PASSWORD = 72;

export function usernameError(name: string): string | null {
  if (name === "") return null;
  return USERNAME_RE.test(name) ? null : "1 to 64 letters, digits, '.', '_' or '-'.";
}

/** Length is counted in UTF-8 bytes: bcrypt reads at most 72. */
export function passwordError(password: string): string | null {
  if (password === "") return null;
  const bytes = new TextEncoder().encode(password).length;
  if (bytes < MIN_PASSWORD || bytes > MAX_PASSWORD) {
    return `${MIN_PASSWORD} to ${MAX_PASSWORD} bytes (this one is ${bytes}).`;
  }
  if (CONTROL_RE.test(password)) return "No control characters (tabs, line breaks…).";
  return null;
}

/** A site may change its route only while it is running or stopped. */
function settled(status: string) {
  return status === "running" || status === "stopped";
}

/**
 * HTTP basic auth in front of the site (`/sites/{id}/basic-auth`). Caddy
 * asks every visitor for the user name and password; the panel stores
 * only a bcrypt hash, so the password is never shown again — replacing
 * it means entering a new one. Setting, replacing or removing it starts
 * a route update job (reported through `onSubmitted`). The stored setting
 * is in force only once that job is done: while it runs the tab says so,
 * and a failed job is shown as not applied (the route keeps its previous
 * setting), never as protected. Viewers see the state only; the server
 * enforces the operator role either way.
 */
export function SiteAccess({
  site,
  onSubmitted,
}: {
  site: SiteDetail;
  onSubmitted?: (jobId: string) => void;
}) {
  const auth = useBasicAuth(site.id);
  const me = useMe();
  const set = useSetBasicAuth(site.id);
  const confirm = useConfirm();
  const [username, setUsername] = React.useState("");
  const [password, setPassword] = React.useState("");
  const [again, setAgain] = React.useState("");
  const [touched, setTouched] = React.useState(false);

  const current = auth.data?.basic_auth ?? null;
  React.useEffect(() => {
    if (current !== null) setUsername((u) => (u === "" ? current.username : u));
  }, [current]);

  if (auth.isPending) return <SkeletonRows rows={3} label="Loading access…" />;
  // Any failure (not only an ApiError: a dropped connection throws a
  // TypeError) must not read as "open to everyone".
  if (auth.isError) return <ErrorText error={auth.error} />;
  if (auth.data === undefined) return <SkeletonRows rows={3} label="Loading access…" />;

  const route = auth.data.route;
  const canEdit = me.data !== undefined && me.data.role !== "viewer";
  const busy = !settled(site.status);
  const nameErr = usernameError(username);
  const passErr = passwordError(password);
  const againErr = touched && again !== password ? "The two passwords differ." : null;
  const ready =
    username !== "" &&
    password !== "" &&
    again === password &&
    nameErr === null &&
    passErr === null;

  function done(message: string) {
    return {
      onSuccess: (res: { job: { id: string } }) => {
        toast.success(message);
        setPassword("");
        setAgain("");
        setTouched(false);
        onSubmitted?.(res.job.id);
      },
      onError: (err: Error) => {
        toast.error(err instanceof ApiError ? err.message : "Update failed");
      },
    };
  }

  function submit(e: React.FormEvent) {
    e.preventDefault();
    setTouched(true);
    if (!ready || busy) return;
    set.mutate(
      { username, password },
      done(current === null ? `${site.name} now asks for a password` : "Credentials replaced"),
    );
  }

  async function remove() {
    const ok = await confirm({
      title: `Open ${site.name} to everyone?`,
      description: `Visitors to ${site.domain} will no longer be asked for a user name and password.`,
      confirmLabel: "Remove password",
      destructive: true,
    });
    if (ok) set.mutate(null, done(`${site.name} is open to everyone`));
  }

  const jobLink =
    route.job_id === null || route.job_id === undefined ? null : (
      <AppLink href={`/jobs/${encodeURIComponent(route.job_id)}`} className="text-link underline">
        route update job
      </AppLink>
    );

  return (
    <div className="flex flex-col gap-4">
      <Card>
        <CardHeader className="pb-5 sm:pb-6">
          <CardTitle className="flex items-center gap-2">
            {route.status === "failed" ? (
              <>
                <TriangleAlert className="size-4 text-destructive" aria-hidden="true" />
                {current === null ? "Removing the password failed" : "Password not applied"}
              </>
            ) : current === null ? (
              <>
                <LockKeyholeOpen className="size-4 text-muted-foreground" aria-hidden="true" />
                Open to everyone
              </>
            ) : (
              <>
                <LockKeyhole className="size-4 text-link" aria-hidden="true" />
                Password protected
              </>
            )}
            {route.status === "applying" ? (
              <span className="text-sm font-normal text-muted-foreground">(applying…)</span>
            ) : null}
          </CardTitle>
          <CardDescription>
            {route.status === "failed" ? (
              <>
                The last change did not reach the site&apos;s route, so {site.domain} still has its
                previous access setting
                {current === null ? " and may still ask for a password" : " and may be open to everyone"}
                .{" "}
                {current === null
                  ? "Retry the removal below."
                  : `Set the credentials for ${current.username} again to retry.`}{" "}
                {jobLink !== null ? <>See the {jobLink}.</> : null}
              </>
            ) : current === null ? (
              <>Anyone who knows {site.domain} can open it.</>
            ) : (
              <>
                Visitors sign in as <code className="text-foreground">{current.username}</code>;
                changed <RelativeTime iso={current.updated_at} />.
              </>
            )}
          </CardDescription>
          {route.status === "failed" && route.error ? (
            <p className="mt-2 text-sm text-destructive">{route.error}</p>
          ) : null}
          {route.status === "failed" && current === null && canEdit ? (
            <div className="mt-3">
              <Button
                type="button"
                variant="outline"
                disabled={busy || set.isPending}
                loading={set.isPending}
                onClick={() => set.mutate(null, done("Retrying the removal"))}
              >
                <RotateCw aria-hidden="true" />
                Retry removal
              </Button>
            </div>
          ) : null}
        </CardHeader>
      </Card>

      {canEdit ? (
        <Card>
          <CardHeader>
            <CardTitle>
              {current === null ? "Ask for a password" : "Replace the credentials"}
            </CardTitle>
            <CardDescription>
              The browser asks every visitor for these (HTTP basic auth). Setu keeps only a hash, so
              the password cannot be shown again. Saving updates the site&apos;s route; the app does
              not restart.
            </CardDescription>
          </CardHeader>
          <form
            className="flex flex-col gap-4 px-5 pb-5 sm:px-6 sm:pb-6"
            onSubmit={submit}
            aria-label="Basic auth"
          >
            {busy ? (
              <Alert variant="warning">
                The site is {site.status}; access can change once it is running or stopped.
              </Alert>
            ) : null}
            <div className="grid gap-4 sm:grid-cols-3">
              <Field label="User name" error={nameErr}>
                <Input
                  autoComplete="off"
                  spellCheck={false}
                  value={username}
                  onChange={(e) => setUsername(e.target.value)}
                />
              </Field>
              <Field label="Password" error={passErr} hint="8 to 72 bytes.">
                <Input
                  type="password"
                  autoComplete="new-password"
                  value={password}
                  onChange={(e) => setPassword(e.target.value)}
                />
              </Field>
              <Field label="Password again" error={againErr}>
                <Input
                  type="password"
                  autoComplete="new-password"
                  value={again}
                  onChange={(e) => setAgain(e.target.value)}
                  onBlur={() => setTouched(true)}
                />
              </Field>
            </div>
            <div className="flex flex-wrap items-center gap-2">
              <Button type="submit" disabled={!ready || busy} loading={set.isPending}>
                <LockKeyhole aria-hidden="true" />
                {current === null ? "Protect site" : "Replace credentials"}
              </Button>
              {current !== null ? (
                <Button
                  type="button"
                  variant="outline"
                  disabled={busy || set.isPending}
                  onClick={() => void remove()}
                >
                  <Trash2 aria-hidden="true" />
                  Remove password
                </Button>
              ) : null}
            </div>
          </form>
        </Card>
      ) : null}
    </div>
  );
}
