import { cleanup, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import { AddServerDialog } from "@/components/AddServerDialog";
import { ServerDetail } from "@/components/ServerDetail";
import { ServersTable } from "@/components/ServersTable";
import type { Plan, Server, Site } from "@/lib/client";
import {
  installClipboard,
  json,
  notFound,
  planFixture,
  renderWithClient,
  serverFixture,
  siteFixture,
  stubFetch,
} from "./helpers";

const servers = [
  serverFixture({
    id: "srv1",
    name: "web1",
    host: "203.0.113.7",
    status: "online",
    is_control_plane: true,
    agent_version: "0.1.0",
    cpu_cores: 4,
    memory_mb: 8192,
    disk_mb: 81920,
    mem_free_mb: 4096,
    disk_free_mb: 40960,
    load1: 0.42,
    last_seen_at: "2026-10-09T00:00:00Z",
  }),
  serverFixture({ id: "srv2", name: "web2", status: "pending" }),
  serverFixture({ id: "srv3", name: "web3", status: "offline", cordoned: true }),
] as Server[];

const plans = [planFixture] as Plan[];

function site(id: string, serverId: string): Site {
  return siteFixture({ id, name: id, domain: `${id}.example.com`, server_id: serverId }) as Site;
}

const COMMAND = "setu agent install --control-plane https://panel.example.com";

describe("servers", () => {
  it("table shows status, control plane, cordon, load and capacity", () => {
    renderWithClient(
      <ServersTable servers={servers} sites={[site("a", "srv1"), site("b", "srv1")]} plans={plans} />,
    );
    const web1 = screen.getByRole("row", { name: /web1/ });
    expect(within(web1).getByText("online")).toBeInTheDocument();
    expect(within(web1).getByText(/control plane/i)).toBeInTheDocument();
    expect(within(web1).getByText(/2 sites/)).toBeInTheDocument();
    expect(within(web1).getByText(/1\.0G/)).toBeInTheDocument();
    expect(within(web1).getByText("0.42")).toBeInTheDocument();
    expect(within(web1).getByText(/4\.0G free/)).toBeInTheDocument();
    expect(within(web1).getByText(/40\.0G free/)).toBeInTheDocument();

    const web2 = screen.getByRole("row", { name: /web2/ });
    expect(within(web2).getByText("pending")).toBeInTheDocument();
    expect(within(web2).queryByText(/control plane/i)).not.toBeInTheDocument();

    const web3 = screen.getByRole("row", { name: /web3/ });
    expect(within(web3).getByText("offline")).toBeInTheDocument();
    expect(within(web3).getByText(/cordoned/i)).toBeInTheDocument();
  });

  it("servers: add dialog with install command and separate token, cordon toggle, control-plane badge", async () => {
    // 1. Add dialog: the command carries no token; the token is its own line.
    const calls = stubFetch((url, method) => {
      if (url.endsWith("/api/v1/servers") && method === "POST") {
        return json(
          {
            server: serverFixture({ id: "srv9", name: "web9", status: "pending" }),
            join_token: "join-abc",
            install_command: COMMAND,
          },
          201,
        );
      }
      if (url.endsWith("/api/v1/servers/srv1/cordon") && method === "POST") {
        return json({ ...servers[0], cordoned: true });
      }
      return notFound();
    });
    const user = userEvent.setup();
    const writeText = installClipboard();
    renderWithClient(<AddServerDialog open onClose={() => undefined} />);
    await user.type(screen.getByLabelText(/server name/i), "web9");
    await user.click(screen.getByRole("button", { name: /add server/i }));
    const command = await screen.findByTestId("install-command");
    expect(calls.find((c) => c.method === "POST")?.body).toEqual({ name: "web9" });
    expect(command).toHaveTextContent(COMMAND);
    expect(command).not.toHaveTextContent("join-abc");
    expect(screen.getByTestId("join-token")).toHaveTextContent("join-abc");
    expect(screen.getByText(/SETU_JOIN_TOKEN=<token>/)).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: /copy install command/i }));
    expect(writeText).toHaveBeenLastCalledWith(COMMAND);
    await user.click(screen.getByRole("button", { name: /copy join token/i }));
    expect(writeText).toHaveBeenLastCalledWith("join-abc");
    cleanup();

    // 2. Detail: control-plane badge and a confirmed cordon toggle.
    renderWithClient(<ServerDetail server={servers[0]!} sites={[site("a", "srv1")]} />);
    expect(screen.getByText(/control plane/i)).toBeInTheDocument();
    expect(screen.getByText("a.example.com")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: /^cordon/i }));
    const dialog = await screen.findByRole("dialog");
    expect(dialog).toHaveTextContent(/cordon web1\?/i);
    expect(calls.some((c) => c.url.endsWith("/cordon"))).toBe(false);
    await user.click(within(dialog).getByTestId("confirm-accept"));
    await waitFor(() =>
      expect(calls.find((c) => c.url.endsWith("/api/v1/servers/srv1/cordon"))?.body).toEqual({
        cordoned: true,
      }),
    );
  }, 20000);

  it("a cordoned server offers uncordon", async () => {
    const calls = stubFetch((url) =>
      url.endsWith("/cordon") ? json({ ...servers[2], cordoned: false }) : notFound(),
    );
    const user = userEvent.setup();
    renderWithClient(<ServerDetail server={servers[2]!} sites={[]} />);
    await user.click(screen.getByRole("button", { name: /uncordon/i }));
    await user.click(await screen.findByTestId("confirm-accept"));
    await waitFor(() =>
      expect(calls.find((c) => c.url.endsWith("/api/v1/servers/srv3/cordon"))?.body).toEqual({
        cordoned: false,
      }),
    );
  });
});
