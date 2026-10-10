import { cn } from "@/lib/utils";

/** The Setu bridge mark (docs/brand), drawn in the accent colour. */
export function Mark({ className, title }: { className?: string; title?: string }) {
  return (
    <svg
      viewBox="0 0 64 64"
      fill="none"
      stroke="currentColor"
      strokeLinecap="round"
      strokeLinejoin="round"
      className={cn("text-primary", className)}
      role={title === undefined ? undefined : "img"}
      aria-hidden={title === undefined ? true : undefined}
      aria-label={title}
    >
      <path d="M10 42 H54" strokeWidth="6" />
      <path d="M10 42 C18 16 46 16 54 42" strokeWidth="6" />
      <path d="M22 42 V27.5 M32 42 V22.5 M42 42 V27.5" strokeWidth="4" />
    </svg>
  );
}

/** Full logo lockup: the inline mark plus the wordmark as text, so it
 * follows the theme (currentColor) and uses the page's Inter webfont. */
export function Logo({ className }: { className?: string }) {
  return (
    <span className={cn("inline-flex items-center gap-2", className)} aria-label="Setu" role="img">
      <Mark className="size-7 shrink-0" />
      <span className="text-[1.15rem] font-semibold tracking-[-0.03em] text-foreground">Setu</span>
    </span>
  );
}
