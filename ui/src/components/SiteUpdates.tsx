import * as React from "react";
import { ArrowUpCircle, CheckCircle2 } from "lucide-react";
import { toast } from "sonner";
import { useAppVersions, useJobs, useUpdateSite, useVersionPrompts } from "@/lib/api";
import { ApiError, type AppVersion, type Job } from "@/lib/client";
import { compareVersions } from "@/lib/format";
import { AppLink } from "./AppLink";
import { ErrorText } from "./ErrorText";
import { RelativeTime } from "./RelativeTime";
import { Alert } from "./ui/alert";
import { Badge } from "./ui/badge";
import { Button } from "./ui/button";
import { Card } from "./ui/card";
import { Field } from "./ui/field";
import { Modal } from "./ui/dialog";
import { Input } from "./ui/input";
import { SkeletonRows } from "./ui/skeleton";

/**
 * Updates tab: the running version (marked current) and the versions
 * newer than it, an update dialog that collects answers for `prompt`
 * variables the target version adds (`NAME=value` lines; the API exposes
 * no per-version manifest, so the server validates the names, R110), and
 * the outcome of the site's latest update job — including an automatic
 * rollback, read from the job error (`rollback: …` / `rollback failed: …`).
 */
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
  const jobs = useJobs();
  const [target, setTarget] = React.useState<AppVersion | null>(null);

  if (versions.isPending) return <SkeletonRows rows={3} label="Loading versions…" />;
  if (versions.error instanceof ApiError) return <ErrorText error={versions.error} />;
  const all = versions.data ?? [];
  const newer = all
    .filter((v) => compareVersions(v.version, currentVersion) > 0)
    .sort((a, b) => compareVersions(b.version, a.version));
  const current = all.find((v) => v.version === currentVersion);
  const latest = (jobs.data ?? [])
    .filter((j) => j.kind === "update_site" && j.site_id === siteId)
    .sort((a, b) => b.created_at.localeCompare(a.created_at))[0];

  return (
    <div className="flex flex-col gap-4">
      <div className="flex flex-wrap items-center gap-3">
        <p className="text-sm font-medium">{`Running ${currentVersion}.`}</p>
        {newer.length === 0 ? (
          <Badge variant="success" dot>
            Up to date
          </Badge>
        ) : (
          <Badge variant="info" dot>
            {newer.length} {newer.length === 1 ? "update" : "updates"} available
          </Badge>
        )}
      </div>
      {latest !== undefined ? <UpdateOutcome job={latest} /> : null}
      <Card>
        <ul aria-label="Versions" className="divide-y">
          {newer.map((v, i) => (
            <li key={v.id} className="flex flex-col gap-3 px-5 py-4 sm:flex-row sm:items-center">
              <div className="flex min-w-0 flex-1 items-center gap-3">
                <ArrowUpCircle className="size-5 shrink-0 text-link" aria-hidden="true" />
                <div className="min-w-0">
                  <p className="flex flex-wrap items-center gap-2">
                    <span className="font-mono text-sm font-semibold">{v.version}</span>
                    {i === 0 ? <Badge variant="info">latest</Badge> : null}
                    <Badge variant="secondary">{v.channel}</Badge>
                  </p>
                  <p className="mt-0.5 text-xs text-muted-foreground">
                    Published <RelativeTime iso={v.published_at} />
                  </p>
                </div>
              </div>
              <Button
                type="button"
                variant={i === 0 ? "default" : "outline"}
                size="sm"
                onClick={() => setTarget(v)}
              >
                Update to {v.version}
              </Button>
            </li>
          ))}
          <li className="flex items-center gap-3 bg-surface px-5 py-4">
            <CheckCircle2 className="size-5 shrink-0 text-success" aria-hidden="true" />
            <div className="min-w-0 flex-1">
              <p className="flex flex-wrap items-center gap-2">
                <span className="font-mono text-sm font-semibold">{currentVersion}</span>
                <Badge variant="success">current</Badge>
              </p>
              {current !== undefined ? (
                <p className="mt-0.5 text-xs text-muted-foreground">
                  Published <RelativeTime iso={current.published_at} />
                </p>
              ) : null}
            </div>
          </li>
        </ul>
      </Card>
      {newer.length === 0 ? (
        <p className="text-sm text-muted-foreground">No newer version published.</p>
      ) : null}
      <UpdateDialog
        siteId={siteId}
        appId={appId}
        currentVersion={currentVersion}
        target={target}
        onClose={() => setTarget(null)}
        onJob={onJob}
      />
    </div>
  );
}

