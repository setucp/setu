import * as React from "react";
import { cn } from "@/lib/utils";

/** Shared metrics for text-like controls (input, select, textarea). */
export const controlClass =
  "w-full rounded-md border border-input bg-card px-3 text-sm text-foreground shadow-xs transition-[border-color,box-shadow] duration-150 placeholder:text-muted-foreground/80 hover:border-muted-foreground/40 focus-visible:border-ring focus-visible:outline-hidden focus-visible:ring-3 focus-visible:ring-ring/25 focus-visible:ring-offset-0 disabled:cursor-not-allowed disabled:opacity-50 aria-invalid:border-destructive aria-invalid:focus-visible:ring-destructive/25";

const Input = React.forwardRef<
  HTMLInputElement,
  React.InputHTMLAttributes<HTMLInputElement>
>(({ className, type, ...props }, ref) => (
  <input
    type={type}
    className={cn(controlClass, "flex h-10 py-2", className)}
    ref={ref}
    {...props}
  />
));
Input.displayName = "Input";

export { Input };
