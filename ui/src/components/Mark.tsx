import { cn } from "@/lib/utils";

/** The Setu keystone mark (docs/brand): a chevron span over its base, in the accent colour. */
export function Mark({ className, title }: { className?: string; title?: string }) {
  return (
    <svg
      viewBox="0 0 64 64"
      fill="currentColor"
      className={cn("text-primary", className)}
      role={title === undefined ? undefined : "img"}
      aria-hidden={title === undefined ? true : undefined}
      aria-label={title}
    >
      <path d="M32 12 L58 38 H46 L32 24 L18 38 H6 Z" stroke="currentColor" strokeWidth="3" strokeLinejoin="round" />
      <rect x="6" y="44" width="52" height="8" rx="4" />
    </svg>
  );
}

/** Full logo lockup: the inline mark plus the wordmark as text, so it
 * follows the theme (currentColor) and uses the page's Inter webfont. */
export function Logo({ className }: { className?: string }) {
  return (
    <span className={cn("inline-flex items-center gap-2", className)} aria-label="Setu" role="img">
      <Mark className="size-7 shrink-0" />
      <span className="font-brand text-[1.2rem] font-bold tracking-[-0.02em] text-foreground">Setu</span>
    </span>
  );
}
