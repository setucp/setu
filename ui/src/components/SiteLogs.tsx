import * as React from "react";
import { useSseLines } from "@/lib/sse";
import { LogPanel } from "./LogPanel";

/** Live site logs (journal tail over SSE) with auto-scroll, pause and copy. */
export function SiteLogs({ siteId }: { siteId: string }) {
  const [paused, setPaused] = React.useState(false);
  const lines = useSseLines(`/api/v1/sites/${encodeURIComponent(siteId)}/logs`, !paused);
  return (
    <LogPanel
      title="Site log"
      testId="site-logs"
      lines={lines}
      live
      paused={paused}
      onTogglePause={() => setPaused((p) => !p)}
      emptyText="Waiting for log lines…"
    />
  );
}
