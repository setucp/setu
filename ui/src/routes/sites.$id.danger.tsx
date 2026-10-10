import { SkeletonRows } from "@/components/ui/skeleton";
import { ErrorText } from "@/components/ErrorText";
import { createFileRoute, useNavigate } from "@tanstack/react-router";
import { SiteDanger } from "@/components/SiteDanger";
import { useSite } from "@/lib/api";
import { ApiError } from "@/lib/client";

export const Route = createFileRoute("/sites/$id/danger")({
  component: SiteDangerTab,
});

function SiteDangerTab() {
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
    <SiteDanger
      site={site.data}
      onJob={(jobId) => {
        void navigate({ to: "/jobs/$id", params: { id: jobId } });
      }}
      onDeleted={() => {
        void navigate({ to: "/sites" });
      }}
    />
  );
}
