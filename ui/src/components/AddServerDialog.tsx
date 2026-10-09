import * as React from "react";
import { toast } from "sonner";
import { useCreateServer } from "@/lib/api";
import { ApiError } from "@/lib/client";
import { Button } from "./ui/button";
import { Modal } from "./ui/dialog";
import { Input } from "./ui/input";

async function copyText(text: string): Promise<boolean> {
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    return false;
  }
}

/**
 * Add-server dialog: names a pending server, then shows the one-line install
 * command (join token embedded, shown once) with a copy button.
 */
export function AddServerDialog({
  open,
  onClose,
  onCreated,
}: {
  open: boolean;
  onClose: () => void;
  onCreated?: (serverId: string) => void;
}) {
  const [name, setName] = React.useState("");
  const create = useCreateServer();

  // Fresh state for every opening; the install command never lingers.
  React.useEffect(() => {
    if (open) {
      setName("");
      create.reset();
    }
    // Reset only on open transitions.
  }, [open ]);

  const result = create.data;

  return (
    <Modal
      open={open}
      onClose={onClose}
      title="Add server"
      description="Registers a pending server and mints its single-use join token (valid 1 hour). Run the install command on the server as root."
    >
      {result === undefined ? (
        <form
          aria-label="Add server"
          className="flex flex-col gap-3"
          onSubmit={(e) => {
            e.preventDefault();
            if (name.trim() === "") return;
            create.mutate(name.trim(), {
              onSuccess: (res) => onCreated?.(res.server.id),
              onError: (err) => {
                toast.error(err instanceof ApiError ? err.message : "add failed");
              },
            });
          }}
        >
          <label className="flex flex-col gap-1.5 text-sm font-medium">
            Server name
            <Input
              value={name}
              onChange={(e) => setName(e.target.value)}
              placeholder="web1"
              required
            />
          </label>
          {create.isError && create.error instanceof ApiError ? (
            <p role="alert" className="text-sm text-destructive">
              {create.error.message}
            </p>
          ) : null}
          <Button type="submit" disabled={create.isPending} className="self-start">
            {create.isPending ? "Adding…" : "Add server"}
          </Button>
        </form>
      ) : (
        <div className="flex flex-col gap-3">
          <p className="text-sm">
            Server <span className="font-medium">{result.server.name}</span>{" "}
            registered. Run this on the server:
          </p>
          <code className="block overflow-x-auto rounded-md border bg-muted/40 p-3 font-mono text-xs break-all">
            {result.install_command}
          </code>
          <div className="flex gap-2">
            <Button
              type="button"
              variant="outline"
              size="sm"
              onClick={() => {
                void copyText(result.install_command).then((ok) => {
                  toast[ok ? "success" : "error"](
                    ok ? "Install command copied" : "Copy failed — select it manually",
                  );
                });
              }}
            >
              Copy install command
            </Button>
            <Button type="button" size="sm" onClick={onClose}>
              Done
            </Button>
          </div>
          <p className="text-xs text-muted-foreground">
            The join token is shown once and expires after one hour.
          </p>
        </div>
      )}
    </Modal>
  );
}
