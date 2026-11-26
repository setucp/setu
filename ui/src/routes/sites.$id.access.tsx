import { createFileRoute, useNavigate } from "@tanstack/react-router";
import { ErrorText } from "@/components/ErrorText";
import { SiteAccess } from "@/components/SiteAccess";
import { SiteAliases } from "@/components/SiteAliases";
import { SiteNoindex } from "@/components/SiteNoindex";
import { SkeletonRows } from "@/components/ui/skeleton";
import { useSite } from "@/lib/api";
import { ApiError } from "@/lib/client";

export const Route = createFileRoute("/sites/$id/access")({
  component: SiteAccessTab,
});

function SiteAccessTab() {
  const { id } = Route.useParams();
  const site = useSite(id);
  const navigate = useNavigate({ from: Route.fullPath });

  if (site.error instanceof ApiError) return <ErrorText error={site.error} />;
  if (site.data === undefined) return <SkeletonRows rows={3} />;
  const toJob = (jobId: string) => void navigate({ to: "/jobs/$id", params: { id: jobId } });
  return (
    <div className="flex flex-col gap-4">
      <SiteAccess site={site.data} onSubmitted={toJob} />
      <SiteAliases site={site.data} onSubmitted={toJob} />
      <SiteNoindex site={site.data} onSubmitted={toJob} />
    </div>
  );
}
