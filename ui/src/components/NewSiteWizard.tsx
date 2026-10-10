import * as React from "react";
import {
  useApps,
  useAppVersions,
  useCreateSite,
  usePlans,
  useServers,
} from "@/lib/api";
import { ApiError } from "@/lib/client";
import { Button } from "./ui/button";
import { Input } from "./ui/input";
import { Select } from "./ui/select";

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

/**
 * New-site flow (spec §6): app → version → plan → server (auto) → domain →
 * owner email → prompt-type env vars. Posts the payload, then reports the
 * provision job id so the page can navigate to it.
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
  const [formError, setFormError] = React.useState<string | null>(null);
  const create = useCreateSite();

  const loading = apps.isPending || plans.isPending || servers.isPending;
  const loadError = apps.error ?? plans.error ?? servers.error;

  function field(label: string, control: React.ReactNode) {
    return (
      <label className="flex flex-col gap-1.5 text-sm font-medium">
        {label}
        {control}
      </label>
    );
  }

  return (
    <form
      aria-label="New site"
      className="flex max-w-xl flex-col gap-4"
      onSubmit={(e) => {
        e.preventDefault();
        if (appId === "" || version === "" || planId === "") {
          setFormError("Choose an application, version and plan.");
          return;
        }
        if (name.trim() === "") {
          setFormError("Give the site a name.");
          return;
        }
        if (!DOMAIN_RE.test(domain.trim())) {
          setFormError("Enter a valid domain, e.g. blog.example.com.");
          return;
        }
        if (!ownerEmail.includes("@")) {
          setFormError("Enter a valid owner email.");
          return;
        }
        const { prompts, badLines } = parsePrompts(promptsText);
        if (badLines.length > 0) {
          setFormError(
            `Prompt values must be KEY=value lines (line ${badLines.join(", ")}).`,
          );
          return;
        }
        setFormError(null);
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
      <h1 className="text-xl font-semibold tracking-tight">New site</h1>
      {loading ? <p className="text-sm text-muted-foreground">Loading…</p> : null}
      {loadError instanceof ApiError ? (
        <p role="alert" className="text-sm text-destructive">
          {loadError.message}
        </p>
      ) : null}

      {field(
        "Application",
        <Select
          aria-label="Application"
          value={appId}
          onChange={(e) => {
            setAppId(e.target.value);
            setVersion("");
          }}
        >
          <option value="">Select an app…</option>
          {(apps.data ?? []).map((a) => (
            <option key={a.id} value={a.id}>
              {a.name}
            </option>
          ))}
        </Select>,
      )}

      {field(
        "Version",
        <Select
          aria-label="Version"
          value={version}
          disabled={appId === ""}
          onChange={(e) => setVersion(e.target.value)}
        >
          <option value="">{versions.isPending ? "Loading…" : "Select a version…"}</option>
          {(versions.data ?? []).map((v) => (
            <option key={v.id} value={v.version}>
              {v.version}
            </option>
          ))}
        </Select>,
      )}

      {field(
        "Plan",
        <Select aria-label="Plan" value={planId} onChange={(e) => setPlanId(e.target.value)}>
          <option value="">Select a plan…</option>
          {(plans.data ?? []).map((p) => (
            <option key={p.id} value={p.id}>
              {p.name}
            </option>
          ))}
        </Select>,
      )}

      {field(
        "Server",
        <Select aria-label="Server" value={serverId} onChange={(e) => setServerId(e.target.value)}>
          <option value="">Automatic (most free memory)</option>
          {(servers.data ?? []).map((s) => (
            <option key={s.id} value={s.id}>
              {s.name} ({s.status})
            </option>
          ))}
        </Select>,
      )}

      {field(
        "Site name",
        <Input value={name} onChange={(e) => setName(e.target.value)} placeholder="blog" />,
      )}
      {field(
        "Domain",
        <Input
          value={domain}
          onChange={(e) => setDomain(e.target.value)}
          placeholder="blog.example.com"
          inputMode="url"
        />,
      )}
      {field(
        "Owner email",
        <Input
          type="email"
          value={ownerEmail}
          onChange={(e) => setOwnerEmail(e.target.value)}
          placeholder="owner@example.com"
        />,
      )}
      {field(
        "Prompt values",
        <textarea
          aria-label="Prompt values"
          value={promptsText}
          onChange={(e) => setPromptsText(e.target.value)}
          placeholder={"HELLO_NAME=blog"}
          rows={3}
          className="flex w-full rounded-md border border-input bg-background px-3 py-2 font-mono text-sm shadow-xs placeholder:text-muted-foreground focus-visible:outline-hidden focus-visible:ring-1 focus-visible:ring-ring"
        />,
      )}

      {formError !== null ? (
        <p role="alert" className="text-sm text-destructive">
          {formError}
        </p>
      ) : null}
      {create.isError && create.error instanceof ApiError ? (
        <p role="alert" className="text-sm text-destructive">
          {create.error.message}
        </p>
      ) : null}
      <Button type="submit" disabled={create.isPending} className="self-start">
        {create.isPending ? "Creating…" : "Create site"}
      </Button>
    </form>
  );
}
