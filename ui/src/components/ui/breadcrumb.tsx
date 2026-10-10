import * as React from "react";
import { ChevronRight } from "lucide-react";
import { cn } from "@/lib/utils";

function Breadcrumb({ className, children }: { className?: string; children: React.ReactNode }) {
  return (
    <nav aria-label="Breadcrumb" className={cn("min-w-0", className)}>
      <ol className="flex min-w-0 items-center gap-1.5 text-sm">{children}</ol>
    </nav>
  );
}

function BreadcrumbItem({
  children,
  current = false,
  last = false,
}: {
  children: React.ReactNode;
  current?: boolean;
  last?: boolean;
}) {
  return (
    <li className="flex min-w-0 items-center gap-1.5">
      <span
        className={cn(
          "truncate",
          current ? "font-medium text-foreground" : "text-muted-foreground",
        )}
        aria-current={current ? "page" : undefined}
      >
        {children}
      </span>
      {last ? null : (
        <ChevronRight className="size-3.5 shrink-0 text-muted-foreground/60" aria-hidden="true" />
      )}
    </li>
  );
}

export { Breadcrumb, BreadcrumbItem };
