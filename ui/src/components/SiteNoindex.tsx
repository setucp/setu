import { EyeOff, Eye } from "lucide-react";
import { toast } from "sonner";
import { useMe, useNoindex, useSetNoindex } from "@/lib/api";
import { ApiError, type SiteDetail } from "@/lib/client";
import { ErrorText } from "./ErrorText";
import { Alert } from "./ui/alert";
import { Badge } from "./ui/badge";
import { Button } from "./ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "./ui/card";
import { useConfirm } from "./ui/dialog";
import { SkeletonRows } from "./ui/skeleton";

/** A site may change its route only while it is running or stopped. */
function settled(status: string) {
  return status === "running" || status === "stopped";
}

/**
 * Per-site noindex (`/sites/{id}/noindex`): when on, the route sends
 * `X-Robots-Tag: noindex, nofollow` on every response and answers
 * `/robots.txt` with a disallow-all file, before basic auth and the app.
 * Meant for staging sites, so turning it on asks first. A change starts a
 * route update job (reported through `onSubmitted`).
 */
export function SiteNoindex({
  site,
  onSubmitted,
}: {
  site: SiteDetail;
  onSubmitted?: (jobId: string) => void;
}) {
  const noindex = useNoindex(site.id);
  const me = useMe();
  const set = useSetNoindex(site.id);
  const confirm = useConfirm();

  if (noindex.isError) return <ErrorText error={noindex.error} />;
  if (noindex.data === undefined) return <SkeletonRows rows={2} label="Loading search engine setting…" />;

  const on = noindex.data;
  const canEdit = me.data !== undefined && me.data.role !== "viewer";
  const busy = !settled(site.status);

  async function toggle() {
    if (!on) {
      const ok = await confirm({
        title: `Hide ${site.name} from search engines?`,
        description: `Search engines are asked not to index ${site.domain} or follow its links, and drop pages they already have. Meant for staging sites.`,
        confirmLabel: "Hide from search engines",
        destructive: true,
      });
      if (!ok) return;
    }
    set.mutate(!on, {
      onSuccess: (res) => {
        toast.success(
          res.enabled
            ? `${site.domain} asks search engines to stay away`
            : `Search engines may index ${site.domain}`,
        );
        onSubmitted?.(res.job.id);
      },
      onError: (err) => {
        toast.error(err instanceof ApiError ? err.message : "Update failed");
      },
    });
  }

  return (
    <Card>
      <CardHeader>
        <CardTitle className="flex items-center gap-2">
          Search engines
          <Badge variant={on ? "warning" : "secondary"} dot>
            {on ? "hidden" : "allowed"}
          </Badge>
        </CardTitle>
        <CardDescription>
          {on ? (
            <>
              {site.domain} sends <code className="text-foreground">X-Robots-Tag: noindex, nofollow</code>{" "}
              and a <code className="text-foreground">/robots.txt</code> that disallows everything; the
              app cannot override them.
            </>
          ) : (
            <>Search engines may index {site.domain}. Hide a staging copy of a site from them.</>
          )}
        </CardDescription>
      </CardHeader>
      {canEdit ? (
        <CardContent className="flex flex-col gap-3">
          {busy ? (
            <Alert variant="warning">
              The site is {site.status}; this can change once it is running or stopped.
            </Alert>
          ) : null}
          <div>
            <Button
              type="button"
              variant="outline"
              disabled={busy}
              loading={set.isPending}
              onClick={() => void toggle()}
            >
              {on ? <Eye aria-hidden="true" /> : <EyeOff aria-hidden="true" />}
              {on ? "Allow search engines" : "Hide from search engines"}
            </Button>
          </div>
        </CardContent>
      ) : null}
    </Card>
  );
}
