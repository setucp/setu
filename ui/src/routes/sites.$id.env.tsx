import { createFileRoute, useNavigate } from "@tanstack/react-router";
import { toast } from "sonner";
import { SiteEnv } from "@/components/SiteEnv";

export const Route = createFileRoute("/sites/$id/env")({
  component: SiteEnvTab,
});

function SiteEnvTab() {
  const { id } = Route.useParams();
  const navigate = useNavigate({ from: Route.fullPath });
  return (
    <SiteEnv
      siteId={id}
      onSubmitted={(jobId) => {
        toast.success("Restarting with the new environment");
        void navigate({ to: "/jobs/$id", params: { id: jobId } });
      }}
    />
  );
}
