import { cn } from "@/lib/utils";

/** Up to two initials from an email or name: `ada.lovelace@x` → "AL". */
export function initials(source: string): string {
  const local = source.split("@")[0] ?? source;
  const parts = local.split(/[^A-Za-z0-9]+/).filter((p) => p !== "");
  const letters =
    parts.length >= 2 ? `${parts[0]?.[0] ?? ""}${parts[1]?.[0] ?? ""}` : local.slice(0, 2);
  return letters.toUpperCase() || "?";
}

function Avatar({ name, className }: { name: string; className?: string }) {
  return (
    <span
      aria-hidden="true"
      className={cn(
        "inline-flex size-8 shrink-0 select-none items-center justify-center rounded-full bg-primary-subtle text-xs font-semibold text-link ring-1 ring-primary/15",
        className,
      )}
    >
      {initials(name)}
    </span>
  );
}

export { Avatar };
