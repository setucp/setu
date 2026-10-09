import * as React from "react";
import { toast } from "sonner";
import { useCreateServer } from "@/lib/api";
import { ApiError } from "@/lib/client";
import { InstallInstructions } from "./CopyButton";
import { Button } from "./ui/button";
import { Modal } from "./ui/dialog";
import { Input } from "./ui/input";

/**
 * Add-server dialog: names a pending server, then shows the token-free
 * install command and, on its own line, the single-use join token (both
 * with copy buttons). Neither lingers: reopening starts fresh.
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
  const reset = create.reset;

  React.useEffect(() => {
    if (open) {
      setName("");
      reset();
    }
  }, [open, reset]);

  const result = create.data;

  return (
    <Modal
      open={open}
      onClose={onClose}
      size="lg"
      title="Add server"
      description="Registers a pending server and mints its single-use join token (valid one hour)."
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
            Server <span className="font-medium">{result.server.name}</span> registered.
            It shows as <span className="font-mono">pending</span> until its agent joins.
          </p>
          <InstallInstructions command={result.install_command} joinToken={result.join_token} />
          <Button type="button" size="sm" className="self-start" onClick={onClose}>
            Done
          </Button>
        </div>
      )}
    </Modal>
  );
}
