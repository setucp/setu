import { createFileRoute, useNavigate } from "@tanstack/react-router";
import { SetupWizard } from "@/components/SetupWizard";

export const Route = createFileRoute("/setup")({
  component: SetupPage,
});

function SetupPage() {
  const navigate = useNavigate({ from: Route.fullPath });
  return (
    <SetupWizard
      onDone={() => {
        void navigate({ to: "/sites" });
      }}
    />
  );
}
