import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { render } from "@testing-library/react";
import * as React from "react";
import { vi } from "vitest";
import { ConfirmProvider } from "@/components/ui/dialog";

export function json(body: unknown, status = 200) {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json" },
  });
}

export function notFound() {
  return json({ error: { code: "not_found", message: "nope" } }, 404);
}

export interface Call {
  method: string;
  url: string;
  body: unknown;
}

/**
 * Stubs `fetch` with `handler`; every call is recorded (method, path,
 * parsed JSON body) in the returned array.
 */
export function stubFetch(
  handler: (url: string, method: string, body: unknown) => Response,
): Call[] {
  const calls: Call[] = [];
  vi.stubGlobal(
    "fetch",
    vi.fn(async (url: unknown, init?: RequestInit) => {
      const method = init?.method ?? "GET";
      const raw = init?.body;
      const body = typeof raw === "string" ? (JSON.parse(raw) as unknown) : undefined;
      const path = String(url);
      calls.push({ method, url: path, body });
      return handler(path, method, body);
    }),
  );
  return calls;
}

/** Controllable EventSource stand-in: tests emit lines via `emit`. */
export class MockEventSource {
  static instances: MockEventSource[] = [];
  url: string;
  onmessage: ((event: { data: string }) => void) | null = null;
  onopen: (() => void) | null = null;
  onerror: (() => void) | null = null;
  closed = false;

  constructor(url: string) {
    this.url = url;
    MockEventSource.instances.push(this);
  }

  emit(data: string) {
    this.onmessage?.({ data });
  }

  /** A browser auto-reconnect on the same instance. */
  reopen() {
    this.onopen?.();
  }

  close() {
    this.closed = true;
  }

  static latest(): MockEventSource {
    const last = MockEventSource.instances[MockEventSource.instances.length - 1];
    if (last === undefined) throw new Error("no EventSource opened");
    return last;
  }
}

export function installEventSource() {
  MockEventSource.instances = [];
  vi.stubGlobal("EventSource", MockEventSource);
}

export function renderWithClient(ui: React.ReactElement) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  });
  return render(
    <QueryClientProvider client={client}>
      <ConfirmProvider>{ui}</ConfirmProvider>
    </QueryClientProvider>,
  );
}

export function installClipboard() {
  const writeText = vi.fn().mockResolvedValue(undefined);
  Object.defineProperty(navigator, "clipboard", {
    value: { writeText },
    configurable: true,
  });
  return writeText;
}

const NOW = "2026-10-09T00:00:00Z";

export function serverFixture(partial: Record<string, unknown> & { id: string }) {
  return {
    name: partial.id,
    status: "online",
    cordoned: false,
    is_control_plane: false,
    created_at: NOW,
    updated_at: NOW,
    ...partial,
  };
}

export function siteFixture(partial: Record<string, unknown> = {}) {
  return {
    id: "s1",
    name: "blog",
    app_id: "hello",
    app_version: "0.1.0",
    server_id: "srv1",
    plan_id: "plan1",
    domain: "blog.example.com",
    status: "running",
    port: 20001,
    db_name: "site_abcd1234",
    owner_email: "owner@example.com",
    failed_step: null,
    created_at: NOW,
    updated_at: NOW,
    ...partial,
  };
}

export function jobFixture(partial: Record<string, unknown> & { id: string }) {
  return {
    kind: "provision_site",
    status: "queued",
    attempts: 0,
    log: "",
    created_at: NOW,
    site_id: "s1",
    server_id: "srv1",
    started_at: null,
    finished_at: null,
    worker: null,
    error: null,
    ...partial,
  };
}

export const planFixture = {
  id: "plan1",
  name: "standard",
  limits: { memory_mb: 512, cpu_percent: 100, disk_mb: 5120 },
  allowed_app_ids: ["hello"],
  created_at: NOW,
  updated_at: NOW,
};
