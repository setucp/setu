import { createFileRoute } from "@tanstack/react-router";
import { useSite } from "@/lib/api";
import { ApiError } from "@/lib/client";

export const Route = createFileRoute("/sites/$id/")({
  component: SiteOverviewTab,
});

/** Overview tab (spec §6): status, URL, admin link, health, placement. */
function SiteOverviewTab() {
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
      <dd>
        <a
          href={`/servers/${s.server_id}`}
          className="text-primary underline-offset-2 hover:underline"
        >
          {s.server_id}
        </a>
      </dd>
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
  );
}
