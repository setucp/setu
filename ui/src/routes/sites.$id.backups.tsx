import { createFileRoute, useNavigate } from "@tanstack/react-router";
import { SiteBackups } from "@/components/SiteBackups";

export const Route = createFileRoute("/sites/$id/backups")({
  component: SiteBackupsTab,
});

function SiteBackupsTab() {
  const { id } = Route.useParams();
  const navigate = useNavigate({ from: Route.fullPath });
  return (
    <SiteBackups
      siteId={id}
      onJob={(jobId) => {
        void navigate({ to: "/jobs/$id", params: { id: jobId } });
      }}
    />
  );
}
