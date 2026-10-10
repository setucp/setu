import { cn } from "@/lib/utils";
import { cva, type VariantProps } from "class-variance-authority";
import * as React from "react";

const badgeVariants = cva(
  "inline-flex shrink-0 items-center gap-1.5 whitespace-nowrap rounded-full border px-2 py-0.5 text-xs font-medium leading-4",
  {
    variants: {
      variant: {
        default: "border-transparent bg-primary-subtle text-link",
        secondary: "border-border bg-muted text-muted-foreground",
        outline: "border-border bg-transparent text-foreground",
        success: "border-success/20 bg-success-subtle text-success",
        warning: "border-warning/25 bg-warning-subtle text-warning",
        destructive: "border-destructive/20 bg-destructive-subtle text-destructive",
        info: "border-primary/20 bg-primary-subtle text-link",
      },
    },
    defaultVariants: {
      variant: "default",
    },
  },
);

const dotColor: Record<string, string> = {
  default: "bg-link",
  secondary: "bg-muted-foreground",
  outline: "bg-muted-foreground",
  success: "bg-success",
  warning: "bg-warning",
  destructive: "bg-destructive",
  info: "bg-link",
};

export interface BadgeProps
  extends React.HTMLAttributes<HTMLSpanElement>,
    VariantProps<typeof badgeVariants> {
  /** A small leading status dot. */
  dot?: boolean;
}

function Badge({ className, variant, dot = false, children, ...props }: BadgeProps) {
  const v = variant ?? "default";
  return (
    <span data-variant={v} className={cn(badgeVariants({ variant }), className)} {...props}>
      {dot ? (
        <span className={cn("size-1.5 rounded-full", dotColor[v])} aria-hidden="true" />
      ) : null}
      {children}
    </span>
  );
}

export { Badge, badgeVariants };
