import { CheckCircle2, CircleDashed, MinusCircle, XCircle } from "lucide-react";
import { useJob } from "@/lib/api";
import { ApiError } from "@/lib/client";
import { useSseLines } from "@/lib/sse";
import { duration } from "@/lib/time";
import { cn } from "@/lib/utils";
import { AppLink } from "./AppLink";
import { ErrorText } from "./ErrorText";
import { LogPanel } from "./LogPanel";
import { PageHeader } from "./PageHeader";
import { RelativeTime } from "./RelativeTime";
import { StatusBadge, jobVariant } from "./StatusBadge";
import { Alert } from "./ui/alert";
import { Card, CardContent, CardHeader, CardTitle } from "./ui/card";
import { Skeleton } from "./ui/skeleton";

export interface JobStep {
  name: string;
  outcome: "ok" | "error" | "ignored";
  detail: string;
}

const STEP_RE = /^step (.+?): (ok|error|ignored)\b ?(.*)$/;

/** The job's steps from its log (`step <name>: ok|error <msg>|ignored (…)`), latest outcome per name. */
export function parseSteps(text: string): JobStep[] {
  const steps: JobStep[] = [];
  const index = new Map<string, number>();
  for (const line of text.split("\n")) {
    const m = STEP_RE.exec(line.trim());
    if (m === null) continue;
    const step: JobStep = {
      name: m[1] ?? "",
      outcome: (m[2] ?? "ok") as JobStep["outcome"],
      detail: m[3] ?? "",
    };
    const at = index.get(step.name);
    if (at === undefined) {
      index.set(step.name, steps.length);
      steps.push(step);
    } else {
      steps[at] = step;
    }
  }
  return steps;
}

function StepList({ steps, running }: { steps: JobStep[]; running: boolean }) {
  return (
    <ol aria-label="Steps" className="flex flex-col">
      {steps.map((s) => (
        <li key={s.name} className="flex items-start gap-3 py-2 text-sm">
          {s.outcome === "ok" ? (
            <CheckCircle2 className="mt-0.5 size-4 shrink-0 text-success" aria-hidden="true" />
          ) : s.outcome === "error" ? (
            <XCircle className="mt-0.5 size-4 shrink-0 text-destructive" aria-hidden="true" />
          ) : (
            <MinusCircle className="mt-0.5 size-4 shrink-0 text-muted-foreground" aria-hidden="true" />
          )}
          <div className="min-w-0 flex-1">
            <p className="font-mono text-xs font-medium">{s.name}</p>
            {s.detail !== "" ? (
              <p
                className={cn(
                  "mt-0.5 break-words text-xs",
                  s.outcome === "error" ? "text-destructive" : "text-muted-foreground",
                )}
              >
                {s.detail}
              </p>
            ) : null}
          </div>
          <span className="sr-only">{s.outcome}</span>
        </li>
      ))}
      {running ? (
        <li className="flex items-center gap-3 py-2 text-sm text-muted-foreground">
          <CircleDashed className="size-4 shrink-0 motion-safe:animate-spin" aria-hidden="true" />
          <span className="text-xs">Working…</span>
        </li>
      ) : null}
    </ol>
  );
}

/**
 * Full job view: status, error, the steps parsed from the log, and the
 * log itself. While the job is open the log comes from the SSE stream,
 * which replays the stored backlog and then tails it (reconnects are
 * merged without duplicates, see `LineMerger`); until its first line
 * arrives the stored log shows. Finished jobs render the stored log only
 * and never subscribe.
 */
export function JobLogView({ jobId }: { jobId: string }) {
  const job = useJob(jobId);
  const open =
    job.data !== undefined && (job.data.status === "queued" || job.data.status === "running");
  const live = useSseLines(open ? `/api/v1/jobs/${encodeURIComponent(jobId)}/log` : null);
  const stored = (job.data?.log ?? "").replace(/\n$/, "");
  const text = open && live.length > 0 ? live.join("\n") : stored;

  if (job.isPending) {
    return (
      <div role="status" className="flex flex-col gap-6">
        <span className="sr-only">Loading job…</span>
        <Skeleton className="h-8 w-64" />
        <Skeleton className="h-64 w-full" />
      </div>
    );
  }
  if (job.error instanceof ApiError) return <ErrorText error={job.error} />;
  const j = job.data;
  if (j === undefined) return null;
  const steps = parseSteps(text);

  return (
    <div className="flex flex-col gap-6">
      <PageHeader
        title={<span className="font-mono text-lg sm:text-xl">{j.kind}</span>}
        meta={<StatusBadge status={j.status} variants={jobVariant} />}
        description={
          <span className="font-mono text-xs">
            {j.id}
          </span>
        }
      />
      {j.error !== null && j.error !== undefined ? (
        <Alert variant="destructive" title="The job failed">
          {j.error}
        </Alert>
      ) : null}
      <div className="grid items-start gap-6 lg:grid-cols-[280px_minmax(0,1fr)]">
        <div className="flex flex-col gap-6">
          <Card>
            <CardContent className="p-4 sm:p-5">
              <dl className="grid grid-cols-[96px_minmax(0,1fr)] gap-x-3 gap-y-2.5 text-sm">
                <dt className="text-muted-foreground">Attempts</dt>
                <dd className="tabular-nums">{j.attempts}</dd>
                <dt className="text-muted-foreground">Created</dt>
                <dd>
                  <RelativeTime iso={j.created_at} />
                </dd>
                <dt className="text-muted-foreground">Duration</dt>
                <dd className="tabular-nums">{duration(j.started_at, j.finished_at)}</dd>
                {j.site_id !== null && j.site_id !== undefined ? (
                  <>
                    <dt className="text-muted-foreground">Site</dt>
                    <dd className="min-w-0 truncate">
                      <AppLink
                        href={`/sites/${j.site_id}`}
                        className="font-mono text-xs text-link hover:underline"
                      >
                        {j.site_id}
                      </AppLink>
                    </dd>
                  </>
                ) : null}
              </dl>
            </CardContent>
          </Card>
          <Card>
            <CardHeader className="pb-0 sm:pb-0">
              <CardTitle as="h2">Steps</CardTitle>
            </CardHeader>
            <CardContent className="pt-2 sm:pt-2">
              {steps.length === 0 && !open ? (
                <p className="text-sm text-muted-foreground">No steps recorded.</p>
              ) : (
                <StepList steps={steps} running={open} />
              )}
            </CardContent>
          </Card>
        </div>
        <LogPanel title="Job log" testId="job-log" text={text} live={open} />
      </div>
    </div>
  );
}
