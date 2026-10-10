import * as React from "react";
import { KeyRound, Monitor, Moon, Sun } from "lucide-react";
import { usePanelBackup, useAudit, useMe, useOperators, useSetupStatus } from "@/lib/api";
import { useTheme, type Theme } from "@/lib/theme";
import { formatBytes } from "@/lib/format";
import { cn } from "@/lib/utils";
import { CopyField } from "./CopyButton";
import { ErrorText } from "./ErrorText";
import { RelativeTime } from "./RelativeTime";
import { AuditTable, OperatorsPanel, PasswordDialog } from "./OperatorsPanel";
import { PageHeader } from "./PageHeader";
import { Avatar } from "./ui/avatar";
import { Alert } from "./ui/alert";
import { Badge } from "./ui/badge";
import { Button } from "./ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "./ui/card";
import { Skeleton, SkeletonRows } from "./ui/skeleton";

const SECTIONS = [
  { id: "profile", label: "Profile" },
  { id: "operators", label: "Operators" },
  { id: "panel", label: "Panel" },
  { id: "appearance", label: "Appearance" },
  { id: "activity", label: "Activity" },
] as const;

function Section({
  id,
  title,
  description,
  children,
}: {
  id: string;
  title: string;
  description?: string;
  children: React.ReactNode;
}) {
  return (
    <Card id={id} className="scroll-mt-20">
      <CardHeader>
        <CardTitle>{title}</CardTitle>
        {description !== undefined ? <CardDescription>{description}</CardDescription> : null}
      </CardHeader>
      <CardContent>{children}</CardContent>
    </Card>
  );
}

function Profile() {
  const me = useMe();
  const [changing, setChanging] = React.useState(false);
  if (me.data === undefined || me.data === null) {
    return <Skeleton className="h-12 w-full" />;
  }
  const op = me.data;
  return (
    <div className="flex flex-col gap-4 sm:flex-row sm:items-center">
      <div className="flex min-w-0 flex-1 items-center gap-3">
        <Avatar name={op.email} className="size-11 text-sm" />
        <div className="min-w-0">
          <p className="truncate text-sm font-semibold">{op.email}</p>
          <Badge variant="info" className="mt-1 capitalize">
            {op.role}
          </Badge>
        </div>
      </div>
      <Button type="button" variant="outline" onClick={() => setChanging(true)}>
        <KeyRound aria-hidden="true" />
        Change your password
      </Button>
      <PasswordDialog operator={changing ? op : null} self onClose={() => setChanging(false)} />
    </div>
  );
}

const THEMES: Array<{ value: Theme; label: string; hint: string; icon: typeof Sun }> = [
  { value: "light", label: "Light", hint: "Bright surfaces", icon: Sun },
  { value: "dark", label: "Dark", hint: "Navy surfaces", icon: Moon },
  { value: "system", label: "System", hint: "Follow this device", icon: Monitor },
];

function Appearance() {
  const { theme, setTheme } = useTheme();
  return (
    <fieldset>
      <legend className="sr-only">Theme</legend>
      <div className="grid gap-3 sm:grid-cols-3">
        {THEMES.map(({ value, label, hint, icon: Icon }) => (
          <label
            key={value}
            className={cn(
              "touch-target flex cursor-pointer items-center gap-3 rounded-lg border p-3 transition-colors has-[:focus-visible]:ring-2 has-[:focus-visible]:ring-ring",
              theme === value ? "border-primary bg-primary-subtle" : "hover:bg-accent",
            )}
          >
            <input
              type="radio"
              name="theme"
              value={value}
              checked={theme === value}
              onChange={() => setTheme(value)}
              className="sr-only"
            />
            <span
              className={cn(
                "flex size-9 shrink-0 items-center justify-center rounded-md border bg-card",
                theme === value && "text-link",
              )}
            >
              <Icon className="size-4" aria-hidden="true" />
            </span>
            <span className="min-w-0">
              <span className="block text-sm font-medium">{label}</span>
              <span className="block text-xs text-muted-foreground">{hint}</span>
            </span>
          </label>
        ))}
      </div>
    </fieldset>
  );
}

