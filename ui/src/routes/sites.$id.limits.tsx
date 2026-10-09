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
  return <SiteLimits site={site.data} />;
}
