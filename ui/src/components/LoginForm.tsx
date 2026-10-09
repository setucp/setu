import * as React from "react";
import { useLogin } from "@/lib/api";
import { ApiError } from "@/lib/client";
import { Button } from "./ui/button";
import { Input } from "./ui/input";
import { cn } from "@/lib/utils";

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
    return err instanceof ApiError ? err.message : "sign in failed";
  }

  return (
    <form
      aria-label="Sign in"
      className={cn("flex flex-col gap-4", className)}
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
      <h1 className="text-xl font-semibold tracking-tight">Sign in to Setu</h1>
      <label className="flex flex-col gap-1.5 text-sm font-medium">
        Email
        <Input
          type="email"
          autoComplete="username"
          value={email}
          onChange={(e) => setEmail(e.target.value)}
          required
        />
      </label>
      <label className="flex flex-col gap-1.5 text-sm font-medium">
        Password
        <Input
          type="password"
          autoComplete="current-password"
          value={password}
          onChange={(e) => setPassword(e.target.value)}
          required
        />
      </label>
      {errorMessage() !== null ? (
        <p role="alert" className="text-sm text-destructive">
          {errorMessage()}
        </p>
      ) : null}
      {login.isSuccess ? (
        <p className="text-sm text-green-700 dark:text-green-400">
          Signed in as {login.data.email}
        </p>
      ) : null}
      <Button type="submit" disabled={login.isPending}>
        {login.isPending ? "Signing in…" : "Sign in"}
      </Button>
    </form>
  );
}