function UpdateOutcome({ job }: { job: Job }) {
  const error = job.error ?? "";
  let variant: "info" | "success" | "warning" | "destructive" = "warning";
  let text: string;
  if (job.status === "queued" || job.status === "running") {
    variant = "info";
    text = "An update is in progress.";
  } else if (job.status === "done") {
    variant = "success";
    text = "The last update finished.";
  } else if (error.startsWith("rollback failed:")) {
    variant = "destructive";
    text = `The last update failed and so did its rollback: ${error.slice("rollback failed:".length).trim()}. The site is failed; retry it from the Danger tab or restore a backup.`;
  } else if (error.startsWith("rollback:")) {
    text = `The last update was rolled back: ${error.slice("rollback:".length).trim()}.`;
  } else {
    variant = "destructive";
    text = `The last update failed${error === "" ? "" : `: ${error}`}.`;
  }
  return (
    <Alert variant={variant} role="status" data-testid="update-outcome">
      <p>{text}</p>
      <AppLink href={`/jobs/${job.id}`} className="mt-1 inline-block text-xs">
        Open the update job
      </AppLink>
    </Alert>
  );
}

function UpdateDialog({
  siteId,
  appId,
  currentVersion,
  target,
  onClose,
  onJob,
}: {
  siteId: string;
  appId: string;
  currentVersion: string;
  target: AppVersion | null;
  onClose: () => void;
  onJob?: (jobId: string) => void;
}) {
  const update = useUpdateSite(siteId);
  const version = target?.version ?? "";
  const prompts = useVersionPrompts(appId, target?.version ?? null);
  const [answers, setAnswers] = React.useState<Record<string, string>>({});
  const reset = update.reset;

  React.useEffect(() => {
    setAnswers({});
    reset();
  }, [target, reset]);

  function submit() {
    const values = Object.fromEntries(Object.entries(answers).filter(([, v]) => v !== ""));
    update.mutate(
      { version, prompts: values },
      {
        onSuccess: (res) => {
          toast.success(`Updating to ${version}`);
          onClose();
          onJob?.(res.job.id);
        },
      },
    );
  }

  const error = update.isError
    ? update.error instanceof ApiError
      ? update.error.message
      : "Update failed"
    : null;

  return (
    <Modal
      open={target !== null}
      onClose={onClose}
      title={`Update to ${version}?`}
      description={`From ${currentVersion}. A pre-update backup is taken first; if ${version} fails its health check the site rolls back to ${currentVersion} automatically.`}
      footer={
        <>
          <Button variant="outline" onClick={onClose}>
            Cancel
          </Button>
          <Button data-testid="confirm-accept" loading={update.isPending} onClick={submit}>
            {update.isPending ? "Starting…" : `Update to ${version}`}
          </Button>
        </>
      }
    >
      <div className="flex flex-col gap-3">
        {prompts.isPending ? <SkeletonRows rows={1} label="Loading prompts…" /> : null}
        <ErrorText error={prompts.error} />
        {prompts.data !== undefined && prompts.data.length === 0 ? (
          <p className="text-sm text-muted-foreground">{version} asks for nothing new.</p>
        ) : null}
        {(prompts.data ?? []).map((p) => (
          <Field
            key={p.name}
            label={p.label}
            hint={
              <>
                Sets <code>{p.name}</code>. Leave empty to keep the current answer.
              </>
            }
          >
            <Input
              type={p.secret ? "password" : "text"}
              autoComplete="off"
              spellCheck={false}
              value={answers[p.name] ?? ""}
              onChange={(e) => setAnswers((a) => ({ ...a, [p.name]: e.target.value }))}
            />
          </Field>
        ))}
        {error !== null ? <Alert variant="destructive">{error}</Alert> : null}
      </div>
    </Modal>
  );
}
