import { createFileRoute } from "@tanstack/react-router";
import { ArrowLeft } from "lucide-react";
import { AppLink } from "@/components/AppLink";
import { JobLogView } from "@/components/JobLogView";
import { useJob } from "@/lib/api";

export const Route = createFileRoute("/jobs/$id")({
  component: JobPage,
});

/** Full job view: status, steps, error, stored log plus a live SSE tail while open. */
function JobPage() {
  const { id } = Route.useParams();
  const job = useJob(id);
  const siteId = job.data?.site_id;

  return (
    <div className="flex flex-col gap-4">
      <AppLink
        href={siteId !== null && siteId !== undefined ? `/sites/${siteId}` : "/jobs"}
        className="inline-flex items-center gap-1.5 self-start text-sm text-muted-foreground hover:text-foreground"
      >
        <ArrowLeft className="size-4" aria-hidden="true" />
        {siteId !== null && siteId !== undefined ? "Back to site" : "All jobs"}
      </AppLink>
      <JobLogView jobId={id} />
    </div>
  );
}
