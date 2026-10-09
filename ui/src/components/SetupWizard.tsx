import * as React from "react";
import { api, ApiError, type LocalServerResponse } from "@/lib/client";
import { InstallInstructions } from "./CopyButton";
import { Button } from "./ui/button";
import { Input } from "./ui/input";
import { cn } from "@/lib/utils";

type Phase =
  | { name: "checking" }
  | { name: "token" }
  | { name: "owner" }
  | { name: "server"; publicUrl: string; install: LocalServerResponse | null }
  | { name: "done" }
  | { name: "complete" };

const STEPS: Partial<Record<Phase["name"], { n: number; title: string }>> = {
  token: { n: 1, title: "Setup token" },
  owner: { n: 2, title: "Owner account" },
  server: { n: 3, title: "This server" },
  done: { n: 4, title: "Done" },
};

/**
 * First-run wizard (spec §7): setup token → owner → local server → done.
 * The token is asked alone, then posted with the owner in one request;
 * a wrong token (401) returns to step 1. The local-server step shows the
 * token-free install command and the join token apart, each with a copy
 * button, and polls `GET /servers` until that server is online. Plain
 * `fetch` calls with local state (no query client needed); `onDone`
 * instead of navigation so tests can assert completion.
 */
export function SetupWizard({
  onDone,
  pollMs = 2000,
  className,
}: {
  onDone?: () => void;
  pollMs?: number;
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
        if (!cancelled) setPhase(status.needed ? { name: "token" } : { name: "complete" });
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
    const trimmedEmail = email.trim();
    try {
      await api.setupOwner(token.trim(), trimmedEmail, password);
    } catch (err) {
      setBusy(false);
      if (err instanceof ApiError && err.status === 401) {
        setToken("");
        setPhase({ name: "token" });
      }
      setError(err instanceof ApiError ? err.message : "setup failed");
      return;
    }
    try {
      // The setup endpoint does not start a session; sign in explicitly.
      await api.login(trimmedEmail, password);
      setPassword("");
      setToken("");
      const status = await api.setupStatus();
      setPhase({ name: "server", publicUrl: status.public_url, install: null });
    } catch (err) {
      setError(err instanceof ApiError ? err.message : "sign-in failed");
    } finally {
      setBusy(false);
    }
  }

  async function registerServer(publicUrl: string) {
    setBusy(true);
    setError(null);
    try {
      const install = await api.setupLocalServer();
      setPhase({ name: "server", publicUrl, install });
    } catch (err) {
      setError(err instanceof ApiError ? err.message : "server registration failed");
    } finally {
      setBusy(false);
    }
  }

  // Once the command is shown, poll the servers list until the agent
  // joins and heartbeats (`online`).
  const waitingFor = phase.name === "server" ? (phase.install?.server.id ?? null) : null;
  const onDoneRef = React.useRef(onDone);
  onDoneRef.current = onDone;
  React.useEffect(() => {
    if (waitingFor === null) return;
    let stopped = false;
    const timer = window.setInterval(() => {
      api
        .listServers()
        .then((servers) => {
          const server = servers.find((s) => s.id === waitingFor);
          if (!stopped && server?.status === "online") {
            stopped = true;
            window.clearInterval(timer);
            setPhase({ name: "done" });
            onDoneRef.current?.();
          }
        })
        .catch(() => {
          // Keep polling through transient failures.
        });
    }, pollMs);
    return () => {
      stopped = true;
      window.clearInterval(timer);
    };
  }, [waitingFor, pollMs]);

  const step = STEPS[phase.name];

  return (
    <div className={cn("flex min-h-dvh items-center justify-center px-5 py-10", className)}>
      <div className="w-full max-w-lg">
        <h1 className="text-xl font-semibold tracking-tight">Set up Setu</h1>
        {step !== undefined ? (
          <p className="mt-1 text-sm text-muted-foreground">
            Step {step.n} of 4: {step.title}
          </p>
        ) : null}
        {phase.name === "checking" && error === null ? (
          <p className="mt-4 text-sm text-muted-foreground">Checking setup status…</p>
        ) : null}
        {error !== null ? (
          <p role="alert" className="mt-4 text-sm text-destructive">
            {error}
          </p>
        ) : null}

        {phase.name === "token" ? (
          <form
            aria-label="Token"
            className="mt-4 flex flex-col gap-4"
            onSubmit={(e) => {
              e.preventDefault();
              if (token.trim() === "") return;
              setError(null);
              setPhase({ name: "owner" });
            }}
          >
            <p className="text-sm text-muted-foreground">
              The installer printed a one-time setup token. It is also in{" "}
              <span className="font-mono">/var/lib/setu/setup-token</span> on the
              panel host (readable by root and the <span className="font-mono">setu</span>{" "}
              user only).
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
            <Button type="submit" className="self-start">
              Continue
            </Button>
          </form>
        ) : null}

        {phase.name === "owner" ? (
          <form
            aria-label="Create owner"
            className="mt-4 flex flex-col gap-4"
            onSubmit={(e) => void createOwner(e)}
          >
            <p className="text-sm text-muted-foreground">
              Create the first operator: the owner, who can do everything. This
              step works exactly once.
            </p>
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
            <div className="flex gap-2">
              <Button
                type="button"
                variant="outline"
                onClick={() => {
                  setError(null);
                  setPhase({ name: "token" });
                }}
              >
                Back
              </Button>
              <Button type="submit" disabled={busy}>
                {busy ? "Creating…" : "Create owner"}
              </Button>
            </div>
          </form>
        ) : null}

        {phase.name === "server" ? (
          <div className="mt-4 flex flex-col gap-4">
            <p className="text-sm">Owner created and signed in. The panel lives at:</p>
            <code className="block overflow-x-auto rounded-md border bg-muted/40 p-3 font-mono text-xs break-all">
              {phase.publicUrl}
            </code>
            {phase.install === null ? (
              <>
                <p className="text-sm text-muted-foreground">
                  Register this machine as the first server; its agent joins the
                  panel like any other server.
                </p>
                <Button
                  type="button"
                  disabled={busy}
                  onClick={() => void registerServer(phase.publicUrl)}
                  className="self-start"
                >
                  {busy ? "Registering…" : "Register this server"}
                </Button>
              </>
            ) : (
              <>
                <p className="text-sm">
                  Server <span className="font-medium">{phase.install.server.name}</span>{" "}
                  registered.
                </p>
                <InstallInstructions
                  command={phase.install.install_command}
                  joinToken={phase.install.join_token}
                />
                <p className="text-xs text-muted-foreground" aria-live="polite">
                  Waiting for the agent to join — this page continues on its own
                  once the server is online.
                </p>
              </>
            )}
          </div>
        ) : null}

        {phase.name === "done" ? (
          <div className="mt-4 flex flex-col gap-2">
            <p className="text-sm text-green-700 dark:text-green-400">
              Server is online. Setup is finished.
            </p>
            <a href="/sites" className="text-sm underline underline-offset-2">
              Go to sites
            </a>
          </div>
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
