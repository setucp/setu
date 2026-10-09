import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { SetupWizard } from "@/components/SetupWizard";
import { installClipboard, json, notFound, serverFixture, stubFetch } from "./helpers";

const needed = { needed: true, public_url: "https://panel.example.com" };
const owner = {
  id: "op1",
  email: "owner@example.com",
  role: "owner",
  created_at: "2026-10-09T00:00:00Z",
};
const COMMAND =
  "curl -fsSL https://panel.example.com/install.sh | sh -s -- agent install --control-plane https://panel.example.com";

describe("setup wizard", () => {
  it("shows the already-complete note when no setup is needed", async () => {
    stubFetch((url) =>
      url.endsWith("/api/v1/setup") ? json({ ...needed, needed: false }) : notFound(),
    );
    render(<SetupWizard />);
    expect(await screen.findByText(/already complete/i)).toBeInTheDocument();
  });

  it("a wrong setup token sends the wizard back to the token step", async () => {
    stubFetch((url, method) => {
      if (url.endsWith("/api/v1/setup") && method === "GET") return json(needed);
      if (url.endsWith("/api/v1/setup")) {
        return json({ error: { code: "unauthorized", message: "bad setup token" } }, 401);
      }
      return notFound();
    });
    const user = userEvent.setup();
    render(<SetupWizard />);
    await user.type(await screen.findByLabelText(/setup token/i), "wrong");
    await user.click(screen.getByRole("button", { name: /continue/i }));
    await user.type(await screen.findByLabelText(/email/i), "owner@example.com");
    await user.type(screen.getByLabelText(/^password/i), "correct-horse-1");
    await user.click(screen.getByRole("button", { name: /create owner/i }));
    expect(await screen.findByRole("alert")).toHaveTextContent(/bad setup token/i);
    expect(screen.getByLabelText(/setup token/i)).toBeInTheDocument();
  }, 20000);

  it("setup wizard: token → owner → local server → done", async () => {
    let polls = 0;
    let agentJoined = false;
    const calls = stubFetch((url, method) => {
      if (url.endsWith("/api/v1/setup") && method === "GET") return json(needed);
      if (url.endsWith("/api/v1/setup")) return json(owner, 201);
      if (url.endsWith("/api/v1/auth/login")) return json(owner);
      if (url.endsWith("/api/v1/setup/local-server")) {
        return json(
          {
            server: serverFixture({
              id: "srv1",
              name: "local",
              status: "pending",
              is_control_plane: true,
            }),
            join_token: "setu_join_abc",
            install_command: COMMAND,
          },
          201,
        );
      }
      if (url.endsWith("/api/v1/servers") && method === "GET") {
        polls += 1;
        return json({
          servers: [
            serverFixture({ id: "other", status: "online" }),
            serverFixture({
              id: "srv1",
              name: "local",
              is_control_plane: true,
              status: agentJoined ? "online" : "pending",
            }),
          ],
        });
      }
      return notFound();
    });
    const onDone = vi.fn();
    const user = userEvent.setup();
    const writeText = installClipboard();
    render(<SetupWizard onDone={onDone} pollMs={30} />);

    // Step 1: the setup token, alone.
    expect(await screen.findByText(/step 1 of 4/i)).toBeInTheDocument();
    expect(screen.queryByLabelText(/email/i)).not.toBeInTheDocument();
    await user.type(screen.getByLabelText(/setup token/i), "tok-123");
    await user.click(screen.getByRole("button", { name: /continue/i }));

    // Step 2: the owner; the token rides along in the same POST.
    expect(await screen.findByText(/step 2 of 4/i)).toBeInTheDocument();
    await user.type(screen.getByLabelText(/email/i), "owner@example.com");
    await user.type(screen.getByLabelText(/^password/i), "correct-horse-1");
    await user.click(screen.getByRole("button", { name: /create owner/i }));
    await waitFor(() =>
      expect(calls.find((c) => c.method === "POST" && c.url.endsWith("/api/v1/setup"))?.body).toEqual({
        token: "tok-123",
        email: "owner@example.com",
        password: "correct-horse-1",
      }),
    );

    // Step 3: the local server: panel URL, then command and token apart.
    expect(await screen.findByText(/step 3 of 4/i)).toBeInTheDocument();
    expect(screen.getByText("https://panel.example.com")).toBeInTheDocument();
    expect(calls.some((c) => c.url.endsWith("/api/v1/auth/login"))).toBe(true);
    await user.click(screen.getByRole("button", { name: /register this server/i }));
    const command = await screen.findByTestId("install-command");
    expect(command).toHaveTextContent(COMMAND);
    expect(command).not.toHaveTextContent("setu_join_abc");
    expect(screen.getByTestId("join-token")).toHaveTextContent("setu_join_abc");
    expect(screen.getByText(/SETU_JOIN_TOKEN=<token>/)).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: /copy install command/i }));
    expect(writeText).toHaveBeenLastCalledWith(COMMAND);
    await user.click(screen.getByRole("button", { name: /copy join token/i }));
    expect(writeText).toHaveBeenLastCalledWith("setu_join_abc");
    // Still waiting: the list has been polled, the server is pending.
    await waitFor(() => expect(polls).toBeGreaterThanOrEqual(1));
    expect(screen.getByText(/step 3 of 4/i)).toBeInTheDocument();
    agentJoined = true;

    // Polls GET /servers until *this* server is online → step 4.
    expect(await screen.findByText(/step 4 of 4/i, {}, { timeout: 3000 })).toBeInTheDocument();
    expect(polls).toBeGreaterThanOrEqual(2);
    expect(screen.getByText(/server is online/i)).toBeInTheDocument();
    expect(onDone).toHaveBeenCalled();
  }, 20000);
});
