import { Link, createFileRoute } from "@tanstack/react-router";
import { StatusBadge } from "@/components/StatusBadge";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { useSite } from "@/lib/api";
import { ApiError } from "@/lib/client";

export const Route = createFileRoute("/sites/$id")({
  component: SiteOverviewPage,
});

/**
 * Task-12 scope: a simple overview (status, URL, app, server, plan). The
 * full tab set (logs, env, backups, updates, limits, danger) is Task 13;
 * the disabled tabs below mark that boundary in the UI itself.
 */
function SiteOverviewPage() {
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
      <Tabs value="overview" onValueChange={() => undefined}>
        <TabsList>
          <TabsTrigger value="overview">Overview</TabsTrigger>
          {["Logs", "Env", "Backups", "Updates", "Danger"].map((tab) => (
            <TabsTrigger key={tab} value={tab.toLowerCase()} disabled title="Coming in Task 13">
              {tab}
            </TabsTrigger>
          ))}
        </TabsList>
        <TabsContent value="overview">
          <dl className="grid max-w-xl grid-cols-[140px_1fr] gap-x-4 gap-y-2 text-sm">
            <dt className="text-muted-foreground">URL</dt>
            <dd>
              <a
                href={`https://${s.domain}`}
                target="_blank"
                rel="noreferrer"
                className="text-primary underline-offset-2 hover:underline"
              >
                {s.domain}
              </a>
            </dd>
            <dt className="text-muted-foreground">App</dt>
            <dd>
              {s.app_id} {s.app_version}
            </dd>
            <dt className="text-muted-foreground">Server</dt>
            <dd>{s.server_id}</dd>
            <dt className="text-muted-foreground">Plan</dt>
            <dd>{s.plan_id}</dd>
            <dt className="text-muted-foreground">Owner</dt>
            <dd>{s.owner_email}</dd>
            <dt className="text-muted-foreground">Port</dt>
            <dd>{s.port}</dd>
            {s.failed_step !== null && s.failed_step !== undefined ? (
              <>
                <dt className="text-muted-foreground">Failed step</dt>
                <dd className="text-destructive">{s.failed_step}</dd>
              </>
            ) : null}
          </dl>
        </TabsContent>
      </Tabs>
      <Link to="/sites" className="text-sm text-muted-foreground hover:text-foreground">
        ← All sites
      </Link>
    </div>
  );
}
