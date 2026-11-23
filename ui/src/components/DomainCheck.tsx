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

const levelIcon: Record<FindingLevel, { Icon: typeof CircleCheck; className: string; label: string }> = {
  ok: { Icon: CircleCheck, className: "text-success", label: "OK" },
  warn: { Icon: TriangleAlert, className: "text-warning", label: "Warning" },
  error: { Icon: CircleX, className: "text-destructive", label: "Problem" },
};

/** One-line verdict over the findings. */
export function domainCheckSummary(result: DomainCheckResult): string {
  if (!result.ok) {
    return "HTTPS will not work until this is fixed. You can still create the site and fix DNS afterwards.";
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
 * The DNS pre-check under the new-site domain field (`GET /domain-check`):
 * once a valid domain has stopped changing and the server is known, it shows
 * whether the name resolves, points at that server, sits behind a proxy and
 * allows Let's Encrypt. Advisory only: it never blocks creating the site.
 */
export function DomainCheck({
  domain,
  serverId,
}: {
  /** The trimmed domain, or `null` while it is not a valid hostname. */
  domain: string | null;
  /** The server the site goes to, or `null` when placement is automatic among several. */
  serverId: string | null;
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
  if (current === null || check.isPending) {
    return (
      <p role="status" className="text-xs text-muted-foreground">
        Checking DNS for {domain}…
      </p>
    );
  }
  if (check.isError) {
    return <ErrorText error={check.error} />;
  }

  const result = check.data;
  return (
    <section aria-label="DNS check" className="rounded-lg border bg-surface px-4 py-3">
      <div className="flex items-start justify-between gap-3">
        <p
          role="status"
          className={cn("text-sm font-medium", !result.ok && "text-destructive")}
        >
          {domainCheckSummary(result)}
        </p>
        <Button
          type="button"
          variant="ghost"
          size="sm"
          loading={check.isFetching}
          onClick={() => void check.refetch()}
        >
          {check.isFetching ? null : <RefreshCw className="size-3.5" aria-hidden="true" />}
          Check again
        </Button>
      </div>
      <ul className="mt-2 flex flex-col gap-2">
        {result.findings.map((f) => {
          const { Icon, className, label } = levelIcon[f.level];
          return (
            <li key={f.id} className="flex gap-2 text-sm">
              <Icon className={cn("mt-0.5 size-4 shrink-0", className)} aria-label={label} role="img" />
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
