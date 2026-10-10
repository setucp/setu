import { useRouterState } from "@tanstack/react-router";
import { useJob, useServer, useSite } from "@/lib/api";
import { AppLink } from "@/components/AppLink";
import { Breadcrumb, BreadcrumbItem } from "@/components/ui/breadcrumb";

type Crumb =
  | { kind: "text"; label: string; href: string }
  | { kind: "site" | "server" | "job"; id: string; href: string };

const SECTIONS: Record<string, string> = {
  sites: "Sites",
  servers: "Servers",
  apps: "Apps",
  plans: "Plans",
  jobs: "Jobs",
  settings: "Settings",
};

const SITE_TABS: Record<string, string> = {
  logs: "Logs",
  env: "Env",
  backups: "Backups",
  updates: "Updates",
  limits: "Limits",
  danger: "Danger",
};

/** `/sites/s1/backups` → Sites / <site> / Backups. Unknown paths → one crumb. */
export function crumbsFor(pathname: string): Crumb[] {
  const parts = pathname.split("/").filter((p) => p !== "");
  const [section, second, third] = parts;
  if (section === undefined || SECTIONS[section] === undefined) {
    return [{ kind: "text", label: "Not found", href: pathname }];
  }
  const out: Crumb[] = [{ kind: "text", label: SECTIONS[section], href: `/${section}` }];
  if (second === undefined) return out;
  if (section === "sites" && second === "new") {
    out.push({ kind: "text", label: "New site", href: "/sites/new" });
    return out;
  }
  const entity = section === "sites" ? "site" : section === "servers" ? "server" : section === "jobs" ? "job" : null;
  if (entity === null) return out;
  const base = `/${section}/${second}`;
  out.push({ kind: entity, id: decodeURIComponent(second), href: base });
  if (entity === "site" && third !== undefined && SITE_TABS[third] !== undefined) {
    out.push({ kind: "text", label: SITE_TABS[third], href: `${base}/${third}` });
  }
  return out;
}

function SiteName({ id }: { id: string }) {
  return <>{useSite(id).data?.name ?? id}</>;
}
function ServerName({ id }: { id: string }) {
  return <>{useServer(id).data?.name ?? id}</>;
}
function JobName({ id }: { id: string }) {
  const kind = useJob(id).data?.kind;
  return <>{kind === undefined ? id : `${kind.replaceAll("_", " ")}`}</>;
}

function label(c: Crumb) {
  switch (c.kind) {
    case "text":
      return c.label;
    case "site":
      return <SiteName id={c.id} />;
    case "server":
      return <ServerName id={c.id} />;
    case "job":
      return <JobName id={c.id} />;
  }
}

/** Route-derived breadcrumb trail for the top bar. */
export function Breadcrumbs({
  className,
  hideSingle = false,
}: {
  className?: string;
  /** Render nothing for a top-level page (the page title already says it). */
  hideSingle?: boolean;
}) {
  const pathname = useRouterState({ select: (s) => s.location.pathname });
  const crumbs = crumbsFor(pathname);
  if (hideSingle && crumbs.length < 2) return null;
  return (
    <Breadcrumb className={className}>
      {crumbs.map((c, i) => {
        const last = i === crumbs.length - 1;
        return (
          <BreadcrumbItem key={c.href} current={last} last={last}>
            {last ? (
              label(c)
            ) : (
              <AppLink href={c.href} className="rounded-sm transition-colors hover:text-foreground">
                {label(c)}
              </AppLink>
            )}
          </BreadcrumbItem>
        );
      })}
    </Breadcrumb>
  );
}
