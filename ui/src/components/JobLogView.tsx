import * as React from "react";
import { useJob } from "@/lib/api";
import { ApiError } from "@/lib/client";
import { useSseLines } from "@/lib/sse";
import { Badge } from "./ui/badge";

const variantByStatus: Record<string, "success" | "warning" | "destructive" | "secondary"> = {
  done: "success",
  queued: "warning",
  running: "warning",
  failed: "destructive",
};

/**
 * Full job view: status, error, the stored log plus a live SSE tail while the
 * job is still open. Finished jobs render the stored log only.
 */
export function JobLogView({ jobId }: { jobId: string }) {
  const job = useJob(jobId);
  const open =
    job.data !== undefined && (job.data.status === "queued" || job.data.status === "running");
  const live = useSseLines(
    open ? `/api/v1/jobs/${encodeURIComponent(jobId)}/log` : null,
  );
  const scrollRef = React.useRef<HTMLPreElement>(null);
  const stored = job.data?.log ?? "";
  const text = [stored, ...live].filter((part) => part !== "").join("\n");

  React.useEffect(() => {
    const el = scrollRef.current;
    if (el !== null) {
      el.scrollTop = el.scrollHeight;
    }
  }, [text]);

  if (job.isPending) {
    return <p className="text-sm text-muted-foreground">Loading…</p>;
  }
  if (job.error instanceof ApiError) {
    return (
      <p role="alert" className="text-sm text-destructive">
        {job.error.message}
      </p>
    );
  }
  const j = job.data;
  if (j === undefined) {
    return <p className="text-sm text-muted-foreground">Loading…</p>;
  }

  return (
    <div className="flex flex-col gap-4">
      <div className="flex items-center gap-3">
        <h1 className="text-xl font-semibold tracking-tight">Job {j.kind}</h1>
        <Badge variant={variantByStatus[j.status] ?? "secondary"}>{j.status}</Badge>
      </div>
      {j.error !== null && j.error !== undefined ? (
        <p role="alert" className="text-sm text-destructive">
          {j.error}
        </p>
      ) : null}
      <dl className="grid max-w-xl grid-cols-[140px_1fr] gap-x-4 gap-y-2 text-sm">
        <dt className="text-muted-foreground">ID</dt>
        <dd className="font-mono">{j.id}</dd>
        <dt className="text-muted-foreground">Attempts</dt>
        <dd>{j.attempts}</dd>
        {j.site_id !== null && j.site_id !== undefined ? (
          <>
            <dt className="text-muted-foreground">Site</dt>
            <dd>
              <a
                href={`/sites/${j.site_id}`}
                className="font-mono text-primary underline-offset-2 hover:underline"
              >
                {j.site_id}
              </a>
            </dd>
          </>
        ) : null}
      </dl>
      <pre
        ref={scrollRef}
        className="max-h-[60dvh] overflow-auto rounded-md border bg-muted/40 p-4 font-mono text-xs whitespace-pre-wrap"
      >
        {text === "" ? "Waiting for output…" : text}
      </pre>
    </div>
  );
}
