import { ApiError } from "@/lib/client";
import { Alert } from "./ui/alert";

/** Renders an `ApiError` as a destructive alert; anything else renders nothing. */
export function ErrorText({ error, className }: { error: unknown; className?: string }) {
  if (!(error instanceof ApiError)) return null;
  return (
    <Alert variant="destructive" className={className}>
      {error.message}
    </Alert>
  );
}
