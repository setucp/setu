import * as React from "react";

/** Cap on kept lines; the oldest go first. */
const MAX_LINES = 10_000;

/**
 * Merges lines from successive SSE connections into one buffer without
 * duplicates. Both log streams replay on every (re)connect: the job log
 * resends its whole backlog, site logs resend the journal tail. A new
 * connection's lines are therefore held while they still match a run of
 * lines already in the buffer (they are on screen already); once the run
 * reaches the end of the buffer the replay is over, and once no run
 * matches any more the held lines were new after all and are appended.
 * Repeated identical lines (`GET / 200`) are kept: matching is by
 * position, not by value.
 */
export class LineMerger {
  lines: string[] = [];
  private replay: {
    candidates: number[] | null;
    held: string[];
    matched: number;
  } | null = null;

  /** Call when a connection (re)opens. */
  connect() {
    this.replay = this.lines.length === 0 ? null : { candidates: null, held: [], matched: 0 };
  }

  /** Returns true when the visible buffer changed. */
  push(line: string): boolean {
    const replay = this.replay;
    if (replay === null) {
      this.append([line]);
      return true;
    }
    const len = this.lines.length;
    const k = replay.held.length; // offset of `line` within a candidate run
    replay.held.push(line);
    const previous =
      replay.candidates ?? Array.from({ length: len }, (_, index) => index);
    const alive: number[] = [];
    for (const start of previous) {
      if (start + k < len && this.lines[start + k] === line) {
        if (start + k + 1 === len) {
          // This run covers the buffer's tail: everything held so far
          // was a replay.
          replay.matched = replay.held.length;
        } else {
          alive.push(start);
        }
      }
    }
    replay.candidates = alive;
    if (alive.length > 0) return false;
    // No run can extend: what follows the longest replayed tail is new.
    const fresh = replay.held.slice(replay.matched);
    this.replay = null;
    if (fresh.length === 0) return false;
    this.append(fresh);
    return true;
  }

  private append(fresh: string[]) {
    this.lines.push(...fresh);
    if (this.lines.length > MAX_LINES) {
      this.lines.splice(0, this.lines.length - MAX_LINES);
    }
  }
}

/**
 * Subscribes to a newline SSE endpoint (`data: <line>` events) and returns
 * the lines received so far, without the duplicates a reconnect replays
 * (see `LineMerger`). The subscription closes when `enabled` turns false
 * or the URL changes; buffered lines are kept so a pause/resume cycle
 * does not lose what is already on screen.
 */
export function useSseLines(url: string | null, enabled = true): string[] {
  const merger = React.useRef<{ url: string | null; m: LineMerger } | null>(null);
  const [lines, setLines] = React.useState<string[]>([]);

  React.useEffect(() => {
    if (url === null || !enabled) return;
    // Another stream (a different site or job): start a fresh buffer.
    if (merger.current === null || merger.current.url !== url) {
      merger.current = { url, m: new LineMerger() };
      setLines([]);
    }
    const m = merger.current.m;
    const source = new EventSource(url);
    m.connect();
    let opened = false;
    source.onopen = () => {
      // The first open belongs to the connect() above; later ones are
      // the browser's automatic reconnects, which replay.
      if (opened) m.connect();
      opened = true;
    };
    source.onmessage = (event: MessageEvent<string>) => {
      opened = true;
      if (m.push(event.data)) setLines([...m.lines]);
    };
    // The job log ends with `event: end` once the job finished: stop
    // instead of letting the browser reconnect and replay it.
    source.addEventListener("end", () => {
      source.close();
    });
    return () => {
      source.close();
    };
  }, [url, enabled]);

  return lines;
}
