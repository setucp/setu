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
    return <p className="text-sm text-muted-foreground">Loading…</p>;
  }
  if (site.error instanceof ApiError) {
    return (
      <p role="alert" className="text-sm text-destructive">
        {site.error.message}
      </p>
    );
  }
  if (site.data === undefined) {
    return <p className="text-sm text-muted-foreground">Loading…</p>;
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
