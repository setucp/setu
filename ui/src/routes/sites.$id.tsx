import { Link, Outlet, createFileRoute } from "@tanstack/react-router";
import { StatusBadge } from "@/components/StatusBadge";
import { useSite } from "@/lib/api";
import { ApiError } from "@/lib/client";

export const Route = createFileRoute("/sites/$id")({
  component: SiteDetailLayout,
});

const TABS = [
  { to: "/sites/$id" as const, label: "Overview", exact: true },
  { to: "/sites/$id/logs" as const, label: "Logs", exact: false },
  { to: "/sites/$id/env" as const, label: "Env", exact: false },
  { to: "/sites/$id/backups" as const, label: "Backups", exact: false },
  { to: "/sites/$id/updates" as const, label: "Updates", exact: false },
  { to: "/sites/$id/limits" as const, label: "Limits", exact: false },
  { to: "/sites/$id/danger" as const, label: "Danger", exact: false },
];

/** Site detail shell: header plus tab navigation; each tab is a child route. */
function SiteDetailLayout() {
  const { id } = Route.useParams();
  const site = useSite(id);

  if (site.isPending) {
    return <p className="text-sm text-muted-foreground">Loading…</p>;
  }
  if (site.error instanceof ApiError) {
    return (
      <p role="alert" className="text-sm text-destructive">
        {site.error.message}
      </p>
    );
  }
  const s = site.data;
  if (s === undefined) {
    return <p className="text-sm text-muted-foreground">Loading…</p>;
  }

  return (
    <div className="flex flex-col gap-4">
      <div className="flex items-center gap-3">
        <h1 className="text-xl font-semibold tracking-tight">{s.name}</h1>
        <StatusBadge status={s.status} />
      </div>
      <nav className="flex flex-wrap items-center gap-1" aria-label="Site sections">
        {TABS.map((tab) => (
          <Link
            key={tab.label}
            to={tab.to}
            params={{ id }}
            activeOptions={tab.exact ? { exact: true } : undefined}
            activeProps={{
              className:
                "rounded-md bg-muted px-3 py-1.5 text-sm font-medium text-foreground",
            }}
            inactiveProps={{
              className:
                "rounded-md px-3 py-1.5 text-sm text-muted-foreground hover:text-foreground",
            }}
          >
            {tab.label}
          </Link>
        ))}
      </nav>
      <Outlet />
      <Link to="/sites" className="text-sm text-muted-foreground hover:text-foreground">
        ← All sites
      </Link>
    </div>
  );
}
