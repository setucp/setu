import { createFileRoute } from "@tanstack/react-router";
import { SiteLogs } from "@/components/SiteLogs";

export const Route = createFileRoute("/sites/$id/logs")({
  component: SiteLogsTab,
});

function SiteLogsTab() {
  const { id } = Route.useParams();
  return <SiteLogs siteId={id} />;
}
