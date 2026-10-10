import * as React from "react";
import { cn } from "@/lib/utils";

/**
 * Label + control + hint/error, wired with ids so the control is named
 * by its label and described by its hint or error.
 */
export function Field({
  label,
  hint,
  error,
  children,
  className,
  optional = false,
}: {
  label: React.ReactNode;
  hint?: React.ReactNode;
  error?: React.ReactNode;
  children: React.ReactElement<{
    id?: string;
    "aria-describedby"?: string;
    "aria-invalid"?: boolean;
  }>;
  className?: string;
  optional?: boolean;
}) {
  const id = React.useId();
  const hintId = `${id}-hint`;
  const errorId = `${id}-error`;
  const describedBy = error ? errorId : hint ? hintId : undefined;
  const control = React.cloneElement(children, {
    id: children.props.id ?? id,
    "aria-describedby": describedBy,
    "aria-invalid": error ? true : undefined,
  });
  return (
    <div className={cn("flex flex-col gap-1.5", className)}>
      <label htmlFor={children.props.id ?? id} className="text-sm font-medium leading-5">
        {label}
        {optional ? (
          <span className="ml-1.5 font-normal text-muted-foreground">(optional)</span>
        ) : null}
      </label>
      {control}
      {hint && !error ? (
        <p id={hintId} className="text-xs text-muted-foreground">
          {hint}
        </p>
      ) : null}
      {error ? (
        <p id={errorId} className="text-xs font-medium text-destructive">
          {error}
        </p>
      ) : null}
    </div>
  );
}
