import { SkeletonRows } from "@/components/ui/skeleton";
import { createFileRoute, useNavigate } from "@tanstack/react-router";
import { SiteBackups } from "@/components/SiteBackups";
import { useSite } from "@/lib/api";

export const Route = createFileRoute("/sites/$id/backups")({
  component: SiteBackupsTab,
});

function SiteBackupsTab() {
  const { id } = Route.useParams();
  const navigate = useNavigate({ from: Route.fullPath });
  const site = useSite(id);
  if (site.data === undefined) {
    return <SkeletonRows rows={3} />;
  }
  return (
    <SiteBackups
      siteId={id}
      siteName={site.data.name}
      onJob={(jobId) => {
        void navigate({ to: "/jobs/$id", params: { id: jobId } });
      }}
    />
  );
}
