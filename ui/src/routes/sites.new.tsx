import { createFileRoute, useNavigate } from "@tanstack/react-router";
import { toast } from "sonner";
import { NewSiteWizard } from "@/components/NewSiteWizard";

export const Route = createFileRoute("/sites/new")({
  component: NewSitePage,
});

function NewSitePage() {
  const navigate = useNavigate({ from: Route.fullPath });
  return (
    <NewSiteWizard
      onCreated={(jobId) => {
        toast.success("Site provisioning started");
        void navigate({ to: "/jobs/$id", params: { id: jobId } });
      }}
    />
  );
}
