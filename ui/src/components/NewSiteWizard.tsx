import * as React from "react";
import { Check, Globe } from "lucide-react";
import {
  useApps,
  useAppVersions,
  useCreateSite,
  usePlans,
  useServers,
} from "@/lib/api";
import { formatMb } from "@/lib/format";
import { cn } from "@/lib/utils";
import { ErrorText } from "./ErrorText";
import { PageHeader } from "./PageHeader";
import { Alert } from "./ui/alert";
import { Button } from "./ui/button";
import { Card } from "./ui/card";
import { Field } from "./ui/field";
import { Input } from "./ui/input";
import { Select } from "./ui/select";
import { Skeleton } from "./ui/skeleton";
import { Textarea } from "./ui/textarea";

const DOMAIN_RE = /^[a-z0-9]([a-z0-9-]*[a-z0-9])?(\.[a-z0-9]([a-z0-9-]*[a-z0-9])?)+$/i;

/** Parse `KEY=value` lines into the `prompts` map the API expects.
 * Returns the map plus the 1-based numbers of lines that do not parse
 * (missing `=` or blank key), so the form can reject typos up front
 * instead of failing the provision job later. */
export function parsePrompts(text: string): {
  prompts: Record<string, string>;
  badLines: number[];
} {
  const prompts: Record<string, string> = {};
  const badLines: number[] = [];
  let n = 0;
  for (const line of text.split("\n")) {
    n += 1;
    const trimmed = line.trim();
    if (trimmed === "" || trimmed.startsWith("#")) continue;
    const eq = trimmed.indexOf("=");
    if (eq > 0 && trimmed.slice(0, eq).trim() !== "") {
      prompts[trimmed.slice(0, eq).trim()] = trimmed.slice(eq + 1).trim();
    } else {
      badLines.push(n);
    }
  }
  return { prompts, badLines };
}

type FieldName = "app" | "version" | "plan" | "name" | "domain" | "owner" | "prompts";
type Errors = Partial<Record<FieldName, string>>;

function Step({
  n,
  title,
  description,
  done,
  children,
}: {
  n: number;
  title: string;
  description?: string;
  done: boolean;
  children: React.ReactNode;
}) {
  return (
    <li className="relative flex gap-4 pb-8 last:pb-0">
      <span
        aria-hidden="true"
        className="absolute left-[13px] top-8 bottom-0 w-px bg-border [li:last-child>&]:hidden"
      />
      <span
        className={cn(
          "relative z-10 flex size-7 shrink-0 items-center justify-center rounded-full border text-xs font-semibold transition-colors",
          done ? "border-primary bg-primary text-primary-foreground" : "bg-card text-muted-foreground",
        )}
        aria-hidden="true"
      >
        {done ? <Check className="size-3.5" /> : n}
      </span>
      <section className="min-w-0 flex-1">
        <h2 className="text-sm font-semibold leading-7">
          <span className="sr-only">Step {n}: </span>
          {title}
        </h2>
        {description !== undefined ? (
          <p className="text-sm text-muted-foreground">{description}</p>
        ) : null}
        <div className="mt-4 flex flex-col gap-4">{children}</div>
      </section>
    </li>
  );
}

function SummaryRow({ term, value }: { term: string; value: React.ReactNode }) {
  const empty = value === "" || value === null || value === undefined;
  return (
    <div className="flex items-baseline justify-between gap-4 py-2 text-sm">
      <dt className="text-muted-foreground">{term}</dt>
      <dd className={cn("min-w-0 truncate text-right font-medium", empty && "font-normal text-muted-foreground/70")}>
        {empty ? "Not set" : value}
      </dd>
    </div>
  );
}

/**
 * New-site flow (spec §6): app → version → plan → server (auto) → domain →
 * owner email → prompt-type env vars, as numbered steps beside a live
 * summary. Posts the payload, then reports the provision job id so the
 * page can navigate to it.
 */
