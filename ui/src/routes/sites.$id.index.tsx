import { SkeletonRows } from "@/components/ui/skeleton";
import { ErrorText } from "@/components/ErrorText";
import { createFileRoute } from "@tanstack/react-router";
import { SiteOverview } from "@/components/SiteOverview";
import { useSite } from "@/lib/api";
import { ApiError } from "@/lib/client";

export const Route = createFileRoute("/sites/$id/")({
  component: SiteOverviewTab,
});

function SiteOverviewTab() {
  const { id } = Route.useParams();
  const site = useSite(id);

  if (site.error instanceof ApiError) {
    return <ErrorText error={site.error} />;
  }
  if (site.data === undefined) {
    return <SkeletonRows rows={3} />;
  }
  return <SiteOverview site={site.data} />;
}
