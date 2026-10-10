import * as React from "react";
import { createFileRoute } from "@tanstack/react-router";
import { Plus } from "lucide-react";
import { AppsCatalogue } from "@/components/AppsCatalogue";
import { PageHeader } from "@/components/PageHeader";
import { PublishAppDialog } from "@/components/PublishAppDialog";
import { Button } from "@/components/ui/button";

export const Route = createFileRoute("/apps/")({
  component: AppsPage,
});

function AppsPage() {
  const [adding, setAdding] = React.useState(false);
  return (
    <div className="flex flex-col gap-6">
      <PageHeader
        title="Apps"
        description="The catalogue new sites are created from. Publish a manifest to add an app or a version."
        actions={
          <Button type="button" onClick={() => setAdding(true)}>
            <Plus aria-hidden="true" />
            Add app
          </Button>
        }
      />
      <AppsCatalogue onAdd={() => setAdding(true)} />
      <PublishAppDialog open={adding} onClose={() => setAdding(false)} />
    </div>
  );
}
