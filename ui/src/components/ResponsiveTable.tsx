import * as React from "react";
import { useIsPhone } from "@/lib/media";
import { cn } from "@/lib/utils";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "./ui/table";

export interface Column<T> {
  id: string;
  header: React.ReactNode;
  cell: (row: T) => React.ReactNode;
  /** Cell classes in table mode. */
  className?: string;
  headClassName?: string;
  /** Card mode: omit this column (it is already in the card's title/actions). */
  hideOnCard?: boolean;
  /** Card mode label; defaults to `header` when that is a string. */
  cardLabel?: string;
}

/**
 * One data set, two layouts: a table from 768 px up, a list of cards on
 * phones. Each card shows `cardTitle` (or the first column) prominently,
 * the remaining columns as label/value pairs, and `cardActions` below.
 */
export function ResponsiveTable<T>({
  rows,
  columns,
  getKey,
  label,
  cardTitle,
  cardActions,
  density,
  rowProps,
}: {
  rows: T[];
  columns: Column<T>[];
  getKey: (row: T) => string;
  /** Accessible name for the table / list. */
  label: string;
  cardTitle?: (row: T) => React.ReactNode;
  cardActions?: (row: T) => React.ReactNode;
  density?: "comfortable" | "compact";
  rowProps?: (row: T) => React.HTMLAttributes<HTMLTableRowElement>;
}) {
  const phone = useIsPhone();

  if (phone) {
    const body = cardTitle === undefined ? columns.slice(1) : columns;
    const title = cardTitle ?? ((row: T) => columns[0]?.cell(row));
    return (
      <ul aria-label={label} className="flex flex-col gap-3">
        {rows.map((row) => {
          const actions = cardActions?.(row);
          return (
            <li key={getKey(row)} className="rounded-lg border bg-card p-4 shadow-card">
              <div className="min-w-0 text-sm">{title(row)}</div>
              <dl className="mt-3 grid grid-cols-2 gap-x-4 gap-y-2.5 text-sm">
                {body
                  .filter((c) => c.hideOnCard !== true)
                  .map((c) => (
                    <div key={c.id} className="min-w-0">
                      <dt className="text-xs text-muted-foreground">
                        {c.cardLabel ?? (typeof c.header === "string" ? c.header : c.id)}
                      </dt>
                      <dd className="mt-0.5 min-w-0 break-words">{c.cell(row)}</dd>
                    </div>
                  ))}
              </dl>
              {actions !== undefined && actions !== null ? (
                <div className="mt-4 flex flex-wrap gap-2 border-t pt-3 [&>*]:flex-1">{actions}</div>
              ) : null}
            </li>
          );
        })}
      </ul>
    );
  }

  return (
    <Table aria-label={label} density={density}>
      <TableHeader>
        <TableRow className="hover:bg-transparent">
          {columns.map((c) => (
            <TableHead key={c.id} className={c.headClassName}>
              {c.header}
            </TableHead>
          ))}
        </TableRow>
      </TableHeader>
      <TableBody>
        {rows.map((row) => {
          const extra = rowProps?.(row) ?? {};
          return (
            <TableRow key={getKey(row)} {...extra} className={cn(extra.className)}>
              {columns.map((c) => (
                <TableCell key={c.id} className={c.className}>
                  {c.cell(row)}
                </TableCell>
              ))}
            </TableRow>
          );
        })}
      </TableBody>
    </Table>
  );
}
