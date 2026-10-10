import * as React from "react";
import { cn } from "@/lib/utils";

/** Scroll container with thin, themed scrollbars. */
const ScrollArea = React.forwardRef<HTMLDivElement, React.HTMLAttributes<HTMLDivElement>>(
  ({ className, ...props }, ref) => (
    <div ref={ref} className={cn("scrollbar-thin overflow-auto overscroll-contain", className)} {...props} />
  ),
);
ScrollArea.displayName = "ScrollArea";

export { ScrollArea };
