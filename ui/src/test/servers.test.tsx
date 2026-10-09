import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import * as React from "react";
import { describe, expect, it, vi } from "vitest";
import { AddServerDialog } from "@/components/AddServerDialog";
import { ServerDetail } from "@/components/ServerDetail";
import { ServersTable } from "@/components/ServersTable";
import type { Plan, Server, Site } from "@/lib/client";

function json(body: unknown, status = 200) {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json" },
  });
}

const servers: Server[] = [
  {
    id: "srv1",
    name: "web1",
    host: "203.0.113.7",
    status: "online",
    agent_version: "0.1.0",
    arch: "x86_64",
    os: "linux",
    cpu_cores: 4,
    memory_mb: 8192,
    disk_mb: 81920,
    mem_free_mb: 4096,
    disk_free_mb: 40000,
    load1: 0.5,
    last_seen_at: "2026-10-09T00:00:00Z",
    created_at: "2026-10-01T00:00:00Z",
    updated_at: "2026-10-09T00:00:00Z",
  },
  {
    id: "srv2",
    name: "web2",
    status: "pending",
    created_at: "2026-10-01T00:00:00Z",
    updated_at: "2026-10-01T00:00:00Z",
  },
];

function site(id: string, serverId: string, planId: string): Site {
  return {
    id,
    name: id,
    app_id: "hello",
    app_version: "0.1.0",
    server_id: serverId,
    plan_id: planId,
    domain: `${id}.example.com`,
    status: "running",
    port: 20001,
    data_dir: `/srv/setu/sites/${id}`,
    db_name: `site_${id}`,
    owner_email: "owner@example.com",
    failed_step: null,
    created_at: "2026-10-01T00:00:00Z",
    updated_at: "2026-10-01T00:00:00Z",
  };
}

const plans: Plan[] = [
  {
    id: "plan1",
    name: "standard",
    limits: { memory_mb: 512, cpu_percent: 100, disk_mb: 5120 },
    allowed_app_ids: ["hello"],
    created_at: "2026-10-01T00:00:00Z",
    updated_at: "2026-10-01T00:00:00Z",
  },
];

function renderWithClient(ui: React.ReactElement) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  });
  return render(
    <QueryClientProvider client={client}>{ui}</QueryClientProvider>,
  );
}

describe("servers", () => {
  it("table shows heartbeat, site counts and capacity", () => {
    renderWithClient(
      <ServersTable
        servers={servers}
        sites={[site("a", "srv1", "plan1"), site("b", "srv1", "plan1")]}
        plans={plans}
      />,
    );
    const row = screen.getByRole("row", { name: /web1/ });
    expect(within(row).getByText("online")).toBeInTheDocument();
    expect(within(row).getByText("203.0.113.7")).toBeInTheDocument();
    // Two sites on web1, none on web2.
    expect(within(row).getByText(/2 sites/)).toBeInTheDocument();
    // Reserved plan memory (2 × 512M = 1.0G) is shown against capacity.
    expect(within(row).getByText(/1\.0G/)).toBeInTheDocument();
  });

  it("add-server dialog shows the install command with a copy button", async () => {
    const posts: unknown[] = [];
    vi.stubGlobal(
      "fetch",
      vi.fn(async (url: unknown, init?: RequestInit) => {
        if (String(url).endsWith("/api/v1/servers") && init?.method === "POST") {
          posts.push(JSON.parse(String(init.body)));
          return json(
            {
              server: { id: "srv9", name: "web9", status: "pending" },
              join_token: "join-abc",
              install_command: "curl https://panel.example.com | sh -s -- join-abc",
            },
            201,
          );
        }
        return new Response("not found", { status: 404 });
      }),
    );
    const writeText = vi.fn().mockResolvedValue(undefined);
    const user = userEvent.setup();
    // Install after userEvent.setup(): setup replaces navigator.clipboard
    // with its own stub, so a mock defined earlier would never be called.
    Object.defineProperty(navigator, "clipboard", {
      value: { writeText },
      configurable: true,
    });
    renderWithClient(<AddServerDialog open onClose={() => undefined} />);
    await user.type(screen.getByLabelText(/server name/i), "web9");
    await user.click(screen.getByRole("button", { name: /add server/i }));
    await waitFor(() => expect(posts).toHaveLength(1));
    expect(posts[0]).toEqual({ name: "web9" });
    expect(await screen.findByText(/curl https:\/\/panel\.example\.com/)).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: /copy/i }));
    expect(writeText).toHaveBeenCalledWith(
      "curl https://panel.example.com | sh -s -- join-abc",
    );
  });

  it("server detail shows heartbeat, capacity and its sites", () => {
    renderWithClient(
      <ServerDetail server={servers[0]!} sites={[site("a", "srv1", "plan1")]} />,
    );
    expect(screen.getByText("web1")).toBeInTheDocument();
    expect(screen.getByText(/0\.1\.0/)).toBeInTheDocument();
    expect(screen.getByText(/4096/)).toBeInTheDocument();
    expect(screen.getByText("a.example.com")).toBeInTheDocument();
  });
});
