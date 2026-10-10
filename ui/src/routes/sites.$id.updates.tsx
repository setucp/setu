import { SkeletonRows } from "@/components/ui/skeleton";
import { ErrorText } from "@/components/ErrorText";
import { createFileRoute, useNavigate } from "@tanstack/react-router";
import { SiteUpdates } from "@/components/SiteUpdates";
import { useSite } from "@/lib/api";
import { ApiError } from "@/lib/client";

export const Route = createFileRoute("/sites/$id/updates")({
  component: SiteUpdatesTab,
});

function SiteUpdatesTab() {
  const { id } = Route.useParams();
  const navigate = useNavigate({ from: Route.fullPath });
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
  return (
    <SiteUpdates
      siteId={id}
      appId={site.data.app_id}
      currentVersion={site.data.app_version}
      onJob={(jobId) => {
        void navigate({ to: "/jobs/$id", params: { id: jobId } });
      }}
    />
  );
}
