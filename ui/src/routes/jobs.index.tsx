import { createFileRoute } from "@tanstack/react-router";
import { ErrorText } from "@/components/ErrorText";
import { JobsTable } from "@/components/JobsTable";
import { PageHeader } from "@/components/PageHeader";
import { SkeletonRows } from "@/components/ui/skeleton";
import { useJobs } from "@/lib/api";

export const Route = createFileRoute("/jobs/")({
  component: JobsPage,
});

function JobsPage() {
  const jobs = useJobs();

  return (
    <div className="flex flex-col gap-6">
      <PageHeader
        title="Jobs"
        description="Everything the panel does to a site or server runs as a job. Refreshes every five seconds."
      />
      {jobs.isPending ? <SkeletonRows rows={6} label="Loading jobs…" /> : null}
      <ErrorText error={jobs.error} />
      {jobs.data !== undefined ? <JobsTable jobs={jobs.data} /> : null}
    </div>
  );
}
