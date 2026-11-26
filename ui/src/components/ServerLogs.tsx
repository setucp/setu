import * as React from "react";
import { useMe } from "@/lib/api";
import { api, ApiError, type SystemUnit } from "@/lib/client";
import { redirectOnUnauthorised } from "@/lib/session";
import { ErrorText } from "./ErrorText";
import { Button } from "./ui/button";
import { LogPanel } from "./LogPanel";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "./ui/card";
import { Tabs, TabsList, TabsTrigger } from "./ui/tabs";

/** How often the open journal is read again. */
export const SERVER_LOG_POLL_MS = 3000;
/** The longest wait between reads after the agent keeps failing. */
export const SERVER_LOG_MAX_BACKOFF_MS = 30_000;
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
 *
 * A request the server refuses (4xx: an agent too old for the journal
 * endpoint, a role without access) is not retried; an expired session goes
 * to the sign-in page. A failing agent (5xx, network) is read again with a
 * doubling wait, and nothing is read while the browser tab is hidden.
 */
export function useServerJournal(serverId: string, unit: SystemUnit, live: boolean) {
  const [lines, setLines] = React.useState<string[]>([]);
  const [error, setError] = React.useState<unknown>(null);
  const cursor = React.useRef<string | null>(null);
  // Set when the server refused a read (4xx): polling has stopped until `restart`.
  const [halted, setHalted] = React.useState(false);
  const [attempt, setAttempt] = React.useState(0);

  // A new server or unit, or a restart after a refusal, starts a fresh journal.
  React.useEffect(() => {
    cursor.current = null;
    setLines([]);
    setError(null);
    setHalted(false);
  }, [serverId, unit, attempt]);

  React.useEffect(() => {
    if (!live) return;
    let stopped = false;
    let timer: ReturnType<typeof setTimeout> | undefined;
    let wait = SERVER_LOG_POLL_MS;
    let waitingForTab = false;
    function schedule() {
      if (stopped) return;
      if (document.hidden) {
        waitingForTab = true;
        return;
      }
      timer = setTimeout(() => void read(), wait);
    }
    function onVisible() {
      if (!document.hidden && waitingForTab && !stopped) {
        waitingForTab = false;
        void read();
      }
    }
    document.addEventListener("visibilitychange", onVisible);
    async function read() {
      try {
        const page = await api.serverLogs(serverId, unit, cursor.current);
        if (stopped) return;
        cursor.current = page.cursor ?? cursor.current;
        wait = SERVER_LOG_POLL_MS;
        setError(null);
        // A read that works again (say, after a pause and resume) clears a refusal.
        setHalted(false);
        if (page.lines.length > 0) {
          setLines((prev) => {
            const next = prev.concat(page.lines);
            return next.length > SERVER_LOG_KEEP ? next.slice(next.length - SERVER_LOG_KEEP) : next;
          });
        }
      } catch (err) {
        if (stopped) return;
        setError(err);
        if (redirectOnUnauthorised(err)) return;
        if (err instanceof ApiError && err.status >= 400 && err.status < 500) {
          setHalted(true);
          return;
        }
        wait = Math.min(wait * 2, SERVER_LOG_MAX_BACKOFF_MS);
      }
      schedule();
    }
    void read();
    return () => {
      stopped = true;
      document.removeEventListener("visibilitychange", onVisible);
      if (timer !== undefined) clearTimeout(timer);
    };
  }, [serverId, unit, live, attempt]);

  /** Read again from the tail after a refusal. */
  const restart = React.useCallback(() => setAttempt((n) => n + 1), []);
  return { lines, error, halted, restart };
}

/** Server detail card: Caddy's and the agent's journal on that host, with pause and copy. */
export function ServerLogs({ serverId, serverName }: { serverId: string; serverName: string }) {
  const me = useMe();
  // The journal endpoint is operator-only: a viewer is told so, and no
  // request is made.
  if (me.data === undefined) return null;
  if (me.data.role === "viewer") {
    return (
      <Card>
        <CardHeader>
          <CardTitle>Service logs</CardTitle>
          <CardDescription>
            The journal of Caddy and the Setu agent on {serverName} is shown to operators and
            owners.
          </CardDescription>
        </CardHeader>
      </Card>
    );
  }
  return <ServerJournalCard serverId={serverId} serverName={serverName} />;
}

function ServerJournalCard({ serverId, serverName }: { serverId: string; serverName: string }) {
  const [unit, setUnit] = React.useState<SystemUnit>("caddy");
  const [paused, setPaused] = React.useState(false);
  const { lines, error, halted, restart } = useServerJournal(serverId, unit, !paused);
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
        <div className="flex items-start justify-between gap-3">
          <ErrorText error={error} />
          {halted ? (
            <Button type="button" variant="outline" size="sm" onClick={restart}>
              Try again
            </Button>
          ) : null}
        </div>
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
