import { useAudit, useSetupStatus } from "@/lib/api";
import { ApiError } from "@/lib/client";
import { AuditTable, OperatorsPanel } from "./OperatorsPanel";

/**
 * Settings: the panel URL (read-only, from `GET /setup`; it is set in
 * `setu.toml`), operators with password changes, and the recent audit
 * trail. Trusted proxies and the backup hour are not exposed by the API
 * in this version, so they have no controls here.
 */
export function SettingsView() {
  const audit = useAudit();
  const setup = useSetupStatus();

  return (
    <div className="flex max-w-3xl flex-col gap-8">
      <h1 className="text-xl font-semibold tracking-tight">Settings</h1>
      <section className="flex flex-col gap-2">
        <h2 className="text-sm font-semibold">Panel URL</h2>
        {setup.data !== undefined ? (
          <code className="self-start rounded-md border bg-muted/40 px-3 py-2 font-mono text-xs">
            {setup.data.public_url}
          </code>
        ) : (
          <p className="text-sm text-muted-foreground">Loading…</p>
        )}
        <p className="text-xs text-muted-foreground">
          Set with <span className="font-mono">public_url</span> in{" "}
          <span className="font-mono">/etc/setu/setu.toml</span>; agents and the
          session cookie use it.
        </p>
      </section>
      <OperatorsPanel />
      <section className="flex flex-col gap-3">
        <h2 className="text-sm font-semibold">Recent activity</h2>
        {audit.isPending ? <p className="text-sm text-muted-foreground">Loading…</p> : null}
        {audit.error instanceof ApiError ? (
          <p role="alert" className="text-sm text-destructive">
            {audit.error.message}
          </p>
        ) : null}
        {audit.data !== undefined ? <AuditTable entries={audit.data} /> : null}
      </section>
    </div>
  );
}
