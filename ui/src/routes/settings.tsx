import { createFileRoute } from "@tanstack/react-router";
import { AuditTable, OperatorsPanel } from "@/components/OperatorsPanel";
import { useAudit } from "@/lib/api";
import { ApiError } from "@/lib/client";

export const Route = createFileRoute("/settings")({
  component: SettingsPage,
});

/**
 * Settings: operators (list + create; the API offers no update/delete) and
 * the recent audit trail. There is no TOTP endpoint in this version, so 2FA
 * has no controls here — just a note saying so.
 */
function SettingsPage() {
  const audit = useAudit();

  return (
    <div className="flex max-w-3xl flex-col gap-8">
      <h1 className="text-xl font-semibold tracking-tight">Settings</h1>
      <OperatorsPanel />
      <section className="flex flex-col gap-3">
        <h2 className="text-sm font-semibold">Two-factor authentication</h2>
        <p className="text-sm text-muted-foreground">
          TOTP two-factor authentication is not available in this version — there
          is no API for it yet.
        </p>
      </section>
      <section className="flex flex-col gap-3">
        <h2 className="text-sm font-semibold">Recent activity</h2>
        {audit.isPending ? (
          <p className="text-sm text-muted-foreground">Loading…</p>
        ) : null}
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
