import * as React from "react";
import { api, type SystemUnit } from "@/lib/client";
import { ErrorText } from "./ErrorText";
import { LogPanel } from "./LogPanel";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "./ui/card";
import { Tabs, TabsList, TabsTrigger } from "./ui/tabs";

/** How often the open journal is read again. */
export const SERVER_LOG_POLL_MS = 3000;
/** Lines kept on screen; older ones drop off the top. */
export const SERVER_LOG_KEEP = 2000;

const UNITS: Array<{ unit: SystemUnit; label: string }> = [
  { unit: "caddy", label: "Caddy" },
  { unit: "agent", label: "Agent" },
];

/**
 * The journal of one host service, read page by page: the first read
 * returns the tail and a cursor, each later read passes the cursor back
 * and appends only the new lines. Pausing stops the reads; resuming goes
 * on from the same cursor, so nothing is missed or shown twice.
 */
export function useServerJournal(serverId: string, unit: SystemUnit, live: boolean) {
  const [lines, setLines] = React.useState<string[]>([]);
  const [error, setError] = React.useState<unknown>(null);
  const cursor = React.useRef<string | null>(null);

  // A new server or unit starts a fresh journal.
  React.useEffect(() => {
    cursor.current = null;
    setLines([]);
    setError(null);
  }, [serverId, unit]);

  React.useEffect(() => {
    if (!live) return;
    let stopped = false;
    let timer: ReturnType<typeof setTimeout> | undefined;
    async function read() {
      try {
        const page = await api.serverLogs(serverId, unit, cursor.current);
        if (stopped) return;
        cursor.current = page.cursor ?? cursor.current;
        setError(null);
        if (page.lines.length > 0) {
          setLines((prev) => {
            const next = prev.concat(page.lines);
            return next.length > SERVER_LOG_KEEP ? next.slice(next.length - SERVER_LOG_KEEP) : next;
          });
        }
      } catch (err) {
        if (stopped) return;
        setError(err);
      }
      if (!stopped) timer = setTimeout(() => void read(), SERVER_LOG_POLL_MS);
    }
    void read();
    return () => {
      stopped = true;
      if (timer !== undefined) clearTimeout(timer);
    };
  }, [serverId, unit, live]);

  return { lines, error };
}

/** Server detail card: Caddy's and the agent's journal on that host, with pause and copy. */
export function ServerLogs({ serverId, serverName }: { serverId: string; serverName: string }) {
  const [unit, setUnit] = React.useState<SystemUnit>("caddy");
  const [paused, setPaused] = React.useState(false);
  const { lines, error } = useServerJournal(serverId, unit, !paused);
  const label = UNITS.find((u) => u.unit === unit)?.label ?? unit;

  return (
    <Card>
      <CardHeader>
        <CardTitle>Service logs</CardTitle>
        <CardDescription>
          The systemd journal of Caddy and the Setu agent on {serverName}, read every few seconds.
        </CardDescription>
      </CardHeader>
      <CardContent className="flex flex-col gap-3">
        <Tabs value={unit} onValueChange={(v) => setUnit(v as SystemUnit)}>
          <TabsList>
            {UNITS.map((u) => (
              <TabsTrigger key={u.unit} value={u.unit}>
                {u.label}
              </TabsTrigger>
            ))}
          </TabsList>
        </Tabs>
        <ErrorText error={error} />
        <LogPanel
          title={`${label} journal`}
          testId="server-logs"
          lines={lines}
          live
          paused={paused}
          onTogglePause={() => setPaused((p) => !p)}
          emptyText={error === null ? "Reading the journal…" : "No lines read."}
        />
      </CardContent>
    </Card>
  );
}
