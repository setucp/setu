import * as React from "react";
import { useQueryClient } from "@tanstack/react-query";
import { RefreshCw } from "lucide-react";
import { toast } from "sonner";
import { useCertificate, useJob, useMe, useRetryCertificate } from "@/lib/api";
import { ApiError, type CertState, type SiteDetail } from "@/lib/client";
import { absoluteTime } from "@/lib/time";
import { AppLink } from "./AppLink";
import { ErrorText } from "./ErrorText";
import { RelativeTime } from "./RelativeTime";
import { StatusBadge, jobVariant } from "./StatusBadge";
import { Badge } from "./ui/badge";
import { Button } from "./ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "./ui/card";
import { SkeletonRows } from "./ui/skeleton";

const stateBadge: Record<CertState, { label: string; variant: "success" | "warning" | "destructive" | "secondary" }> = {
  valid: { label: "valid", variant: "success" },
  expiring: { label: "expiring soon", variant: "warning" },
  expired: { label: "expired", variant: "destructive" },
  pending: { label: "pending", variant: "warning" },
  unknown: { label: "unknown", variant: "secondary" },
};

/** "in 63 days", "today", "3 days ago" from the API's whole-day count. */
export function daysLeftText(days: number): string {
  if (days === 0) return "today";
  if (days > 0) return days === 1 ? "in 1 day" : `in ${days} days`;
  return days === -1 ? "1 day ago" : `${-days} days ago`;
}

/** The retry API accepts a site that is running or stopped (409 otherwise). */
function canRetry(status: string): boolean {
  return status === "running" || status === "stopped";
}

/**
 * Overview card: the TLS certificate Caddy serves for the site's domain
 * (state, issuer, expiry, when it was read) and, for operators, a button
 * that makes Caddy try again now. The retry is a route job; the card
 * follows it and reads the certificate again once it ends.
 */
export function SiteCertificate({ site }: { site: SiteDetail }) {
  const cert = useCertificate(site.id);
  const me = useMe();
  const retry = useRetryCertificate(site.id);
  const [jobId, setJobId] = React.useState<string | null>(null);
  const canEdit = me.data !== undefined && me.data.role !== "viewer";

  function onRetry() {
    retry.mutate(undefined, {
      onSuccess: (job) => {
        setJobId(job.id);
        toast.success(`Caddy retries the certificate for ${site.domain}`);
      },
      onError: (err) => {
        toast.error(err instanceof ApiError ? err.message : "Retry failed");
      },
    });
  }

  const c = cert.data;
  const badge = c === undefined ? null : stateBadge[c.state];
  return (
    <Card className="self-start">
      <CardHeader>
        <CardTitle>Certificate</CardTitle>
        <CardDescription>The TLS certificate Caddy serves for {site.domain}.</CardDescription>
      </CardHeader>
      <CardContent className="flex flex-col gap-3">
        {cert.isError ? (
          <ErrorText error={cert.error} />
        ) : c === undefined ? (
          <SkeletonRows rows={2} label="Reading the certificate…" />
        ) : (
          <>
            <dl className="grid grid-cols-[100px_minmax(0,1fr)] gap-x-4 gap-y-2 text-sm">
              <dt className="text-muted-foreground">State</dt>
              <dd>
                <Badge variant={badge?.variant ?? "secondary"} dot>
                  {badge?.label ?? c.state}
                </Badge>
              </dd>
              {c.issuer ? (
                <>
                  <dt className="text-muted-foreground">Issuer</dt>
                  <dd className="break-words">{c.issuer}</dd>
                </>
              ) : null}
              {c.not_after ? (
                <>
                  <dt className="text-muted-foreground">Expires</dt>
                  <dd>
                    <time dateTime={c.not_after} title={absoluteTime(c.not_after)}>
                      {new Date(c.not_after).toLocaleDateString()}
                    </time>
                    {c.days_left !== null && c.days_left !== undefined ? (
                      <span className="text-muted-foreground"> ({daysLeftText(c.days_left)})</span>
                    ) : null}
                  </dd>
                </>
              ) : null}
              <dt className="text-muted-foreground">Checked</dt>
              <dd>
                <RelativeTime iso={c.checked_at} />
              </dd>
            </dl>
            {c.error ? (
              <p className="rounded-md bg-muted/60 px-3 py-2 text-xs break-words text-muted-foreground">
                {c.error}
              </p>
            ) : null}
          </>
        )}
        {canEdit ? (
          <div className="flex flex-col gap-2 border-t pt-3">
            <div className="flex flex-wrap items-center gap-2">
              <Button
                type="button"
                variant="outline"
                size="sm"
                loading={retry.isPending}
                disabled={!canRetry(site.status)}
                onClick={onRetry}
              >
                {retry.isPending ? null : <RefreshCw aria-hidden="true" />}
                Retry now
              </Button>
              {jobId !== null ? <RetryJob id={jobId} siteId={site.id} /> : null}
            </div>
            <p className="text-xs text-muted-foreground">
              {canRetry(site.status)
                ? "Caddy reloads the site's route and asks for the certificate again."
                : `Available once the site is running or stopped (it is ${site.status}).`}
            </p>
          </div>
        ) : null}
      </CardContent>
    </Card>
  );
}

/** The retry's route job: its status, a link to its log, and a fresh read when it ends. */
function RetryJob({ id, siteId }: { id: string; siteId: string }) {
  const job = useJob(id);
  const queryClient = useQueryClient();
  const status = job.data?.status;
  React.useEffect(() => {
    if (status === "done" || status === "failed") {
      void queryClient.invalidateQueries({ queryKey: ["sites", siteId, "certificate"] });
    }
  }, [status, siteId, queryClient]);
  return (
    <span className="flex items-center gap-1.5 text-xs">
      <AppLink href={`/jobs/${id}`} className="text-link hover:underline">
        Retry job
      </AppLink>
      {status !== undefined ? <StatusBadge status={status} variants={jobVariant} /> : null}
    </span>
  );
}
