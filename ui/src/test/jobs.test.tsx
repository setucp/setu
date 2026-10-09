import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import * as React from "react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { JobLogView } from "@/components/JobLogView";
import { JobsTable } from "@/components/JobsTable";
import type { Job } from "@/lib/client";

/** Controllable EventSource stand-in: tests emit lines via `emit`. */
class MockEventSource {
  static instances: MockEventSource[] = [];
  url: string;
  onmessage: ((event: { data: string }) => void) | null = null;
  closed = false;

  constructor(url: string) {
    this.url = url;
    MockEventSource.instances.push(this);
  }

  emit(data: string) {
    this.onmessage?.({ data });
  }

  close() {
    this.closed = true;
  }
}

function json(body: unknown, status = 200) {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json" },
  });
}

function job(partial: Partial<Job> & { id: string }): Job {
  return {
    kind: "provision_site",
    status: "queued",
    attempts: 0,
    log: "",
    created_at: "2026-10-09T00:00:00Z",
    site_id: "s1",
    server_id: "srv1",
    started_at: null,
    finished_at: null,
    worker: null,
    error: null,
    ...partial,
  };
}

const jobs: Job[] = [
  job({ id: "j1", status: "running", log: "step install: ok" }),
  job({ id: "j2", kind: "backup_site", status: "done", log: "step backup: ok" }),
  job({ id: "j3", kind: "update_site", status: "failed", error: "health failed" }),
];

function renderWithClient(ui: React.ReactElement) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  });
  return render(
    <QueryClientProvider client={client}>{ui}</QueryClientProvider>,
  );
}

describe("jobs", () => {
  beforeEach(() => {
    MockEventSource.instances = [];
    vi.stubGlobal("EventSource", MockEventSource);
  });

  it("table separates running from recent with status badges and links", () => {
    renderWithClient(<JobsTable jobs={jobs} />);
    expect(screen.getByText(/running/i, { selector: "h2" })).toBeInTheDocument();
    const runningRow = screen.getByRole("row", { name: /provision_site.*running/ });
    expect(
      within(runningRow).getByRole("link", { name: /j1/ }),
    ).toHaveAttribute("href", "/jobs/j1");
    expect(screen.getByText("health failed")).toBeInTheDocument();
  });

  it("job page shows status, error and a live SSE log tail", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async (url: unknown) => {
        if (String(url).endsWith("/api/v1/jobs/j1")) {
          return json(
            job({ id: "j1", status: "running", log: "step install: ok" }),
          );
        }
        return new Response("not found", { status: 404 });
      }),
    );
    const user = userEvent.setup();
    void user;
    renderWithClient(<JobLogView jobId="j1" />);
    expect(await screen.findByText(/provision_site/)).toBeInTheDocument();
    expect(screen.getByText(/step install: ok/)).toBeInTheDocument();

    await waitFor(() => expect(MockEventSource.instances).toHaveLength(1));
    expect(MockEventSource.instances[0]?.url).toBe("/api/v1/jobs/j1/log");
    MockEventSource.instances[0]?.emit("step health: ok");
    expect(await screen.findByText(/step health: ok/)).toBeInTheDocument();
  });

  it("finished jobs do not subscribe to the live log", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async (url: unknown) => {
        if (String(url).endsWith("/api/v1/jobs/j2")) {
          return json(job({ id: "j2", kind: "backup_site", status: "done", log: "all done" }));
        }
        return new Response("not found", { status: 404 });
      }),
    );
    renderWithClient(<JobLogView jobId="j2" />);
    expect(await screen.findByText(/all done/)).toBeInTheDocument();
    await waitFor(() => expect(screen.getByText(/backup_site/)).toBeInTheDocument());
    // Give the effect a chance to (incorrectly) subscribe.
    await new Promise((resolve) => setTimeout(resolve, 50));
    expect(MockEventSource.instances).toHaveLength(0);
  });
});
