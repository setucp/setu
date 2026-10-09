import * as React from "react";
import { createFileRoute } from "@tanstack/react-router";
import { AppsCatalogue } from "@/components/AppsCatalogue";
import { PublishAppDialog } from "@/components/PublishAppDialog";
import { Button } from "@/components/ui/button";

export const Route = createFileRoute("/apps/")({
  component: AppsPage,
});

function AppsPage() {
  const [adding, setAdding] = React.useState(false);
  return (
    <div className="flex flex-col gap-4">
      <div className="flex items-center">
        <h1 className="text-xl font-semibold tracking-tight">Apps</h1>
        <Button type="button" className="ml-auto" onClick={() => setAdding(true)}>
          Add app
        </Button>
      </div>
      <AppsCatalogue />
      <PublishAppDialog open={adding} onClose={() => setAdding(false)} />
    </div>
  );
}
