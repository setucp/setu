import { Alert } from "./ui/alert";

/** Renders an error (usually an `ApiError`) as a destructive alert; nothing otherwise. */
export function ErrorText({ error, className }: { error: unknown; className?: string }) {
  if (!(error instanceof Error)) return null;
  return (
    <Alert variant="destructive" className={className}>
      {error.message}
    </Alert>
  );
}
