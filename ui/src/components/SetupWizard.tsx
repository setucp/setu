import * as React from "react";
import { toast } from "sonner";
import { api, ApiError, type LocalServerResponse } from "@/lib/client";
import { Button } from "./ui/button";
import { Input } from "./ui/input";
import { cn } from "@/lib/utils";

type Phase =
  | { name: "checking" }
  | { name: "owner" }
  | { name: "panel"; publicUrl: string }
  | { name: "server"; install: LocalServerResponse }
  | { name: "done" }
  | { name: "complete" };

async function copyText(text: string): Promise<boolean> {
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    return false;
  }
}

/**
 * First-run wizard: create the owner, show the panel URL, register the
 * local server and wait for it to come online. Used by `/setup`; plain
 * `fetch` calls with local state (no query client needed), and `onDone`
 * instead of navigation so tests can assert completion.
 */
export function SetupWizard({
  onDone,
  className,
}: {
  onDone?: () => void;
  className?: string;
}) {
  const [phase, setPhase] = React.useState<Phase>({ name: "checking" });
  const [token, setToken] = React.useState("");
  const [email, setEmail] = React.useState("");
  const [password, setPassword] = React.useState("");
  const [busy, setBusy] = React.useState(false);
  const [error, setError] = React.useState<string | null>(null);

  React.useEffect(() => {
    let cancelled = false;
    api
      .setupStatus()
      .then((status) => {
        if (!cancelled) {
          setPhase(
            status.needed
              ? { name: "owner" }
              : { name: "complete" },
          );
        }
      })
      .catch((err: unknown) => {
        if (!cancelled) {
          setError(err instanceof ApiError ? err.message : "setup check failed");
        }
      });
    return () => {
      cancelled = true;
    };
  }, []);

  async function createOwner(e: React.FormEvent) {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      const trimmedEmail = email.trim();
      await api.setupOwner(token.trim(), trimmedEmail, password);
      // The setup endpoint does not start a session; sign in explicitly.
      await api.login(trimmedEmail, password);
      setPassword("");
      setToken("");
      const status = await api.setupStatus();
      setPhase({ name: "panel", publicUrl: status.public_url });
    } catch (err) {
      setError(err instanceof ApiError ? err.message : "setup failed");
    } finally {
      setBusy(false);
    }
  }

  async function registerServer() {
    setBusy(true);
    setError(null);
    try {
      const install = await api.setupLocalServer();
      setPhase({ name: "server", install });
    } catch (err) {
      setError(err instanceof ApiError ? err.message : "server registration failed");
    } finally {
      setBusy(false);
    }
  }

  // After the install command is shown, poll the server until the agent
  // joins (online) — then the single-node install is one finished flow.
  React.useEffect(() => {
    if (phase.name !== "server") return;
    const id = phase.install.server.id;
    const timer = window.setInterval(() => {
      api
        .getServer(id)
        .then((server) => {
          if (server.status === "online") {
            window.clearInterval(timer);
            setPhase({ name: "done" });
            onDone?.();
          }
        })
        .catch(() => {
          // Keep polling through transient failures.
        });
    }, 2000);
    return () => window.clearInterval(timer);
  }, [phase, onDone]);

  return (
    <div className={cn("flex min-h-dvh items-center justify-center px-5", className)}>
      <div className="w-full max-w-md">
        <h1 className="text-xl font-semibold tracking-tight">Set up Setu</h1>
        {phase.name === "checking" && error === null ? (
          <p className="mt-4 text-sm text-muted-foreground">Checking setup status…</p>
        ) : null}
        {error !== null ? (
          <p role="alert" className="mt-4 text-sm text-destructive">
            {error}
          </p>
        ) : null}
        {phase.name === "owner" ? (
          <form
            aria-label="Create owner"
            className="mt-4 flex flex-col gap-4"
            onSubmit={(e) => void createOwner(e)}
          >
            <p className="text-sm text-muted-foreground">
              Create the first operator account. This step works exactly once.
              The setup token is in{" "}
              <span className="font-mono">/var/lib/setu/setup-token</span> on
              the panel host.
            </p>
            <label className="flex flex-col gap-1.5 text-sm font-medium">
              Setup token
              <Input
                type="password"
                autoComplete="off"
                value={token}
                onChange={(e) => setToken(e.target.value)}
                required
              />
            </label>
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
                autoComplete="new-password"
                minLength={8}
                value={password}
                onChange={(e) => setPassword(e.target.value)}
                required
              />
            </label>
            <Button type="submit" disabled={busy} className="self-start">
              {busy ? "Creating…" : "Create owner"}
            </Button>
          </form>
        ) : null}
        {phase.name === "panel" ? (
          <div className="mt-4 flex flex-col gap-3">
            <p className="text-sm">
              Owner created and signed in. The panel lives at:
            </p>
            <code className="block overflow-x-auto rounded-md border bg-muted/40 p-3 font-mono text-xs break-all">
              {phase.publicUrl}
            </code>
            <p className="text-sm text-muted-foreground">
              Set it with <span className="font-mono">SETU_PUBLIC_URL</span> (or{" "}
              <span className="font-mono">public_url</span> in{" "}
              <span className="font-mono">setu.toml</span>) — the install
              command below embeds it so the agent can reach this panel.
            </p>
            <Button type="button" disabled={busy} onClick={() => void registerServer()} className="self-start">
              {busy ? "Registering…" : "Register this server"}
            </Button>
          </div>
        ) : null}
        {phase.name === "server" ? (
          <div className="mt-4 flex flex-col gap-3">
            <p className="text-sm">
              Server{" "}
              <span className="font-medium">{phase.install.server.name}</span>{" "}
              registered. Run this on the server as root:
            </p>
            <code className="block overflow-x-auto rounded-md border bg-muted/40 p-3 font-mono text-xs break-all">
              {phase.install.install_command}
            </code>
            <p className="text-sm">
              It reads the join token from{" "}
              <span className="font-mono">SETU_JOIN_TOKEN</span> or stdin:
            </p>
            <code className="block overflow-x-auto rounded-md border bg-muted/40 p-3 font-mono text-xs break-all">
              {phase.install.join_token}
            </code>
            <div className="flex gap-2">
              <Button
                type="button"
                variant="outline"
                size="sm"
                onClick={() => {
                  void copyText(phase.install.install_command).then((ok) => {
                    toast[ok ? "success" : "error"](
                      ok ? "Install command copied" : "Copy failed — select it manually",
                    );
                  });
                }}
              >
                Copy install command
              </Button>
            </div>
            <p className="text-xs text-muted-foreground">
              Waiting for the agent to join — this page continues
              automatically once it is online. The join token is shown
              once and expires after one hour.
            </p>
          </div>
        ) : null}
        {phase.name === "done" ? (
          <p className="mt-4 text-sm text-green-700 dark:text-green-400">
            Server is online. Setup is finished.
          </p>
        ) : null}
        {phase.name === "complete" ? (
          <p className="mt-4 text-sm text-muted-foreground">
            Setup is already complete.{" "}
            <a href="/sites" className="underline underline-offset-2">
              Go to sites
            </a>
            .
          </p>
        ) : null}
      </div>
    </div>
  );
}