export function NewSiteWizard({ onCreated }: { onCreated?: (jobId: string) => void }) {
  const apps = useApps();
  const plans = usePlans();
  const servers = useServers();
  const [appId, setAppId] = React.useState("");
  const versions = useAppVersions(appId === "" ? null : appId);
  const [version, setVersion] = React.useState("");
  const [planId, setPlanId] = React.useState("");
  const [serverId, setServerId] = React.useState("");
  const [name, setName] = React.useState("");
  const [domain, setDomain] = React.useState("");
  const [ownerEmail, setOwnerEmail] = React.useState("");
  const [promptsText, setPromptsText] = React.useState("");
  const [errors, setErrors] = React.useState<Errors>({});
  const create = useCreateSite();

  const loading = apps.isPending || plans.isPending || servers.isPending;
  const loadError = apps.error ?? plans.error ?? servers.error;
  const app = apps.data?.find((a) => a.id === appId);
  const plan = plans.data?.find((p) => p.id === planId);
  const server = servers.data?.find((s) => s.id === serverId);
  const domainOk = DOMAIN_RE.test(domain.trim());

  function clear(field: FieldName) {
    setErrors((prev) => (prev[field] === undefined ? prev : { ...prev, [field]: undefined }));
  }

  function validate(): { ok: boolean; prompts: Record<string, string> } {
    const next: Errors = {};
    if (appId === "") next.app = "Choose an application.";
    if (version === "") next.version = "Choose a version.";
    if (planId === "") next.plan = "Choose a plan.";
    if (name.trim() === "") next.name = "Give the site a name.";
    if (!domainOk) next.domain = "Enter a valid domain, e.g. blog.example.com.";
    if (!ownerEmail.includes("@")) next.owner = "Enter a valid owner email.";
    const { prompts, badLines } = parsePrompts(promptsText);
    if (badLines.length > 0) {
      next.prompts = `Prompt values must be KEY=value lines (line ${badLines.join(", ")}).`;
    }
    setErrors(next);
    return { ok: Object.keys(next).length === 0, prompts };
  }

  const hasErrors = Object.values(errors).some((e) => e !== undefined);

  return (
    <div className="flex flex-col gap-6">
      <PageHeader
        title="New site"
        description="Pick an app and a plan, point a domain at it, and Setu provisions the rest."
      />
      <ErrorText error={loadError} />
      <form
        aria-label="New site"
        noValidate
        className="grid items-start gap-6 lg:grid-cols-[minmax(0,1fr)_320px]"
        onSubmit={(e) => {
          e.preventDefault();
          const { ok, prompts } = validate();
          if (!ok) return;
          create.mutate(
            {
              name: name.trim(),
              app_id: appId,
              version,
              plan_id: planId,
              domain: domain.trim(),
              owner_email: ownerEmail.trim(),
              prompts,
              ...(serverId === "" ? {} : { server_id: serverId }),
            },
            { onSuccess: (res) => onCreated?.(res.job.id) },
          );
        }}
      >
        <Card className="p-5 sm:p-6">
          {loading ? (
            <div role="status" className="mb-6 flex flex-col gap-3">
              <span className="sr-only">Loading…</span>
              <Skeleton className="h-4 w-40" />
              <Skeleton className="h-10 w-full" />
            </div>
          ) : null}
          <ol className="flex flex-col">
            <Step
              n={1}
              title="Application"
              description="What the site runs: an app from your catalogue and one of its versions."
              done={appId !== "" && version !== ""}
            >
              <div className="grid gap-4 sm:grid-cols-2">
                <Field label="Application" error={errors.app}>
                  <Select
                    value={appId}
                    onChange={(e) => {
                      setAppId(e.target.value);
                      setVersion("");
                      clear("app");
                    }}
                  >
                    <option value="">Select an app…</option>
                    {(apps.data ?? []).map((a) => (
                      <option key={a.id} value={a.id}>
                        {a.name}
                      </option>
                    ))}
                  </Select>
                </Field>
                <Field label="Version" error={errors.version}>
                  <Select
                    value={version}
                    disabled={appId === ""}
                    onChange={(e) => {
                      setVersion(e.target.value);
                      clear("version");
                    }}
                  >
                    <option value="">
                      {appId !== "" && versions.isPending ? "Loading…" : "Select a version…"}
                    </option>
                    {(versions.data ?? []).map((v) => (
                      <option key={v.id} value={v.version}>
                        {v.version}
                      </option>
                    ))}
                  </Select>
                </Field>
              </div>
            </Step>

            <Step
              n={2}
              title="Plan and placement"
              description="The plan caps memory, CPU and disk. Leave the server on automatic unless you need a specific one."
              done={planId !== ""}
            >
              <div className="grid gap-4 sm:grid-cols-2">
                <Field label="Plan" error={errors.plan}>
                  <Select
                    value={planId}
                    onChange={(e) => {
                      setPlanId(e.target.value);
                      clear("plan");
                    }}
                  >
                    <option value="">Select a plan…</option>
                    {(plans.data ?? []).map((p) => (
                      <option key={p.id} value={p.id}>
                        {p.name}
                      </option>
                    ))}
                  </Select>
                </Field>
                <Field label="Server">
                  <Select value={serverId} onChange={(e) => setServerId(e.target.value)}>
                    <option value="">Automatic (most free memory)</option>
                    {(servers.data ?? []).map((s) => (
                      <option key={s.id} value={s.id}>
                        {s.name} ({s.status})
                      </option>
                    ))}
                  </Select>
                </Field>
              </div>
            </Step>

            <Step
              n={3}
              title="Name and domain"
              description="Point the domain's DNS at the server; HTTPS is issued automatically."
              done={name.trim() !== "" && domainOk}
            >
              <div className="grid gap-4 sm:grid-cols-2">
                <Field label="Site name" error={errors.name} hint="Lowercase, used in ids and paths.">
                  <Input
                    value={name}
                    onChange={(e) => {
                      setName(e.target.value);
                      clear("name");
                    }}
                    placeholder="blog"
                    autoComplete="off"
                  />
                </Field>
                <Field label="Domain" error={errors.domain}>
                  <Input
                    value={domain}
                    onChange={(e) => {
                      setDomain(e.target.value);
                      clear("domain");
                    }}
                    placeholder="blog.example.com"
                    inputMode="url"
                    autoCapitalize="none"
                    autoComplete="off"
                    spellCheck={false}
                  />
                </Field>
              </div>
            </Step>

            <Step
              n={4}
              title="Owner"
              description="The site's own administrator; the app receives this address on first boot."
              done={ownerEmail.includes("@")}
            >
              <Field label="Owner email" error={errors.owner}>
                <Input
                  type="email"
                  value={ownerEmail}
                  onChange={(e) => {
                    setOwnerEmail(e.target.value);
                    clear("owner");
                  }}
                  placeholder="owner@example.com"
                  autoComplete="off"
                />
              </Field>
            </Step>

            <Step
              n={5}
              title="Prompts"
              description="Answers for the app's prompt variables, one NAME=value per line. Leave empty if the app asks for none."
              done={promptsText.trim() !== "" && errors.prompts === undefined}
            >
              <Field label="Prompt values" error={errors.prompts} optional>
                <Textarea
                  value={promptsText}
                  onChange={(e) => {
                    setPromptsText(e.target.value);
                    clear("prompts");
                  }}
                  placeholder={"HELLO_NAME=blog"}
                  rows={3}
                  spellCheck={false}
                  className="font-mono text-xs"
                />
              </Field>
            </Step>
          </ol>
        </Card>

        <aside className="flex flex-col gap-4 lg:sticky lg:top-20" aria-label="Summary">
          <Card>
            <div className="flex items-center gap-3 border-b px-5 py-4">
              <span className="flex size-9 items-center justify-center rounded-lg bg-primary-subtle text-link">
                <Globe className="size-4" aria-hidden="true" />
              </span>
              <div className="min-w-0">
                <p className="truncate text-sm font-semibold">{name.trim() || "Untitled site"}</p>
                <p className="truncate text-xs text-muted-foreground">
                  {domainOk ? `https://${domain.trim()}` : "No domain yet"}
                </p>
              </div>
            </div>
            <dl className="divide-y px-5 py-1">
              <SummaryRow term="App" value={app === undefined ? "" : `${app.name} ${version}`.trim()} />
              <SummaryRow term="Plan" value={plan?.name ?? ""} />
              <SummaryRow
                term="Limits"
                value={
                  plan === undefined
                    ? ""
                    : `${formatMb(plan.limits.memory_mb)} · ${plan.limits.cpu_percent ?? "—"}% CPU · ${formatMb(plan.limits.disk_mb)}`
                }
              />
              <SummaryRow term="Server" value={server?.name ?? "Automatic"} />
              <SummaryRow term="Owner" value={ownerEmail.trim()} />
            </dl>
            <div className="flex flex-col gap-3 border-t bg-surface px-5 py-4">
              {hasErrors ? (
                <Alert variant="destructive" role="status">
                  Fix the highlighted fields first.
                </Alert>
              ) : null}
              <ErrorText error={create.isError ? create.error : null} />
              <Button type="submit" loading={create.isPending} className="w-full">
                {create.isPending ? "Creating…" : "Create site"}
              </Button>
              <p className="text-xs text-muted-foreground">
                Provisioning runs as a job; you will see its live log next.
              </p>
            </div>
          </Card>
        </aside>
      </form>
    </div>
  );
}
