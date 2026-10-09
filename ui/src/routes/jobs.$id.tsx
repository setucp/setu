import { Link, createFileRoute } from "@tanstack/react-router";
import { Badge } from "@/components/ui/badge";
import { useJob } from "@/lib/api";
import { ApiError } from "@/lib/client";

export const Route = createFileRoute("/jobs/$id")({
  component: JobPage,
});

const variantByStatus: Record<string, "success" | "warning" | "destructive" | "secondary"> = {
  done: "success",
  queued: "warning",
  running: "warning",
  failed: "destructive",
};

/**
 * Minimal job view for Task 12 (the new-site flow lands here): status plus
 * a polling log tail. The full jobs screen is Task 13.
 */
function JobPage() {
  const { id } = Route.useParams();
  const job = useJob(id);

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
      <pre className="max-h-[60dvh] overflow-auto rounded-md border bg-muted/40 p-4 font-mono text-xs whitespace-pre-wrap">
        {j.log === "" ? "Waiting for output…" : j.log}
      </pre>
      {j.site_id !== null && j.site_id !== undefined ? (
        <Link
          to="/sites/$id"
          params={{ id: j.site_id }}
          className="text-sm text-muted-foreground hover:text-foreground"
        >
          ← Back to site
        </Link>
      ) : null}
    </div>
  );
}
