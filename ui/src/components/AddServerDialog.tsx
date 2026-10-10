import * as React from "react";
import { toast } from "sonner";
import { useCreateServer } from "@/lib/api";
import { ApiError } from "@/lib/client";
import { InstallInstructions } from "./CopyButton";
import { ErrorText } from "./ErrorText";
import { Alert } from "./ui/alert";
import { Button } from "./ui/button";
import { Modal } from "./ui/dialog";
import { Field } from "./ui/field";
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
      footer={
        result === undefined ? (
          <>
            <Button variant="outline" onClick={onClose}>
              Cancel
            </Button>
            <Button type="submit" form="add-server-form" loading={create.isPending}>
              {create.isPending ? "Adding…" : "Add server"}
            </Button>
          </>
        ) : (
          <Button type="button" onClick={onClose}>
            Done
          </Button>
        )
      }
    >
      {result === undefined ? (
        <form
          id="add-server-form"
          aria-label="Add server"
          className="flex flex-col gap-4"
          onSubmit={(e) => {
            e.preventDefault();
            if (name.trim() === "") return;
            create.mutate(name.trim(), {
              onSuccess: (res) => {
                toast.success(`${res.server.name} registered`);
                onCreated?.(res.server.id);
              },
              onError: (err) => {
                toast.error(err instanceof ApiError ? err.message : "Add failed");
              },
            });
          }}
        >
          <Field label="Server name" hint="A short name you will recognise, e.g. web1.">
            <Input
              value={name}
              onChange={(e) => setName(e.target.value)}
              placeholder="web1"
              autoComplete="off"
              autoFocus
              required
            />
          </Field>
          <ErrorText error={create.isError ? create.error : null} />
        </form>
      ) : (
        <div className="flex flex-col gap-4">
          <Alert variant="success" role="status" title={`${result.server.name} registered`}>
            It shows as <code>pending</code> until its agent joins.
          </Alert>
          <InstallInstructions command={result.install_command} joinToken={result.join_token} />
        </div>
      )}
    </Modal>
  );
}
