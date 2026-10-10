import * as React from "react";
import { Check, Copy } from "lucide-react";
import { toast } from "sonner";
import { cn } from "@/lib/utils";
import { Button } from "./ui/button";

export async function copyText(text: string): Promise<boolean> {
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    return false;
  }
}

/** Copies `text`; the button flips to "Copied" and never toasts the value. */
export function CopyButton({
  text,
  label,
  className,
  iconOnly = false,
}: {
  text: string;
  label: string;
  className?: string;
  iconOnly?: boolean;
}) {
  const [copied, setCopied] = React.useState(false);
  React.useEffect(() => {
    if (!copied) return;
    const t = window.setTimeout(() => setCopied(false), 1800);
    return () => window.clearTimeout(t);
  }, [copied]);
  const name = `Copy ${label.toLowerCase()}`;
  return (
    <Button
      type="button"
      variant="outline"
      size={iconOnly ? "icon-sm" : "sm"}
      aria-label={iconOnly ? name : undefined}
      className={cn(copied && "text-success", className)}
      onClick={() => {
        void copyText(text).then((ok) => {
          if (ok) setCopied(true);
          else toast.error("Copy failed — select it manually");
        });
      }}
    >
      {copied ? <Check aria-hidden="true" /> : <Copy aria-hidden="true" />}
      {iconOnly ? null : copied ? (
        <span>
          Copied<span className="sr-only"> {label.toLowerCase()}</span>
        </span>
      ) : (
        name
      )}
    </Button>
  );
}

/** A copyable mono value: the text in a code well with a copy button beside it. */
export function CopyField({
  value,
  label,
  testId,
}: {
  value: string;
  label: string;
  testId?: string;
}) {
  return (
    <div className="flex flex-col gap-2 sm:flex-row sm:items-start">
      <code
        data-testid={testId}
        className="scrollbar-thin block min-w-0 flex-1 overflow-x-auto rounded-md border bg-muted/60 px-3 py-2.5 font-mono text-xs leading-5 break-all"
      >
        {value}
      </code>
      <CopyButton text={value} label={label} className="self-start" />
    </div>
  );
}

/**
 * The install command and its join token, apart: the command is safe to
 * paste into a shell history, the token goes into the environment.
 */
export function InstallInstructions({
  command,
  joinToken,
}: {
  command: string;
  joinToken: string;
}) {
  return (
    <div className="flex flex-col gap-4">
      <div className="flex flex-col gap-2">
        <p className="text-sm font-medium">1. Run this on the server as root</p>
        <CopyField value={command} label="Install command" testId="install-command" />
      </div>
      <div className="flex flex-col gap-2">
        <p className="text-sm font-medium">
          2. Paste the join token when asked{" "}
          <span className="font-normal text-muted-foreground">
            (single use, valid one hour, shown once)
          </span>
        </p>
        <CopyField value={joinToken} label="Join token" testId="join-token" />
      </div>
      <p className="text-xs text-muted-foreground">
        The command asks for the token on stdin. To run it unattended, prefix
        it with <code>SETU_JOIN_TOKEN=&lt;token&gt; </code>
        (the token, a space, then the command). Never pass the token as a
        command-line flag.
      </p>
    </div>
  );
}
