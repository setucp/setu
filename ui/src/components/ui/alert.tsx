import * as React from "react";
import { cva, type VariantProps } from "class-variance-authority";
import { AlertTriangle, CheckCircle2, Info, XCircle } from "lucide-react";
import { cn } from "@/lib/utils";

const alertVariants = cva(
  "relative flex gap-3 rounded-lg border px-4 py-3 text-sm [&>svg]:mt-0.5 [&>svg]:size-4 [&>svg]:shrink-0",
  {
    variants: {
      variant: {
        info: "border-primary/20 bg-primary-subtle text-foreground [&>svg]:text-link",
        success: "border-success/25 bg-success-subtle text-foreground [&>svg]:text-success",
        warning: "border-warning/30 bg-warning-subtle text-foreground [&>svg]:text-warning",
        destructive:
          "border-destructive/25 bg-destructive-subtle text-foreground [&>svg]:text-destructive",
      },
    },
    defaultVariants: { variant: "info" },
  },
);

const icons = {
  info: Info,
  success: CheckCircle2,
  warning: AlertTriangle,
  destructive: XCircle,
} as const;

export interface AlertProps
  extends Omit<React.HTMLAttributes<HTMLDivElement>, "title">,
    VariantProps<typeof alertVariants> {
  title?: React.ReactNode;
}

/**
 * Inline callout. Destructive alerts get `role="alert"` (announced at once);
 * the rest are plain regions unless the caller passes a role.
 */
function Alert({ className, variant, title, children, role, ...props }: AlertProps) {
  const v = variant ?? "info";
  const Icon = icons[v];
  return (
    <div
      role={role ?? (v === "destructive" ? "alert" : undefined)}
      className={cn(alertVariants({ variant }), className)}
      {...props}
    >
      <Icon aria-hidden="true" />
      <div className="min-w-0 flex-1 [&_a]:font-medium [&_a]:text-link [&_a]:underline-offset-2 hover:[&_a]:underline">
        {title !== undefined ? <p className="font-medium leading-5">{title}</p> : null}
        {children !== undefined ? (
          <div className={cn("leading-5", title !== undefined && "mt-0.5 text-muted-foreground")}>
            {children}
          </div>
        ) : null}
      </div>
    </div>
  );
}

export { Alert };
