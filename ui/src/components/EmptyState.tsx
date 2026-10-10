import * as React from "react";
import { cn } from "@/lib/utils";
import { Mark } from "./Mark";

/** Friendly empty list: the brand mark in a soft tile, a line of text, a CTA. */
export function EmptyState({
  title,
  description,
  action,
  icon,
  className,
}: {
  title: string;
  description?: React.ReactNode;
  action?: React.ReactNode;
  icon?: React.ReactNode;
  className?: string;
}) {
  return (
    <div
      className={cn(
        "flex flex-col items-center justify-center rounded-lg border border-dashed bg-card px-6 py-14 text-center",
        className,
      )}
    >
      <div className="relative mb-4 flex size-14 items-center justify-center rounded-xl bg-primary-subtle ring-1 ring-primary/15">
        {icon ?? <Mark className="size-8" />}
      </div>
      <p className="text-sm font-semibold">{title}</p>
      {description !== undefined ? (
        <p className="mt-1 max-w-sm text-sm text-muted-foreground">{description}</p>
      ) : null}
      {action !== undefined ? <div className="mt-5">{action}</div> : null}
    </div>
  );
}
