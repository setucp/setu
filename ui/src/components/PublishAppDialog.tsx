import * as React from "react";
import { CheckCircle2, FileCode2, Link2 } from "lucide-react";
import { toast } from "sonner";
import { usePublishApp, useValidateManifest } from "@/lib/api";
import { ApiError } from "@/lib/client";
import { cn } from "@/lib/utils";
import { ErrorText } from "./ErrorText";
import { Button } from "./ui/button";
import { Modal } from "./ui/dialog";
import { Field } from "./ui/field";
import { Input } from "./ui/input";
import { Textarea } from "./ui/textarea";

type Mode = "toml" | "url";

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
  const [mode, setMode] = React.useState<Mode>("toml");
  const [text, setText] = React.useState("");
  const [validatedText, setValidatedText] = React.useState<string | null>(null);
  const validate = useValidateManifest();
  const publish = usePublishApp();
  const resetValidate = validate.reset;
  const resetPublish = publish.reset;

  React.useEffect(() => {
    if (open) {
      setText("");
      setValidatedText(null);
      resetValidate();
      resetPublish();
    }
  }, [open, resetValidate, resetPublish]);

  const fresh = validatedText !== null && validatedText === text;
  const validation = validate.data;
  const problems = validation !== undefined && !validation.ok ? validation.errors : [];

  function doPublish() {
    const payload = mode === "toml" ? { toml: text } : { url: text.trim() };
    publish.mutate(payload, {
      onSuccess: () => {
        toast.success("App published");
        onPublished?.();
        onClose();
      },
      onError: (err) => {
        toast.error(err instanceof ApiError ? err.message : "Publish failed");
      },
    });
  }

  const disabled =
    text.trim() === "" ||
    // Pasted manifests need a fresh validation; URLs are validated
    // server-side at publish time.
    (mode === "toml" && (!validation?.ok || !fresh));

  return (
    <Modal
      open={open}
      onClose={onClose}
      size="lg"
      title="Add app"
      description="Publish a version from a catalogue URL or pasted manifest TOML."
      footer={
        <>
          {mode === "toml" && !fresh ? (
            <span className="text-xs text-muted-foreground sm:mr-auto sm:self-center">
              Validate the manifest before publishing.
            </span>
          ) : null}
          <Button variant="outline" onClick={onClose}>
            Cancel
          </Button>
          <Button type="button" disabled={disabled} loading={publish.isPending} onClick={doPublish}>
            {publish.isPending ? "Publishing…" : "Publish"}
          </Button>
        </>
      }
    >
      <div className="flex flex-col gap-4">
        <div role="radiogroup" aria-label="Source" className="grid grid-cols-2 gap-1 rounded-lg bg-muted p-1">
          {(
            [
              { value: "toml", label: "Paste TOML", icon: FileCode2 },
              { value: "url", label: "From URL", icon: Link2 },
            ] as const
          ).map(({ value, label, icon: Icon }) => (
            <button
              key={value}
              type="button"
              role="radio"
              aria-checked={mode === value}
              onClick={() => {
                setMode(value);
                setValidatedText(null);
                resetValidate();
              }}
              className={cn(
                "touch-target inline-flex h-8 items-center justify-center gap-2 rounded-md text-sm font-medium transition-colors",
                mode === value
                  ? "bg-card text-foreground shadow-card"
                  : "text-muted-foreground hover:text-foreground",
              )}
            >
              <Icon className="size-4" aria-hidden="true" />
              {label}
            </button>
          ))}
        </div>
        {mode === "toml" ? (
          <Field label="Manifest TOML">
            <Textarea
              value={text}
              onChange={(e) => setText(e.target.value)}
              rows={12}
              spellCheck={false}
              placeholder={'schema = 1\n\n[app]\nid = "hello"'}
              className="font-mono text-xs"
              aria-invalid={problems.length > 0 || undefined}
            />
          </Field>
        ) : (
          <Field label="Manifest URL" hint="Fetched and validated by the panel when you publish.">
            <Input
              value={text}
              onChange={(e) => setText(e.target.value)}
              placeholder="https://example.com/manifest.toml"
              inputMode="url"
              autoCapitalize="none"
              spellCheck={false}
            />
          </Field>
        )}
        {mode === "toml" ? (
          <div className="flex flex-wrap items-center gap-3">
            <Button
              type="button"
              variant="outline"
              size="sm"
              disabled={text.trim() === ""}
              loading={validate.isPending}
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
                <p className="inline-flex items-center gap-1.5 text-sm font-medium text-success">
                  <CheckCircle2 className="size-4" aria-hidden="true" />
                  Manifest is valid.
                </p>
              ) : (
                <p className="text-sm font-medium text-destructive">
                  {validation.errors.length} problem
                  {validation.errors.length === 1 ? "" : "s"} found.
                </p>
              )
            ) : null}
          </div>
        ) : null}
        {problems.length > 0 ? (
          <ul
            aria-label="Manifest problems"
            className="flex flex-col divide-y divide-destructive/15 overflow-hidden rounded-md border border-destructive/30 bg-destructive-subtle text-xs"
          >
            {problems.map((e, i) => (
              <li key={i} className="flex gap-3 px-3 py-2">
                <span className="shrink-0 font-mono font-medium text-destructive">
                  {e.line !== null && e.line !== undefined ? `line ${e.line}` : "—"}
                </span>
                <span className="font-mono text-muted-foreground">{e.path}</span>
                <span className="min-w-0 flex-1 break-words text-foreground">{e.message}</span>
              </li>
            ))}
          </ul>
        ) : null}
        <ErrorText error={validate.isError ? validate.error : null} />
        <ErrorText error={publish.isError ? publish.error : null} />
      </div>
    </Modal>
  );
}
