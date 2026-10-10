import * as React from "react";
import { useSseLines } from "@/lib/sse";
import { Button } from "./ui/button";

/** Live site logs over SSE with auto-scroll and a pause control. */
export function SiteLogs({ siteId }: { siteId: string }) {
  const [paused, setPaused] = React.useState(false);
  const lines = useSseLines(`/api/v1/sites/${encodeURIComponent(siteId)}/logs`, !paused);
  const scrollRef = React.useRef<HTMLPreElement>(null);

  React.useEffect(() => {
    const el = scrollRef.current;
    if (el !== null && !paused) {
      el.scrollTop = el.scrollHeight;
    }
  }, [lines, paused]);

  return (
    <div className="flex flex-col gap-3">
      <div className="flex items-center gap-2">
        <Button
          type="button"
          variant="outline"
          size="sm"
          onClick={() => setPaused((p) => !p)}
        >
          {paused ? "Resume live tail" : "Pause live tail"}
        </Button>
        <span className="text-xs text-muted-foreground">
          {paused ? "Paused — buffered lines are kept." : "Streaming new lines…"}
        </span>
      </div>
      <pre
        ref={scrollRef}
        data-testid="site-logs"
        className="max-h-[60dvh] overflow-auto rounded-md border bg-muted/40 p-4 font-mono text-xs whitespace-pre-wrap"
      >
        {lines.length === 0 ? "Waiting for log lines…" : lines.join("\n")}
      </pre>
    </div>
  );
}
