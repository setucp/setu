import * as React from "react";
import { ArrowRight, Check, CheckCircle2, Loader2 } from "lucide-react";
import { api, ApiError, type LocalServerResponse } from "@/lib/client";
import { cn } from "@/lib/utils";
import { AuthLayout } from "./AuthLayout";
import { CopyField, InstallInstructions } from "./CopyButton";
import { Alert } from "./ui/alert";
import { Button } from "./ui/button";
import { Field } from "./ui/field";
import { Input } from "./ui/input";

type Phase =
  | { name: "checking" }
  | { name: "token" }
  | { name: "owner" }
  | { name: "server"; publicUrl: string; install: LocalServerResponse | null }
  | { name: "done" }
  | { name: "complete" };

const STEPS = [
  { key: "token", title: "Token", long: "Setup token" },
  { key: "owner", title: "Owner", long: "Owner account" },
  { key: "server", title: "Server", long: "This server" },
  { key: "done", title: "Done", long: "Done" },
] as const;

/** Numbered progress for the four setup steps; the current one is `aria-current="step"`. */
export function SetupStepper({ current }: { current: number }) {
  return (
    <ol aria-label="Setup progress" className="flex items-center">
      {STEPS.map((step, i) => {
        const n = i + 1;
        const state = n < current ? "complete" : n === current ? "current" : "upcoming";
        return (
          <li
            key={step.key}
            aria-current={state === "current" ? "step" : undefined}
            data-state={state}
            className="flex flex-1 items-center last:flex-none"
          >
            <span className="flex items-center gap-2">
              <span
                className={cn(
                  "flex size-7 shrink-0 items-center justify-center rounded-full border text-xs font-semibold transition-colors",
                  state === "complete" && "border-primary bg-primary text-primary-foreground",
                  state === "current" && "border-primary bg-primary-subtle text-link ring-4 ring-primary/10",
                  state === "upcoming" && "bg-card text-muted-foreground",
                )}
              >
                {state === "complete" ? <Check className="size-3.5" aria-hidden="true" /> : n}
              </span>
              <span
                className={cn(
                  "text-xs font-medium max-sm:sr-only",
                  state === "upcoming" ? "text-muted-foreground" : "text-foreground",
                )}
              >
                {step.title}
                {state === "complete" ? <span className="sr-only"> (done)</span> : null}
              </span>
            </span>
            {n < STEPS.length ? (
              <span
                aria-hidden="true"
                className={cn(
                  "mx-2 h-px flex-1 sm:mx-3",
                  n < current ? "bg-primary" : "bg-border",
                )}
              />
            ) : null}
          </li>
        );
      })}
    </ol>
  );
}

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
}: {
  onDone?: () => void;
  pollMs?: number;
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
          setError(err instanceof ApiError ? err.message : "Setup check failed");
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
      setError(err instanceof ApiError ? err.message : "Setup failed");
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
      setError(err instanceof ApiError ? err.message : "Sign-in failed");
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
      setError(err instanceof ApiError ? err.message : "Server registration failed");
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

  const stepIndex = STEPS.findIndex((s) => s.key === phase.name);
  const step = stepIndex >= 0 ? STEPS[stepIndex] : undefined;

  return (
    <AuthLayout wide>
      <div className="rounded-xl border bg-card shadow-overlay">
        <div className="border-b px-6 pb-5 pt-6 sm:px-8">
          <h1 className="text-xl font-semibold tracking-tight">Set up Setu</h1>
          <p className="mt-1 text-sm text-muted-foreground">
            {step !== undefined
              ? `Step ${stepIndex + 1} of 4: ${step.long}`
              : "Four short steps to a running control panel."}
          </p>
          {step !== undefined ? (
            <div className="mt-5">
              <SetupStepper current={stepIndex + 1} />
            </div>
          ) : null}
        </div>

        <div className="flex flex-col gap-5 px-6 py-6 sm:px-8">
          {phase.name === "checking" && error === null ? (
            <p role="status" className="flex items-center gap-2 text-sm text-muted-foreground">
              <Loader2 className="size-4 animate-spin" aria-hidden="true" />
              Checking setup status…
            </p>
          ) : null}
          {error !== null ? <Alert variant="destructive">{error}</Alert> : null}

          {phase.name === "token" ? (
            <form
              aria-label="Token"
              className="flex flex-col gap-5"
              onSubmit={(e) => {
                e.preventDefault();
                if (token.trim() === "") return;
                setError(null);
                setPhase({ name: "owner" });
              }}
            >
              <p className="text-sm text-muted-foreground">
                The installer printed a one-time setup token. It is also in{" "}
                <code className="text-foreground">/var/lib/setu/setup-token</code> on the panel
                host (readable by root and the <code className="text-foreground">setu</code> user
                only).
              </p>
              <Field label="Setup token">
                <Input
                  type="password"
                  autoComplete="off"
                  autoFocus
                  className="font-mono"
                  value={token}
                  onChange={(e) => setToken(e.target.value)}
                  required
                />
              </Field>
              <Button type="submit" className="self-stretch sm:self-end">
                Continue
                <ArrowRight aria-hidden="true" />
              </Button>
            </form>
          ) : null}

          {phase.name === "owner" ? (
            <form
              aria-label="Create owner"
              className="flex flex-col gap-5"
              onSubmit={(e) => void createOwner(e)}
            >
              <p className="text-sm text-muted-foreground">
                Create the first operator: the owner, who can do everything. This step works
                exactly once.
              </p>
              <Field label="Email">
                <Input
                  type="email"
                  autoComplete="username"
                  autoFocus
                  placeholder="you@example.com"
                  value={email}
                  onChange={(e) => setEmail(e.target.value)}
                  required
                />
              </Field>
              <Field label="Password" hint="At least 8 characters.">
                <Input
                  type="password"
                  autoComplete="new-password"
                  minLength={8}
                  value={password}
                  onChange={(e) => setPassword(e.target.value)}
                  required
                />
              </Field>
              <div className="flex flex-col-reverse gap-2 sm:flex-row sm:justify-end">
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
                <Button type="submit" loading={busy}>
                  {busy ? "Creating…" : "Create owner"}
                </Button>
              </div>
            </form>
          ) : null}

          {phase.name === "server" ? (
            <div className="flex flex-col gap-5">
              <Alert variant="success" role="status" title="Owner created and signed in">
                The panel lives at the address below.
              </Alert>
              <CopyField value={phase.publicUrl} label="Panel URL" />
              {phase.install === null ? (
                <>
                  <p className="text-sm text-muted-foreground">
                    Register this machine as the first server; its agent joins the panel like
                    any other server.
                  </p>
                  <Button
                    type="button"
                    loading={busy}
                    onClick={() => void registerServer(phase.publicUrl)}
                    className="self-stretch sm:self-end"
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
                  <p
                    className="flex items-center gap-2 rounded-md border border-dashed px-3 py-2.5 text-xs text-muted-foreground"
                    aria-live="polite"
                  >
                    <Loader2 className="size-3.5 shrink-0 animate-spin" aria-hidden="true" />
                    Waiting for the agent to join — this page continues on its own once the
                    server is online.
                  </p>
                </>
              )}
            </div>
          ) : null}

          {phase.name === "done" ? (
            <div className="flex flex-col items-center gap-3 py-4 text-center">
              <CheckCircle2 className="size-10 text-success" aria-hidden="true" />
              <p className="text-base font-semibold">Server is online. Setup is finished.</p>
              <p className="text-sm text-muted-foreground">
                Publish an app and create your first site.
              </p>
              <a
                href="/sites"
                className="mt-2 inline-flex h-10 items-center gap-2 rounded-md bg-primary px-5 text-sm font-medium text-primary-foreground hover:bg-primary-hover"
              >
                Go to sites
                <ArrowRight className="size-4" aria-hidden="true" />
              </a>
            </div>
          ) : null}

          {phase.name === "complete" ? (
            <div className="flex flex-col items-center gap-3 py-4 text-center">
              <CheckCircle2 className="size-10 text-success" aria-hidden="true" />
              <p className="text-sm text-muted-foreground">Setup is already complete.</p>
              <a
                href="/sites"
                className="inline-flex h-10 items-center gap-2 rounded-md bg-primary px-5 text-sm font-medium text-primary-foreground hover:bg-primary-hover"
              >
                Go to sites
                <ArrowRight className="size-4" aria-hidden="true" />
              </a>
            </div>
          ) : null}
        </div>
      </div>
    </AuthLayout>
  );
}
