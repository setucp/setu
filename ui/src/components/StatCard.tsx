import * as React from "react";
import { cn } from "@/lib/utils";

/** One figure in a stat row: label, value, optional footnote. */
export function StatCard({
  label,
  value,
  hint,
  icon,
  className,
}: {
  label: string;
  value: React.ReactNode;
  hint?: React.ReactNode;
  icon?: React.ReactNode;
  className?: string;
}) {
  return (
    <div className={cn("rounded-lg border bg-card p-4 shadow-card", className)}>
      <dt className="flex items-center gap-1.5 text-xs font-medium text-muted-foreground [&_svg]:size-3.5">
        {icon}
        {label}
      </dt>
      <dd className="mt-1.5 truncate text-lg font-semibold tabular-nums tracking-tight">{value}</dd>
      {hint !== undefined ? (
        <dd className="mt-0.5 truncate text-xs text-muted-foreground">{hint}</dd>
      ) : null}
    </div>
  );
}
