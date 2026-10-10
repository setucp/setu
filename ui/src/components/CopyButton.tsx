import { toast } from "sonner";
import { Button } from "./ui/button";

export async function copyText(text: string): Promise<boolean> {
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    return false;
  }
}

/** Copies `text`; the toast names what was copied, never the value. */
export function CopyButton({ text, label }: { text: string; label: string }) {
  return (
    <Button
      type="button"
      variant="outline"
      size="sm"
      onClick={() => {
        void copyText(text).then((ok) => {
          toast[ok ? "success" : "error"](
            ok ? `${label} copied` : "Copy failed — select it manually",
          );
        });
      }}
    >
      Copy {label.toLowerCase()}
    </Button>
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
    <div className="flex flex-col gap-3">
      <p className="text-sm">Run this on the server as root:</p>
      <code
        data-testid="install-command"
        className="block overflow-x-auto rounded-md border bg-muted/40 p-3 font-mono text-xs break-all"
      >
        {command}
      </code>
      <div>
        <CopyButton text={command} label="Install command" />
      </div>
      <p className="text-sm">Join token (single use, valid one hour, shown once):</p>
      <code
        data-testid="join-token"
        className="block overflow-x-auto rounded-md border bg-muted/40 p-3 font-mono text-xs break-all"
      >
        {joinToken}
      </code>
      <div>
        <CopyButton text={joinToken} label="Join token" />
      </div>
      <p className="text-xs text-muted-foreground">
        The command asks for the token on stdin. To run it unattended, prefix
        it with <code className="font-mono">SETU_JOIN_TOKEN=&lt;token&gt; </code>
        (the token, a space, then the command). Never pass the token as a
        command-line flag.
      </p>
    </div>
  );
}
