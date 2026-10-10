import * as React from "react";
import { Pause, Play } from "lucide-react";
import { cn } from "@/lib/utils";
import { CopyButton } from "./CopyButton";

/**
 * Terminal-styled log viewer: mono, dark in both themes, follows the tail
 * unless the reader scrolled up. The `<pre>` holds exactly the lines (no
 * decoration) so its text is the log.
 */
export function LogPanel({
  lines,
  text,
  title,
  testId,
  live = false,
  paused,
  onTogglePause,
  emptyText = "Waiting for output…",
  className,
}: {
  lines?: string[];
  text?: string;
  title: string;
  testId?: string;
  live?: boolean;
  paused?: boolean;
  onTogglePause?: () => void;
  emptyText?: string;
  className?: string;
}) {
  const body = text ?? (lines ?? []).join("\n");
  const ref = React.useRef<HTMLPreElement>(null);
  const stick = React.useRef(true);

  React.useEffect(() => {
    const el = ref.current;
    if (el !== null && paused !== true && stick.current) {
      el.scrollTop = el.scrollHeight;
    }
  }, [body, paused]);

  const count = body === "" ? 0 : body.split("\n").length;

  return (
    <section
      aria-label={title}
      className={cn(
        "overflow-hidden rounded-lg border border-slate-800 bg-terminal text-terminal-foreground shadow-card dark:border-border",
        className,
      )}
    >
      <div className="flex flex-wrap items-center gap-2 border-b border-white/10 px-3 py-2">
        <span className="flex gap-1.5" aria-hidden="true">
          <span className="size-2.5 rounded-full bg-white/15" />
          <span className="size-2.5 rounded-full bg-white/15" />
          <span className="size-2.5 rounded-full bg-white/15" />
        </span>
        <span className="ml-1 text-xs font-medium text-terminal-muted">{title}</span>
        {live ? (
          <span className="flex items-center gap-1.5 text-xs text-terminal-muted" aria-live="polite">
            <span
              className={cn(
                "size-1.5 rounded-full",
                paused === true ? "bg-amber-400" : "bg-emerald-400 motion-safe:animate-pulse",
              )}
              aria-hidden="true"
            />
            {paused === true ? "Paused — buffered lines are kept" : "Live"}
          </span>
        ) : null}
        <span className="ml-auto flex items-center gap-1.5">
          <span className="hidden text-xs tabular-nums text-terminal-muted sm:inline">
            {count} {count === 1 ? "line" : "lines"}
          </span>
          {onTogglePause !== undefined ? (
            <button
              type="button"
              onClick={onTogglePause}
              className="touch-target inline-flex h-7 items-center gap-1.5 rounded-md border border-white/10 px-2 text-xs font-medium text-terminal-foreground transition-colors hover:bg-white/10"
            >
              {paused === true ? (
                <Play className="size-3.5" aria-hidden="true" />
              ) : (
                <Pause className="size-3.5" aria-hidden="true" />
              )}
              {paused === true ? "Resume live tail" : "Pause live tail"}
            </button>
          ) : null}
          <CopyButton
            text={body}
            label="Log"
            iconOnly
            className="h-7 w-7 border-white/10 bg-transparent text-terminal-foreground shadow-none hover:bg-white/10 hover:text-terminal-foreground"
          />
        </span>
      </div>
      <pre
        ref={ref}
        data-testid={testId}
        tabIndex={0}
        aria-label={`${title} output`}
        onScroll={(e) => {
          const el = e.currentTarget;
          stick.current = el.scrollHeight - el.scrollTop - el.clientHeight < 24;
        }}
        className="scrollbar-thin max-h-[60dvh] min-h-48 overflow-auto p-4 font-mono text-xs leading-5 whitespace-pre-wrap break-words focus-visible:ring-inset focus-visible:ring-offset-0"
      >
        {body === "" ? <span className="text-terminal-muted">{emptyText}</span> : body}
      </pre>
    </section>
  );
}
