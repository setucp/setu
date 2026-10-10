import * as React from "react";
import { Link, createFileRoute } from "@tanstack/react-router";
import { Plus, Search, X } from "lucide-react";
import { SitesTable } from "@/components/SitesTable";
import { PageHeader } from "@/components/PageHeader";
import { ErrorText } from "@/components/ErrorText";
import { EmptyState } from "@/components/EmptyState";
import { SkeletonRows } from "@/components/ui/skeleton";
import { Input } from "@/components/ui/input";
import { buttonVariants } from "@/components/ui/button";
import { usePlans, useServers, useSites } from "@/lib/api";
import type { Site } from "@/lib/client";

export const Route = createFileRoute("/sites/")({
  component: SitesPage,
});

function matches(site: Site, q: string): boolean {
  const needle = q.trim().toLowerCase();
  if (needle === "") return true;
  return [site.name, site.domain, site.app_id, site.status].some((v) =>
    v.toLowerCase().includes(needle),
  );
}

function SitesPage() {
  const sites = useSites();
  const plans = usePlans();
  const servers = useServers();
  const [query, setQuery] = React.useState("");
  const error = sites.error ?? plans.error ?? servers.error;
  const loading = sites.isPending || plans.isPending || servers.isPending;
  const all = sites.data ?? [];
  const shown = all.filter((s) => matches(s, query));
  const running = all.filter((s) => s.status === "running").length;

  return (
    <div className="flex flex-col gap-6">
      <PageHeader
        title="Sites"
        description={
          sites.data === undefined
            ? "Every app instance the panel runs."
            : `${all.length} ${all.length === 1 ? "site" : "sites"}, ${running} running`
        }
        actions={
          <Link to="/sites/new" className={buttonVariants()}>
            <Plus aria-hidden="true" />
            New site
          </Link>
        }
      />
      {all.length > 0 ? (
        <div className="relative w-full sm:max-w-xs">
          <Search
            className="pointer-events-none absolute left-3 top-1/2 size-4 -translate-y-1/2 text-muted-foreground"
            aria-hidden="true"
          />
          <Input
            type="search"
            aria-label="Search sites"
            placeholder="Search by name, domain or app"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            className="pl-9"
          />
        </div>
      ) : null}
      {loading ? <SkeletonRows label="Loading sites…" /> : null}
      <ErrorText error={error} />
      {sites.data !== undefined && plans.data !== undefined && servers.data !== undefined ? (
        all.length > 0 && shown.length === 0 ? (
          <EmptyState
            icon={<Search className="size-6 text-link" aria-hidden="true" />}
            title={`No sites match “${query.trim()}”`}
            description="Try a different name, domain or app."
            action={
              <button
                type="button"
                onClick={() => setQuery("")}
                className={buttonVariants({ variant: "outline" })}
              >
                <X aria-hidden="true" />
                Clear search
              </button>
            }
          />
        ) : (
          <SitesTable sites={shown} plans={plans.data} servers={servers.data} />
        )
      ) : null}
    </div>
  );
}
