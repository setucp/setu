import { Link, createFileRoute } from "@tanstack/react-router";
import { SitesTable } from "@/components/SitesTable";
import { usePlans, useServers, useSites } from "@/lib/api";
import { ApiError } from "@/lib/client";
import { buttonVariants } from "@/components/ui/button";

export const Route = createFileRoute("/sites/")({
  component: SitesPage,
});

function SitesPage() {
  const sites = useSites();
  const plans = usePlans();
  const servers = useServers();
  const error = sites.error ?? plans.error ?? servers.error;

  return (
    <div className="flex flex-col gap-4">
      <div className="flex items-center">
        <h1 className="text-xl font-semibold tracking-tight">Sites</h1>
        <Link to="/sites/new" className={buttonVariants({ className: "ml-auto" })}>
          New site
        </Link>
      </div>
      {sites.isPending || plans.isPending || servers.isPending ? (
        <p className="text-sm text-muted-foreground">Loading…</p>
      ) : null}
      {error instanceof ApiError ? (
        <p role="alert" className="text-sm text-destructive">
          {error.message}
        </p>
      ) : null}
      {sites.data !== undefined && plans.data !== undefined && servers.data !== undefined ? (
        <SitesTable sites={sites.data} plans={plans.data} servers={servers.data} />
      ) : null}
    </div>
  );
}
