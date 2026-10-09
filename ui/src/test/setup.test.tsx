import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { SetupWizard } from "@/components/SetupWizard";

function json(body: unknown, status = 200) {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json" },
  });
}

function stubFetch(handler: (url: string, init?: RequestInit) => Response) {
  vi.stubGlobal(
    "fetch",
    vi.fn(async (url: unknown, init?: RequestInit) => handler(String(url), init)),
  );
}

const needed = { needed: true, public_url: "http://127.0.0.1:7000" };

describe("setup wizard", () => {
  it("renders the owner form when setup is needed", async () => {
    stubFetch((url) => {
      if (url.endsWith("/api/v1/setup")) return json(needed);
      return json({ error: { code: "not_found", message: "nope" } }, 404);
    });
    render(<SetupWizard />);
    expect(await screen.findByLabelText(/email/i)).toBeInTheDocument();
    expect(screen.getByLabelText(/^password/i)).toBeInTheDocument();
  });

  it("shows the already-complete note when no setup is needed", async () => {
    stubFetch((url) => {
      if (url.endsWith("/api/v1/setup")) {
        return json({ needed: false, public_url: "http://127.0.0.1:7000" });
      }
      return json({ error: { code: "not_found", message: "nope" } }, 404);
    });
    render(<SetupWizard />);
    expect(await screen.findByText(/already complete/i)).toBeInTheDocument();
  });

  it("creates the owner, logs in, and shows the install command", async () => {
    const calls: string[] = [];
    stubFetch((url, init) => {
      calls.push(`${init?.method ?? "GET"} ${url}`);
      if (url.endsWith("/api/v1/setup") && (init?.method ?? "GET") === "GET") {
        // First status check says needed; later ones (after login) too.
        return json(needed);
      }
      if (url.endsWith("/api/v1/setup")) {
        return json(
          {
            id: "op1",
            email: "owner@example.com",
            role: "owner",
            created_at: new Date().toISOString(),
          },
          201,
        );
      }
      if (url.endsWith("/api/v1/auth/login")) {
        return json({
          id: "op1",
          email: "owner@example.com",
          role: "owner",
          created_at: new Date().toISOString(),
        });
      }
      if (url.endsWith("/api/v1/setup/local-server")) {
        return json(
          {
            server: {
              id: "srv1",
              name: "local-server",
              status: "pending",
              created_at: new Date().toISOString(),
              updated_at: new Date().toISOString(),
            },
            join_token: "setu_join_abc",
            install_command:
              "setu agent install --control-plane http://127.0.0.1:7000 --join-token setu_join_abc",
          },
          201,
        );
      }
      if (url.endsWith("/api/v1/servers/srv1")) {
        return json({
          id: "srv1",
          name: "local-server",
          status: "online",
          created_at: new Date().toISOString(),
          updated_at: new Date().toISOString(),
        });
      }
      return json({ error: { code: "not_found", message: "nope" } }, 404);
    });
    const onDone = vi.fn();
    // Real timers: the wizard polls every 2 s, so the online transition
    // lands a little after that (generous waitFor below).
    render(<SetupWizard onDone={onDone} />);
    await userEvent.type(
      await screen.findByLabelText(/email/i),
      "owner@example.com",
    );
    await userEvent.type(screen.getByLabelText(/^password/i), "correct-horse-1");
    await userEvent.click(screen.getByRole("button", { name: /create owner/i }));

    await waitFor(
      () =>
        expect(screen.getByText(/register this server/i)).toBeInTheDocument(),
      { timeout: 8000 },
    );
    expect(calls).toContain("POST /api/v1/setup");
    expect(calls).toContain("POST /api/v1/auth/login");

    await userEvent.click(screen.getByRole("button", { name: /register this server/i }));
    await waitFor(
      () =>
        expect(screen.getByText(/setu agent install/i)).toBeInTheDocument(),
      { timeout: 8000 },
    );

    // The 2 s poll reads the server back as online → done.
    await waitFor(() => expect(onDone).toHaveBeenCalled(), { timeout: 8000 });
  }, 20000);
});
