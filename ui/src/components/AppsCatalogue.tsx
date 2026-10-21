import { ExternalLink, Package, RefreshCw } from "lucide-react";
import { toast } from "sonner";
import { useApps, useAppVersions, useCheckApp, useClearAppSource } from "@/lib/api";
import { ApiError, type AppInfo } from "@/lib/client";
import { EmptyState } from "./EmptyState";
import { ErrorText } from "./ErrorText";
import { RelativeTime } from "./RelativeTime";
import { Badge } from "./ui/badge";
import { Button, buttonVariants } from "./ui/button";
import { useConfirm } from "./ui/dialog";
import { Skeleton } from "./ui/skeleton";

/**
 * The manifest URL an app follows (0.1.5): Setu checks it every 6 hours
 * and publishes new versions; Check now runs one check.
 */
function AppSource({ app }: { app: AppInfo }) {
  const check = useCheckApp(app.id);
  const clear = useClearAppSource(app.id);
  const confirm = useConfirm();
  const source = app.source;
  if (source === null || source === undefined) return null;

  function onCheck() {
    check.mutate(undefined, {
      onSuccess: (out) =>
        out.published
          ? toast.success(`Published ${app.name} ${out.published}`)
          : toast.success(`${app.name} is up to date`),
      onError: (err) => toast.error(err instanceof ApiError ? err.message : "Check failed"),
    });
  }

  function onStop() {
    void (async () => {
      const ok = await confirm({
        title: `Stop following ${app.name}'s manifest URL?`,
        description: "Published versions stay. New versions appear only when you publish them.",
        confirmLabel: "Stop following",
      });
      if (!ok) return;
      clear.mutate(undefined, {
        onSuccess: () => toast.success("Stopped following"),
        onError: (err) => toast.error(err instanceof ApiError ? err.message : "Stop failed"),
      });
    })();
  }

  return (
    <div className="space-y-1 border-t pt-2">
      <p className="truncate" title={source.url}>
        Follows <span className="font-mono">{source.url.replace(/^https?:\/\//, "")}</span>
      </p>
      {source.last_error ? (
        <p className="text-destructive">Last check failed: {source.last_error}</p>
      ) : source.checked_at ? (
        <p>
          Checked <RelativeTime iso={source.checked_at} />
        </p>
      ) : (
        <p>Not checked yet</p>
      )}
      <div className="flex flex-wrap gap-2 pt-1">
        <Button
          type="button"
          size="sm"
          variant="outline"
          loading={check.isPending}
          onClick={onCheck}
        >
          <RefreshCw className="size-3.5" aria-hidden="true" />
          Check now
        </Button>
        <Button type="button" size="sm" variant="ghost" loading={clear.isPending} onClick={onStop}>
          Stop following
        </Button>
      </div>
    </div>
  );
}

function AppCard({ app }: { app: AppInfo }) {
  const versions = useAppVersions(app.id);
  const list = versions.data ?? [];
  const [latest, ...older] = list;
  return (
    <li className="group flex min-w-0 flex-col rounded-lg border bg-card p-5 shadow-card transition-colors hover:border-primary/40">
      <div className="flex items-start gap-3">
        <span className="flex size-10 shrink-0 items-center justify-center rounded-lg bg-primary-subtle text-link ring-1 ring-primary/15">
          <Package className="size-5" aria-hidden="true" />
        </span>
        <div className="min-w-0 flex-1">
          <h2 className="truncate text-sm font-semibold">{app.name}</h2>
          <p className="truncate font-mono text-xs text-muted-foreground">{app.id}</p>
        </div>
        {versions.isPending ? (
          <Skeleton className="h-5 w-14 rounded-full" />
        ) : latest !== undefined ? (
          <Badge variant="info" title="Latest version">
            {latest.version}
          </Badge>
        ) : null}
      </div>
      <div className="mt-4 flex flex-1 flex-col justify-end gap-2 text-xs text-muted-foreground">
        <p>
          {versions.isPending
            ? "Loading versions…"
            : list.length === 0
              ? "No versions published."
              : `${list.length} ${list.length === 1 ? "version" : "versions"}${
                  older.length > 0
                    ? `; earlier: ${older
                        .slice(0, 3)
                        .map((v) => v.version)
                        .join(", ")}${older.length > 3 ? "…" : ""}`
                    : ""
                }`}
        </p>
        {app.homepage ? (
          <a
            href={app.homepage}
            target="_blank"
            rel="noreferrer"
            className="inline-flex max-w-full items-center gap-1 text-link hover:underline"
          >
            <span className="truncate">{app.homepage.replace(/^https?:\/\//, "")}</span>
            <ExternalLink className="size-3 shrink-0" aria-hidden="true" />
          </a>
        ) : null}
        <AppSource app={app} />
      </div>
    </li>
  );
}

/** App catalogue as cards, each with its latest version (newest first from the API). */
export function AppsCatalogue({ onAdd }: { onAdd?: () => void }) {
  const apps = useApps();

  if (apps.isPending) {
    return (
      <div role="status" className="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
        <span className="sr-only">Loading apps…</span>
        {Array.from({ length: 3 }, (_, i) => (
          <Skeleton key={i} className="h-36 rounded-lg" />
        ))}
      </div>
    );
  }
  if (apps.error instanceof ApiError) return <ErrorText error={apps.error} />;
  const rows = apps.data ?? [];
  if (rows.length === 0) {
    return (
      <EmptyState
        icon={<Package className="size-6 text-link" aria-hidden="true" />}
        title="No apps yet."
        description="Publish an app manifest to make it available for new sites."
        action={
          onAdd !== undefined ? (
            <button type="button" onClick={onAdd} className={buttonVariants()}>
              Add app
            </button>
          ) : undefined
        }
      />
    );
  }
  return (
    <ul aria-label="Apps" className="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
      {rows.map((a) => (
        <AppCard key={a.id} app={a} />
      ))}
    </ul>
  );
}
