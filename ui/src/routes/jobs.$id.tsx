import { Link, createFileRoute } from "@tanstack/react-router";
import { JobLogView } from "@/components/JobLogView";
import { useJob } from "@/lib/api";

export const Route = createFileRoute("/jobs/$id")({
  component: JobPage,
});

/** Full job view: status, error, stored log plus a live SSE tail while open. */
function JobPage() {
  const { id } = Route.useParams();
  const job = useJob(id);
  const siteId = job.data?.site_id;

  return (
    <div className="flex flex-col gap-4">
      <JobLogView jobId={id} />
      {siteId !== null && siteId !== undefined ? (
        <Link
          to="/sites/$id"
          params={{ id: siteId }}
          className="text-sm text-muted-foreground hover:text-foreground"
        >
          ← Back to site
        </Link>
      ) : (
        <Link to="/jobs" className="text-sm text-muted-foreground hover:text-foreground">
          ← All jobs
        </Link>
      )}
    </div>
  );
}
