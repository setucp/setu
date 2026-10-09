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
    return (
      <p role="alert" className="text-sm text-destructive">
        {site.error.message}
      </p>
    );
  }
  if (site.data === undefined) {
    return <p className="text-sm text-muted-foreground">Loading…</p>;
  }
  return <SiteOverview site={site.data} />;
}
