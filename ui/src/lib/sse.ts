import * as React from "react";

/**
 * Subscribes to a newline SSE endpoint (`data: <line>` events) and returns
 * the lines received so far. The subscription closes when `enabled` turns
 * false or the URL changes; buffered lines are kept so a pause/resume
 * cycle does not lose what is already on screen.
 */
export function useSseLines(url: string | null, enabled = true): string[] {
  const [lines, setLines] = React.useState<string[]>([]);

  React.useEffect(() => {
    if (url === null || !enabled) return;
    const source = new EventSource(url);
    source.onmessage = (event: MessageEvent<string>) => {
      const line = event.data;
      setLines((prev) => [...prev, line]);
    };
    return () => {
      source.close();
    };
  }, [url, enabled]);

  return lines;
}
