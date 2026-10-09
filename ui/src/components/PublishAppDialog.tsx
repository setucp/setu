import * as React from "react";
import { toast } from "sonner";
import { usePublishApp, useValidateManifest } from "@/lib/api";
import { ApiError } from "@/lib/client";
import { Button } from "./ui/button";
import { Modal } from "./ui/dialog";
import { Input } from "./ui/input";
import { Select } from "./ui/select";

/**
 * Publish dialog: from a catalogue URL or pasted TOML. Pasted manifests must
 * pass `POST /apps/validate` before Publish enables — the validator's errors
 * render inline with path and line.
 */
export function PublishAppDialog({
  open,
  onClose,
  onPublished,
}: {
  open: boolean;
  onClose: () => void;
  onPublished?: () => void;
}) {
  const [mode, setMode] = React.useState<"toml" | "url">("toml");
  const [text, setText] = React.useState("");
  const [validatedText, setValidatedText] = React.useState<string | null>(null);
  const validate = useValidateManifest();
  const publish = usePublishApp();

  React.useEffect(() => {
    if (open) {
      setText("");
      setValidatedText(null);
      validate.reset();
      publish.reset();
    }
  }, [open ]);

  const fresh = validatedText !== null && validatedText === text;
  const validation = validate.data;

  function doPublish() {
    const payload = mode === "toml" ? { toml: text } : { url: text.trim() };
    publish.mutate(payload, {
      onSuccess: () => {
        toast.success("App published");
        onPublished?.();
        onClose();
      },
      onError: (err) => {
        toast.error(err instanceof ApiError ? err.message : "publish failed");
      },
    });
  }

  return (
    <Modal
      open={open}
      onClose={onClose}
      size="lg"
      title="Add app"
      description="Publish a version from a catalogue URL or pasted manifest TOML."
    >
      <div className="flex flex-col gap-3">
        <label className="flex flex-col gap-1.5 text-sm font-medium">
          Source
          <Select
            aria-label="Source"
            value={mode}
            onChange={(e) => {
              setMode(e.target.value as "toml" | "url");
              setValidatedText(null);
              validate.reset();
            }}
          >
            <option value="toml">Paste manifest TOML</option>
            <option value="url">Fetch from URL</option>
          </Select>
        </label>
        {mode === "toml" ? (
          <label className="flex flex-col gap-1.5 text-sm font-medium">
            Manifest TOML
            <textarea
              aria-label="Manifest TOML"
              value={text}
              onChange={(e) => setText(e.target.value)}
              rows={10}
              spellCheck={false}
              placeholder={'schema = 1\n\n[app]\nid = "hello"'}
              className="flex w-full rounded-md border border-input bg-background px-3 py-2 font-mono text-sm shadow-xs placeholder:text-muted-foreground focus-visible:outline-hidden focus-visible:ring-1 focus-visible:ring-ring"
            />
          </label>
        ) : (
          <label className="flex flex-col gap-1.5 text-sm font-medium">
            Manifest URL
            <Input
              aria-label="Manifest URL"
              value={text}
              onChange={(e) => setText(e.target.value)}
              placeholder="https://example.com/manifest.toml"
              inputMode="url"
            />
          </label>
        )}
        {mode === "toml" ? (
          <div className="flex items-center gap-2">
            <Button
              type="button"
              variant="outline"
              size="sm"
              disabled={text.trim() === "" || validate.isPending}
              onClick={() => {
                validate.mutate(text, {
                  onSuccess: (res) => {
                    if (res.ok) setValidatedText(text);
                  },
                });
              }}
            >
              {validate.isPending ? "Validating…" : "Validate"}
            </Button>
            {validation !== undefined && !validate.isPending ? (
              validation.ok ? (
                <p className="text-sm text-green-700 dark:text-green-400">
                  Manifest is valid.
                </p>
              ) : (
                <p className="text-sm text-destructive">
                  {validation.errors.length} problem
                  {validation.errors.length === 1 ? "" : "s"} found.
                </p>
              )
            ) : null}
          </div>
        ) : null}
        {validation !== undefined && !validation.ok ? (
          <ul className="flex flex-col gap-1 rounded-md border border-destructive/40 p-3 text-sm">
            {validation.errors.map((e, i) => (
              <li key={i} className="font-mono text-xs text-destructive">
                {e.path}
                {e.line !== null && e.line !== undefined ? `:${e.line}` : ""} —{" "}
                {e.message}
              </li>
            ))}
          </ul>
        ) : null}
        {validate.isError && validate.error instanceof ApiError ? (
          <p role="alert" className="text-sm text-destructive">
            {validate.error.message}
          </p>
        ) : null}
        <div className="flex gap-2">
          <Button
            type="button"
            disabled={
              publish.isPending ||
              text.trim() === "" ||
              // Pasted manifests need a fresh validation; URLs are validated
              // server-side at publish time.
              (mode === "toml" && (!validation?.ok || !fresh))
            }
            onClick={doPublish}
          >
            {publish.isPending ? "Publishing…" : "Publish"}
          </Button>
          {mode === "toml" && !fresh ? (
            <span className="self-center text-xs text-muted-foreground">
              Validate the manifest before publishing.
            </span>
          ) : null}
        </div>
        {publish.isError && publish.error instanceof ApiError ? (
          <p role="alert" className="text-sm text-destructive">
            {publish.error.message}
          </p>
        ) : null}
      </div>
    </Modal>
  );
}
