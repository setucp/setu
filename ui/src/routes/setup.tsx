import { createFileRoute } from "@tanstack/react-router";
import { SetupWizard } from "@/components/SetupWizard";

export const Route = createFileRoute("/setup")({
  component: SetupPage,
});

/** The wizard's last step links to the sites list itself. */
function SetupPage() {
  return <SetupWizard />;
}
