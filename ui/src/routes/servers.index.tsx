import * as React from "react";
import { createFileRoute } from "@tanstack/react-router";
import { AddServerDialog } from "@/components/AddServerDialog";
import { ServersTable } from "@/components/ServersTable";
import { Button } from "@/components/ui/button";
import { usePlans, useServers, useSites } from "@/lib/api";
import { ApiError } from "@/lib/client";

export const Route = createFileRoute("/servers/")({
  component: ServersPage,
});

function ServersPage() {
  const servers = useServers();
  const sites = useSites();
  const plans = usePlans();
  const [adding, setAdding] = React.useState(false);
  const error = servers.error ?? sites.error ?? plans.error;

  return (
    <div className="flex flex-col gap-4">
      <div className="flex items-center">
        <h1 className="text-xl font-semibold tracking-tight">Servers</h1>
        <Button type="button" className="ml-auto" onClick={() => setAdding(true)}>
          Add server
        </Button>
      </div>
      {servers.isPending || sites.isPending || plans.isPending ? (
        <p className="text-sm text-muted-foreground">Loading…</p>
      ) : null}
      {error instanceof ApiError ? (
        <p role="alert" className="text-sm text-destructive">
          {error.message}
        </p>
      ) : null}
      {servers.data !== undefined &&
      sites.data !== undefined &&
      plans.data !== undefined ? (
        <ServersTable servers={servers.data} sites={sites.data} plans={plans.data} />
      ) : null}
      <AddServerDialog open={adding} onClose={() => setAdding(false)} />
    </div>
  );
}
