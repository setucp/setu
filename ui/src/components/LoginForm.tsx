import * as React from "react";
import { useLogin } from "@/lib/api";
import { ApiError } from "@/lib/client";
import { cn } from "@/lib/utils";
import { Alert } from "./ui/alert";
import { Button } from "./ui/button";
import { Field } from "./ui/field";
import { Input } from "./ui/input";

/** Sign-in form: posts credentials, reports the operator on success. */
export function LoginForm({
  onSuccess,
  className,
}: {
  onSuccess?: (operator: { email: string }) => void;
  className?: string;
}) {
  const [email, setEmail] = React.useState("");
  const [password, setPassword] = React.useState("");
  const login = useLogin();

  function errorMessage(): string | null {
    if (!login.isError) return null;
    const err = login.error;
    return err instanceof ApiError ? err.message : "Sign in failed";
  }
  const error = errorMessage();

  return (
    <form
      aria-label="Sign in"
      className={cn(
        "flex flex-col gap-5 rounded-xl border bg-card p-6 shadow-overlay sm:p-8",
        className,
      )}
      onSubmit={(e) => {
        e.preventDefault();
        login.mutate(
          { email, password },
          {
            onSuccess: (operator) => {
              if (onSuccess) onSuccess(operator);
            },
          },
        );
      }}
    >
      <div className="flex flex-col gap-1">
        <h1 className="text-xl font-semibold tracking-tight">Sign in to Setu</h1>
        <p className="text-sm text-muted-foreground">Use your operator email and password.</p>
      </div>
      {error !== null ? <Alert variant="destructive">{error}</Alert> : null}
      <Field label="Email">
        <Input
          type="email"
          autoComplete="username"
          autoFocus
          placeholder="you@example.com"
          value={email}
          onChange={(e) => setEmail(e.target.value)}
          aria-invalid={error !== null || undefined}
          required
        />
      </Field>
      <Field label="Password">
        <Input
          type="password"
          autoComplete="current-password"
          value={password}
          onChange={(e) => setPassword(e.target.value)}
          aria-invalid={error !== null || undefined}
          required
        />
      </Field>
      {login.isSuccess ? (
        <Alert variant="success" role="status">
          Signed in as {login.data.email}
        </Alert>
      ) : null}
      <Button type="submit" size="lg" loading={login.isPending} className="w-full">
        {login.isPending ? "Signing in…" : "Sign in"}
      </Button>
    </form>
  );
}