/**
 * Settings: your profile, operators with password changes, the panel URL
 * (read-only, from `GET /setup`; it is set in `setu.toml`), the theme,
 * and the recent audit trail. Trusted proxies and the backup hour are not
 * exposed by the API in this version, so they have no controls here.
 */
export function SettingsView() {
  const audit = useAudit();
  const setup = useSetupStatus();
  const operators = useOperators();

  return (
    <div className="flex flex-col gap-6">
      <PageHeader title="Settings" description="Your account, the people who can sign in, and how the panel looks." />
      <div className="grid items-start gap-6 lg:grid-cols-[180px_minmax(0,1fr)]">
        <nav aria-label="Settings sections" className="hidden lg:sticky lg:top-20 lg:block">
          <ul className="flex flex-col gap-0.5 text-sm">
            {SECTIONS.map((s) => (
              <li key={s.id}>
                <a
                  href={`#${s.id}`}
                  className="block rounded-md px-3 py-1.5 text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
                >
                  {s.label}
                </a>
              </li>
            ))}
          </ul>
        </nav>
        <div className="flex min-w-0 flex-col gap-6">
          <Section id="profile" title="Profile" description="The operator you are signed in as.">
            <Profile />
          </Section>
          <Section id="operators" title="Operators">
            <OperatorsPanel />
          </Section>
          <Section id="panel" title="Panel" description="Agents and the session cookie use this address.">
            {setup.data !== undefined ? (
              <CopyField value={setup.data.public_url} label="Panel URL" />
            ) : (
              <Skeleton className="h-10 w-full" />
            )}
            <p className="mt-3 text-xs text-muted-foreground">
              Set with <code>public_url</code> in <code>/etc/setu/setu.toml</code>.
            </p>
            <SelfBackupStatus />
          </Section>
          <Section id="appearance" title="Appearance" description="Stored in this browser only.">
            <Appearance />
          </Section>
          <Section id="activity" title="Recent activity" description="The last 100 audited actions.">
            {audit.isPending ? <SkeletonRows rows={3} label="Loading activity…" /> : null}
            <ErrorText error={audit.error} />
            {audit.data !== undefined ? (
              <AuditTable entries={audit.data} operators={operators.data ?? []} />
            ) : null}
          </Section>
        </div>
      </div>
    </div>
  );
}

/** The control plane's own nightly `pg_dump` (0.1.1 #9): last success, failures. */
function SelfBackupStatus() {
  const backup = usePanelBackup();
  if (backup.isPending) return <Skeleton className="mt-4 h-10 w-full" />;
  if (backup.error) return <ErrorText error={backup.error} />;
  const b = backup.data;
  const failedLast =
    b.last_error !== null &&
    b.last_error !== undefined &&
    (b.last_success_at === null ||
      b.last_success_at === undefined ||
      (b.last_attempt_at ?? "") > b.last_success_at);
  return (
    <div className="mt-4 flex flex-col gap-2 border-t pt-4 text-sm">
      <p className="font-medium">Control-plane backup</p>
      {b.last_success_at ? (
        <p>
          Last backup <RelativeTime iso={b.last_success_at} />: <code>{b.file}</code> (
          {formatBytes(b.size_bytes)})
        </p>
      ) : (
        <p className="text-muted-foreground">No backup yet. One runs nightly at 03:00.</p>
      )}
      {failedLast ? <Alert variant="warning">The last attempt failed: {b.last_error}</Alert> : null}
      <p className="text-xs text-muted-foreground">
        Seven are kept in <code>{b.dir}</code>. Copy them, and <code>setu.toml</code>, off this
        box.
      </p>
    </div>
  );
}
