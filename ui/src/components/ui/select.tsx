import * as React from "react";
import { ChevronDown } from "lucide-react";
import { cn } from "@/lib/utils";
import { controlClass } from "./input";

/**
 * Styled native `<select>`: same metrics as `Input`, full keyboard and
 * screen-reader support, and the platform picker on phones.
 */
const Select = React.forwardRef<
  HTMLSelectElement,
  React.SelectHTMLAttributes<HTMLSelectElement>
>(({ className, children, ...props }, ref) => (
  <span className="relative block w-full">
    <select
      ref={ref}
      className={cn(controlClass, "flex h-10 appearance-none py-2 pr-9", className)}
      {...props}
    >
      {children}
    </select>
    <ChevronDown
      className="pointer-events-none absolute right-3 top-1/2 size-4 -translate-y-1/2 text-muted-foreground"
      aria-hidden="true"
    />
  </span>
));
Select.displayName = "Select";

export { Select };
