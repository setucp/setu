import { createFileRoute } from "@tanstack/react-router";
import { JobsTable } from "@/components/JobsTable";
import { useJobs } from "@/lib/api";
import { ApiError } from "@/lib/client";

export const Route = createFileRoute("/jobs/")({
  component: JobsPage,
});

function JobsPage() {
  const jobs = useJobs();

  return (
    <div className="flex flex-col gap-4">
      <h1 className="text-xl font-semibold tracking-tight">Jobs</h1>
      {jobs.isPending ? (
        <p className="text-sm text-muted-foreground">Loading…</p>
      ) : null}
      {jobs.error instanceof ApiError ? (
        <p role="alert" className="text-sm text-destructive">
          {jobs.error.message}
        </p>
      ) : null}
      {jobs.data !== undefined ? <JobsTable jobs={jobs.data} /> : null}
    </div>
  );
}
