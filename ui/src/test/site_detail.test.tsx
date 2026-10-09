import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import * as React from "react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { SiteBackups } from "@/components/SiteBackups";
import { SiteEnv } from "@/components/SiteEnv";
import { SiteLogs } from "@/components/SiteLogs";
import { SiteUpdates } from "@/components/SiteUpdates";
import { ConfirmProvider } from "@/components/ui/dialog";

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

const envBody = {
  env: [
    {
      name: "PORT",
      value: "20001",
      masked: false,
      secret: false,
      once: false,
      has_value: true,
    },
    {
      name: "VYASA_ADMIN_PASSWORD",
      value: "[MASKED_VALUE]",
      masked: true,
      secret: true,
      once: true,
      has_value: true,
    },
  ],
};

const backupsBody = {
  backups: [
    {
      id: "b1",
      site_id: "s1",
      taken_at: "2026-10-05T00:00:00Z",
      location: "/var/lib/setu/backups/s1/ts",
      kind: "manual",
      status: "completed",
      size_bytes: 1024,
    },
  ],
};

const versionsBody = {
  versions: [
    {
      id: "v2",
      app_id: "hello",
      version: "0.1.1",
      channel: "stable",
      published_at: "2026-10-02T00:00:00Z",
    },
    {
      id: "v1",
      app_id: "hello",
      version: "0.1.0",
      channel: "stable",
      published_at: "2026-10-01T00:00:00Z",
    },
  ],
};

function renderWithClient(ui: React.ReactElement) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  });
  return render(
    <QueryClientProvider client={client}>
      <ConfirmProvider>{ui}</ConfirmProvider>
    </QueryClientProvider>,
  );
}

describe("site detail tabs", () => {
  beforeEach(() => {
    MockEventSource.instances = [];
    vi.stubGlobal("EventSource", MockEventSource);
  });

  it("logs subscribe to SSE and append lines, pausing on demand", async () => {
    renderWithClient(<SiteLogs siteId="s1" />);
    await waitFor(() => expect(MockEventSource.instances).toHaveLength(1));
    expect(MockEventSource.instances[0]?.url).toBe("/api/v1/sites/s1/logs");

    MockEventSource.instances[0]?.emit("first line");
    MockEventSource.instances[0]?.emit("second line");
    expect(await screen.findByText(/first line/)).toBeInTheDocument();
    expect(screen.getByText(/second line/)).toBeInTheDocument();

    const user = userEvent.setup();
    await user.click(screen.getByRole("button", { name: /pause/i }));
    expect(MockEventSource.instances[0]?.closed).toBe(true);
    // Buffered lines survive the pause.
    expect(screen.getByText(/first line/)).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: /resume/i }));
    await waitFor(() => expect(MockEventSource.instances).toHaveLength(2));
  });

  it("env masks secrets and reveals an editor only after confirm", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async (url: unknown) => {
        if (String(url).endsWith("/api/v1/sites/s1/env")) return json(envBody);
        return new Response("not found", { status: 404 });
      }),
    );
    const user = userEvent.setup();
    renderWithClient(<SiteEnv siteId="s1" />);
    await screen.findByText("VYASA_ADMIN_PASSWORD");

    expect(screen.getByText("[MASKED_VALUE]")).toBeInTheDocument();
    expect(screen.queryByText("sup3r-secret")).not.toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: /reveal/i }));
    // Confirm dialog appears before anything editable does.
    expect(await screen.findByText(/will be visible on screen/i)).toBeInTheDocument();
    expect(screen.queryByPlaceholderText(/new value/i)).not.toBeInTheDocument();
    await user.click(screen.getByTestId("confirm-accept"));
    expect(await screen.findByPlaceholderText(/new value/i)).toBeInTheDocument();
  });

  it("env save puts the edited values and reports the restart job", async () => {
    const puts: unknown[] = [];
    vi.stubGlobal(
      "fetch",
      vi.fn(async (url: unknown, init?: RequestInit) => {
        const path = String(url);
        if (path.endsWith("/api/v1/sites/s1/env")) {
          if (init?.method === "PUT") {
            puts.push(JSON.parse(String(init.body)));
            return json(
              {
                site: { id: "s1" },
                job: { id: "job-env", status: "queued" },
              },
              202,
            );
          }
          return json(envBody);
        }
        return new Response("not found", { status: 404 });
      }),
    );
    const user = userEvent.setup();
    const onSubmitted = vi.fn();
    renderWithClient(<SiteEnv siteId="s1" onSubmitted={onSubmitted} />);
    await screen.findByText("PORT");
    await user.click(screen.getByRole("button", { name: /edit/i }));
    await user.type(screen.getByLabelText(/new value for port/i), "20002");
    await user.click(screen.getByRole("button", { name: /save changes/i }));
    await waitFor(() => expect(puts).toHaveLength(1));
    expect(puts[0]).toEqual({ env: { PORT: "20002" } });
    expect(onSubmitted).toHaveBeenCalledWith("job-env");
  });

  it("backups trigger restore only after a confirmation naming the backup", async () => {
    const posts: string[] = [];
    vi.stubGlobal(
      "fetch",
      vi.fn(async (url: unknown, init?: RequestInit) => {
        const path = String(url);
        if (path.endsWith("/api/v1/sites/s1/backups")) return json(backupsBody);
        if (path.endsWith("/restore") && init?.method === "POST") {
          posts.push(path);
          return json({ job: { id: "job-restore", status: "queued" } }, 202);
        }
        return new Response("not found", { status: 404 });
      }),
    );
    const user = userEvent.setup();
    const onJob = vi.fn();
    renderWithClient(<SiteBackups siteId="s1" onJob={onJob} />);
    await screen.findByText("b1");
    await user.click(screen.getByRole("button", { name: /restore/i }));
    // The confirmation names the backup under restore.
    expect(await screen.findByText(/restore backup b1\?/i)).toBeInTheDocument();
    expect(posts).toHaveLength(0);
    await user.click(screen.getByTestId("confirm-accept"));
    await waitFor(() => expect(posts).toHaveLength(1));
    expect(posts[0]).toContain("/api/v1/sites/s1/backups/b1/restore");
    expect(onJob).toHaveBeenCalledWith("job-restore");
  });

  it("updates list versions with the current highlighted and start an update", async () => {
    const posts: unknown[] = [];
    vi.stubGlobal(
      "fetch",
      vi.fn(async (url: unknown, init?: RequestInit) => {
        const path = String(url);
        if (path.endsWith("/api/v1/apps/hello/versions")) return json(versionsBody);
        if (path.endsWith("/api/v1/sites/s1/update") && init?.method === "POST") {
          posts.push(JSON.parse(String(init.body)));
          return json(
            { site: { id: "s1" }, job: { id: "job-update", status: "queued" } },
            202,
          );
        }
        return new Response("not found", { status: 404 });
      }),
    );
    const user = userEvent.setup();
    const onJob = vi.fn();
    renderWithClient(
      <SiteUpdates siteId="s1" appId="hello" currentVersion="0.1.0" onJob={onJob} />,
    );
    await screen.findByText("0.1.1");
    expect(screen.getByText(/current/i)).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: /update to 0\.1\.1/i }));
    // The update asks for confirmation naming the version; the dialog
    // button shares the row button's label, so scope to the dialog.
    const dialog = await screen.findByRole("dialog");
    await user.click(
      within(dialog).getByRole("button", { name: /update to 0\.1\.1/i }),
    );
    await waitFor(() => expect(posts).toHaveLength(1));
    expect(posts[0]).toEqual({ version: "0.1.1" });
    expect(onJob).toHaveBeenCalledWith("job-update");
  });
});
