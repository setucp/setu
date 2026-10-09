import * as React from "react";
import { toast } from "sonner";
import { useAppVersions, useJobs, useUpdateSite } from "@/lib/api";
import { ApiError, type AppVersion, type Job } from "@/lib/client";
import { compareVersions, parseAssignments } from "@/lib/format";
import { Button } from "./ui/button";
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
 * Updates tab: versions newer than the running one, an update dialog that
 * collects answers for `prompt` variables the target version adds
 * (`NAME=value` lines; the API exposes no per-version manifest, so the
 * server validates the names, R110), and the outcome of the site's latest
 * update job — including an automatic rollback, read from the job error
 * (`rollback: …` / `rollback failed: …`).
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
  const newer = (versions.data ?? [])
    .filter((v) => compareVersions(v.version, currentVersion) > 0)
    .sort((a, b) => compareVersions(b.version, a.version));
  const latest = (jobs.data ?? [])
    .filter((j) => j.kind === "update_site" && j.site_id === siteId)
    .sort((a, b) => b.created_at.localeCompare(a.created_at))[0];

  return (
    <div className="flex flex-col gap-4">
      <p className="text-sm">{`Running ${currentVersion}.`}</p>
      {latest !== undefined ? <UpdateOutcome job={latest} /> : null}
      {newer.length === 0 ? (
        <p className="text-sm text-muted-foreground">No newer version published.</p>
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
            {newer.map((v) => (
              <TableRow key={v.id}>
                <TableCell className="font-mono">{v.version}</TableCell>
                <TableCell>{v.channel}</TableCell>
                <TableCell>{new Date(v.published_at).toLocaleDateString()}</TableCell>
                <TableCell>
                  <Button type="button" variant="outline" size="sm" onClick={() => setTarget(v)}>
                    Update to {v.version}
                  </Button>
                </TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      )}
      <UpdateDialog
        siteId={siteId}
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
  let tone = "border-yellow-500/40 bg-yellow-500/10";
  let text: string;
  if (job.status === "queued" || job.status === "running") {
    text = "An update is in progress.";
  } else if (job.status === "done") {
    tone = "border-green-500/40 bg-green-500/10";
    text = "The last update finished.";
  } else if (error.startsWith("rollback failed:")) {
    tone = "border-destructive/50 bg-destructive/10";
    text = `The last update failed and so did its rollback: ${error.slice("rollback failed:".length).trim()}. The site is failed; retry it from the Danger tab or restore a backup.`;
  } else if (error.startsWith("rollback:")) {
    text = `The last update was rolled back: ${error.slice("rollback:".length).trim()}.`;
  } else {
    tone = "border-destructive/50 bg-destructive/10";
    text = `The last update failed${error === "" ? "" : `: ${error}`}.`;
  }
  return (
    <div data-testid="update-outcome" className={`rounded-md border p-3 text-sm ${tone}`}>
      <p>{text}</p>
      <a
        href={`/jobs/${job.id}`}
        className="mt-1 inline-block text-xs text-primary underline-offset-2 hover:underline"
      >
        Open the update job
      </a>
    </div>
  );
}

function UpdateDialog({
  siteId,
  currentVersion,
  target,
  onClose,
  onJob,
}: {
  siteId: string;
  currentVersion: string;
  target: AppVersion | null;
  onClose: () => void;
  onJob?: (jobId: string) => void;
}) {
  const update = useUpdateSite(siteId);
  const [answers, setAnswers] = React.useState("");
  const [localError, setLocalError] = React.useState<string | null>(null);
  const reset = update.reset;

  React.useEffect(() => {
    setAnswers("");
    setLocalError(null);
    reset();
  }, [target, reset]);

  const version = target?.version ?? "";
  const promptsId = React.useId();

  function submit() {
    const { values, badLines } = parseAssignments(answers);
    if (badLines.length > 0) {
      setLocalError(
        `Expected NAME=value on line ${badLines.join(", ")} (names are upper case, digits and _).`,
      );
      return;
    }
    setLocalError(null);
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

  const error =
    localError ??
    (update.isError
      ? update.error instanceof ApiError
        ? update.error.message
        : "update failed"
      : null);

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
          <Button data-testid="confirm-accept" disabled={update.isPending} onClick={submit}>
            {update.isPending ? "Starting…" : `Update to ${version}`}
          </Button>
        </>
      }
    >
      <div className="flex flex-col gap-2">
        <label htmlFor={promptsId} className="text-sm font-medium">
          New prompt answers
        </label>
        <p className="text-xs text-muted-foreground">
          If {version} adds <code>prompt</code> variables, answer them here, one{" "}
          <code>NAME=value</code> per line. Existing answers are kept; leave this empty
          when nothing new is asked.
        </p>
        <textarea
          id={promptsId}
          value={answers}
          onChange={(e) => setAnswers(e.target.value)}
          rows={4}
          spellCheck={false}
          className="w-full rounded-md border border-input bg-background px-3 py-2 font-mono text-xs focus-visible:outline-hidden focus-visible:ring-1 focus-visible:ring-ring"
          placeholder="SMTP_HOST=mail.example.com"
        />
        {error !== null ? (
          <p role="alert" className="text-sm text-destructive">
            {error}
          </p>
        ) : null}
      </div>
    </Modal>
  );
}
