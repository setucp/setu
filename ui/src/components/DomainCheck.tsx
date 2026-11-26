import * as React from "react";
import { CircleCheck, CircleX, RefreshCw, TriangleAlert } from "lucide-react";
import { useDomainCheck } from "@/lib/api";
import type { DomainCheck as DomainCheckResult, FindingLevel } from "@/lib/client";
import { cn } from "@/lib/utils";
import { ErrorText } from "./ErrorText";
import { Button } from "./ui/button";

/** How long the domain must stay unchanged before it is checked. */
export const DOMAIN_CHECK_DEBOUNCE_MS = 600;

const findingTitle: Record<string, string> = {
  dns: "Resolves",
  points_at_server: "Points at the server",
  proxy: "Proxy",
  caa: "CAA (who may issue certificates)",
};

const levelIcon: Record<
  FindingLevel,
  { Icon: typeof CircleCheck; className: string; label: string }
> = {
  ok: { Icon: CircleCheck, className: "text-success", label: "OK" },
  warn: { Icon: TriangleAlert, className: "text-warning", label: "Warning" },
  error: { Icon: CircleX, className: "text-destructive", label: "Problem" },
};

/** Where the check runs: the new-site form, or a site's extra hostname. */
export type DomainCheckContext = "site" | "alias";

/** One-line verdict over the findings. */
export function domainCheckSummary(
  result: DomainCheckResult,
  context: DomainCheckContext = "site",
): string {
  if (!result.ok) {
    return context === "site"
      ? "HTTPS will not work until this is fixed. You can still create the site and fix DNS afterwards."
      : "HTTPS will not work on this name until this is fixed. You can still add it and fix DNS afterwards.";
  }
  if (result.findings.some((f) => f.level === "warn")) {
    return "DNS works, with warnings.";
  }
  return "DNS points at the server; HTTPS can be issued.";
}

function useDebounced<T>(value: T, ms: number): T {
  const [debounced, setDebounced] = React.useState(value);
  React.useEffect(() => {
    const t = setTimeout(() => setDebounced(value), ms);
    return () => clearTimeout(t);
  }, [value, ms]);
  return debounced;
}

/**
 * The DNS pre-check under a domain field (`GET /domain-check`): once a
 * valid domain has stopped changing and the server is known, it shows
 * whether the name resolves, points at that server, sits behind a proxy and
 * allows Let's Encrypt. Advisory only: it never blocks the form.
 */
export function DomainCheck({
  domain,
  serverId,
  context = "site",
}: {
  /** The trimmed domain, or `null` while it is not a valid hostname. */
  domain: string | null;
  /** The server the site goes to, or `null` when placement is automatic among several. */
  serverId: string | null;
  /** Which form the check sits in; words the summary for it. */
  context?: DomainCheckContext;
}) {
  const settled = useDebounced(domain, DOMAIN_CHECK_DEBOUNCE_MS);
  const current = settled !== null && settled === domain ? settled : null;
  const check = useDomainCheck(current, serverId);

  if (domain === null) return null;
  if (serverId === null) {
    return (
      <p className="text-xs text-muted-foreground">
        Choose a server in step 2 to check that this domain points at it.
      </p>
    );
  }
  // Only the settled check is announced: a live region that changed on
  // every keystroke would read out each partial name.
  if (current === null || check.isPending) {
    return (
      <p className="text-xs text-muted-foreground" aria-hidden={current === null}>
        Checking DNS for {domain}…
      </p>
    );
  }
  if (check.isError) {
    return (
      <div className="flex items-start justify-between gap-3">
        <ErrorText error={check.error} />
        <CheckAgain fetching={check.isFetching} onClick={() => void check.refetch()} />
      </div>
    );
  }

  const result = check.data;
  return (
    <section aria-label="DNS check" className="rounded-lg border bg-surface px-4 py-3">
      <div className="flex items-start justify-between gap-3">
        <p role="status" className={cn("text-sm font-medium", !result.ok && "text-destructive")}>
          {domainCheckSummary(result, context)}
        </p>
        <CheckAgain fetching={check.isFetching} onClick={() => void check.refetch()} />
      </div>
      <ul className="mt-2 flex flex-col gap-2">
        {result.findings.map((f) => {
          const { Icon, className, label } = levelIcon[f.level];
          return (
            <li key={f.id} className="flex gap-2 text-sm">
              <Icon
                className={cn("mt-0.5 size-4 shrink-0", className)}
                aria-label={label}
                role="img"
              />
              <span className="min-w-0">
                <span className="font-medium">{findingTitle[f.id] ?? f.id}</span>
                <span className="text-muted-foreground">: {f.message}</span>
              </span>
            </li>
          );
        })}
      </ul>
      {result.server_ip ? (
        <p className="mt-2 text-xs text-muted-foreground">
          Server address: <span className="font-mono">{result.server_ip}</span>
        </p>
      ) : null}
    </section>
  );
}

function CheckAgain({ fetching, onClick }: { fetching: boolean; onClick: () => void }) {
  return (
    <Button type="button" variant="ghost" size="sm" loading={fetching} onClick={onClick}>
      {fetching ? null : <RefreshCw className="size-3.5" aria-hidden="true" />}
      Check again
    </Button>
  );
}
