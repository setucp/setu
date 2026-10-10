import * as React from "react";
import { createFileRoute } from "@tanstack/react-router";
import { Plus } from "lucide-react";
import { AddServerDialog } from "@/components/AddServerDialog";
import { ErrorText } from "@/components/ErrorText";
import { PageHeader } from "@/components/PageHeader";
import { ServersTable } from "@/components/ServersTable";
import { Button } from "@/components/ui/button";
import { SkeletonRows } from "@/components/ui/skeleton";
import { usePlans, useServers, useSites } from "@/lib/api";

export const Route = createFileRoute("/servers/")({
  component: ServersPage,
});

function ServersPage() {
  const servers = useServers();
  const sites = useSites();
  const plans = usePlans();
  const [adding, setAdding] = React.useState(false);
  const error = servers.error ?? sites.error ?? plans.error;
  const online = (servers.data ?? []).filter((s) => s.status === "online").length;

  return (
    <div className="flex flex-col gap-6">
      <PageHeader
        title="Servers"
        description={
          servers.data === undefined
            ? "Machines whose agents run your sites."
            : `${servers.data.length} ${servers.data.length === 1 ? "server" : "servers"}, ${online} online`
        }
        actions={
          <Button type="button" onClick={() => setAdding(true)}>
            <Plus aria-hidden="true" />
            Add server
          </Button>
        }
      />
      {servers.isPending || sites.isPending || plans.isPending ? (
        <SkeletonRows label="Loading servers…" />
      ) : null}
      <ErrorText error={error} />
      {servers.data !== undefined && sites.data !== undefined && plans.data !== undefined ? (
        <ServersTable servers={servers.data} sites={sites.data} plans={plans.data} />
      ) : null}
      <AddServerDialog open={adding} onClose={() => setAdding(false)} />
    </div>
  );
}
