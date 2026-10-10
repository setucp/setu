import { cn } from "@/lib/utils";

/**
 * Horizontal meter. Turns amber past 75 % and red past 90 % (a usage bar
 * that is nearly full is the one place red marks a failure-in-waiting).
 */
function Progress({
  value,
  max = 100,
  label,
  className,
  tone = "auto",
}: {
  value: number;
  max?: number;
  /** Accessible name, e.g. "Memory 128 of 512 MB". */
  label: string;
  className?: string;
  tone?: "auto" | "primary";
}) {
  const pct = max > 0 ? Math.min(100, Math.max(0, (value / max) * 100)) : 0;
  const color =
    tone === "primary" ? "bg-primary" : pct >= 90 ? "bg-destructive" : pct >= 75 ? "bg-warning" : "bg-primary";
  return (
    <div
      role="progressbar"
      aria-label={label}
      aria-valuemin={0}
      aria-valuemax={max}
      aria-valuenow={Math.round(value)}
      className={cn("h-1.5 w-full overflow-hidden rounded-full bg-muted", className)}
    >
      <div
        className={cn("h-full rounded-full transition-[width] duration-500", color)}
        style={{ width: `${pct.toFixed(1)}%` }}
      />
    </div>
  );
}

export { Progress };
