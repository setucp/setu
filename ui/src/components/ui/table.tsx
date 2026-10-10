import * as React from "react";
import { cn } from "@/lib/utils";

type Density = "comfortable" | "compact";
const DensityContext = React.createContext<Density>("comfortable");

/**
 * Data table: framed card, sticky header, row hover, no zebra stripes.
 * `density="compact"` tightens rows for long lists (audit, jobs).
 */
const Table = React.forwardRef<
  HTMLTableElement,
  React.HTMLAttributes<HTMLTableElement> & { density?: Density; frame?: boolean }
>(({ className, density = "comfortable", frame = true, ...props }, ref) => (
  <DensityContext.Provider value={density}>
    <div
      className={cn(
        "scrollbar-thin relative w-full overflow-auto",
        frame && "rounded-lg border bg-card shadow-card",
      )}
    >
      <table
        ref={ref}
        className={cn("w-full caption-bottom border-collapse text-sm", className)}
        {...props}
      />
    </div>
  </DensityContext.Provider>
));
Table.displayName = "Table";

const TableHeader = React.forwardRef<
  HTMLTableSectionElement,
  React.HTMLAttributes<HTMLTableSectionElement>
>(({ className, ...props }, ref) => (
  <thead
    ref={ref}
    className={cn("sticky top-0 z-10 bg-surface [&_tr]:border-b", className)}
    {...props}
  />
));
TableHeader.displayName = "TableHeader";

const TableBody = React.forwardRef<
  HTMLTableSectionElement,
  React.HTMLAttributes<HTMLTableSectionElement>
>(({ className, ...props }, ref) => (
  <tbody ref={ref} className={cn("[&_tr:last-child]:border-0", className)} {...props} />
));
TableBody.displayName = "TableBody";

const TableRow = React.forwardRef<
  HTMLTableRowElement,
  React.HTMLAttributes<HTMLTableRowElement>
>(({ className, ...props }, ref) => (
  <tr
    ref={ref}
    className={cn(
      "border-b transition-colors hover:bg-muted/50 data-[state=selected]:bg-muted",
      className,
    )}
    {...props}
  />
));
TableRow.displayName = "TableRow";

const TableHead = React.forwardRef<
  HTMLTableCellElement,
  React.ThHTMLAttributes<HTMLTableCellElement>
>(({ className, ...props }, ref) => {
  const density = React.useContext(DensityContext);
  return (
    <th
      ref={ref}
      scope="col"
      className={cn(
        "whitespace-nowrap px-4 text-left align-middle text-xs font-medium text-muted-foreground",
        density === "compact" ? "h-9" : "h-10",
        className,
      )}
      {...props}
    />
  );
});
TableHead.displayName = "TableHead";

const TableCell = React.forwardRef<
  HTMLTableCellElement,
  React.TdHTMLAttributes<HTMLTableCellElement>
>(({ className, ...props }, ref) => {
  const density = React.useContext(DensityContext);
  return (
    <td
      ref={ref}
      className={cn("px-4 align-middle", density === "compact" ? "py-2" : "py-3", className)}
      {...props}
    />
  );
});
TableCell.displayName = "TableCell";

export { Table, TableHeader, TableBody, TableRow, TableHead, TableCell };
