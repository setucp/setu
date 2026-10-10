import { SkeletonRows } from "@/components/ui/skeleton";
import { ErrorText } from "@/components/ErrorText";
import { createFileRoute } from "@tanstack/react-router";
import { SiteLimits } from "@/components/SiteLimits";
import { useSite } from "@/lib/api";
import { ApiError } from "@/lib/client";

export const Route = createFileRoute("/sites/$id/limits")({
  component: SiteLimitsTab,
});

function SiteLimitsTab() {
  const { id } = Route.useParams();
  const site = useSite(id);

  if (site.isPending) {
    return <SkeletonRows rows={3} />;
  }
  if (site.error instanceof ApiError) {
    return <ErrorText error={site.error} />;
  }
  if (site.data === undefined) {
    return <SkeletonRows rows={3} />;
  }
  return <SiteLimits site={site.data} />;
}
