import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import { SettingsView } from "@/components/SettingsView";
import { json, notFound, renderWithClient, stubFetch } from "./helpers";

const me = { id: "op1", email: "owner@example.com", role: "owner", created_at: "2026-10-01T00:00:00Z" };
const other = { id: "op2", email: "ops@example.com", role: "operator", created_at: "2026-10-02T00:00:00Z" };

describe("settings", () => {
  it("settings: operators, password change", async () => {
    let operators = [me, other];
    const calls = stubFetch((url, method, body) => {
      if (url.endsWith("/api/v1/auth/me")) return json(me);
      if (url.endsWith("/api/v1/setup")) return json({ needed: false, public_url: "https://panel.example.com" });
      if (url.includes("/api/v1/audit")) return json({ audit: [] });
      if (url.endsWith("/api/v1/operators") && method === "POST") {
        const created = { id: "op3", created_at: "2026-10-09T00:00:00Z", ...(body as object) };
        operators = [...operators, created as typeof me];
        return json(created, 201);
      }
      if (url.endsWith("/api/v1/operators")) return json({ operators });
      if (url.endsWith("/password") && method === "PUT") {
        const b = body as { current_password?: string };
        if (url.includes("/op1/") && b.current_password !== "old-password-1") {
          return json({ error: { code: "forbidden", message: "current password is wrong" } }, 403);
        }
        return new Response(null, { status: 204 });
      }
      return notFound();
    });
    const user = userEvent.setup();
    renderWithClient(<SettingsView />);

    // Panel URL and operators.
    expect(await screen.findByText("https://panel.example.com")).toBeInTheDocument();
    expect(await screen.findByText("ops@example.com")).toBeInTheDocument();

    // Create.
    await user.click(screen.getByRole("button", { name: /add operator/i }));
    let dialog = await screen.findByRole("dialog");
    await user.type(within(dialog).getByLabelText(/email/i), "viewer@example.com");
    await user.type(within(dialog).getByLabelText(/password/i), "viewer-pass-1");
    await user.selectOptions(within(dialog).getByLabelText(/role/i), "viewer");
    await user.click(within(dialog).getByRole("button", { name: /create operator/i }));
    expect(await screen.findByText("viewer@example.com")).toBeInTheDocument();
    expect(calls.find((c) => c.method === "POST")?.body).toEqual({
      email: "viewer@example.com",
      password: "viewer-pass-1",
      role: "viewer",
    });

    // Someone else's password: the owner sets it, no current password.
    const otherRow = screen.getByRole("row", { name: /ops@example\.com/ });
    await user.click(within(otherRow).getByRole("button", { name: /change password/i }));
    dialog = await screen.findByRole("dialog");
    expect(dialog).toHaveTextContent(/ops@example\.com/);
    expect(within(dialog).queryByLabelText(/current password/i)).not.toBeInTheDocument();
    await user.type(within(dialog).getByLabelText(/^new password/i), "brand-new-pass");
    await user.type(within(dialog).getByLabelText(/confirm new password/i), "brand-new-pass");
    await user.click(within(dialog).getByRole("button", { name: /set password/i }));
    await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
    expect(calls.find((c) => c.url.endsWith("/api/v1/operators/op2/password"))?.body).toEqual({
      password: "brand-new-pass",
    });

    // Your own: the current password is required; a mismatch shows.
    const meRow = screen.getByRole("row", { name: /owner@example\.com/ });
    await user.click(within(meRow).getByRole("button", { name: /change password/i }));
    dialog = await screen.findByRole("dialog");
    await user.type(within(dialog).getByLabelText(/current password/i), "wrong-one");
    await user.type(within(dialog).getByLabelText(/^new password/i), "another-pass-9");
    await user.type(within(dialog).getByLabelText(/confirm new password/i), "another-pass-9");
    await user.click(within(dialog).getByRole("button", { name: /set password/i }));
    expect(await within(dialog).findByRole("alert")).toHaveTextContent(/current password is wrong/i);
    await user.clear(within(dialog).getByLabelText(/current password/i));
    await user.type(within(dialog).getByLabelText(/current password/i), "old-password-1");
    await user.click(within(dialog).getByRole("button", { name: /set password/i }));
    await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
    expect(calls.filter((c) => c.url.endsWith("/api/v1/operators/op1/password")).at(-1)?.body).toEqual({
      current_password: "old-password-1",
      password: "another-pass-9",
    });
  }, 20000);

  it("password change refuses a mismatched confirmation locally", async () => {
    const calls = stubFetch((url) => {
      if (url.endsWith("/api/v1/auth/me")) return json(me);
      if (url.endsWith("/api/v1/setup")) return json({ needed: false, public_url: "https://p" });
      if (url.includes("/api/v1/audit")) return json({ audit: [] });
      if (url.endsWith("/api/v1/operators")) return json({ operators: [me, other] });
      return notFound();
    });
    const user = userEvent.setup();
    renderWithClient(<SettingsView />);
    const row = await screen.findByRole("row", { name: /ops@example\.com/ });
    await user.click(within(row).getByRole("button", { name: /change password/i }));
    const dialog = await screen.findByRole("dialog");
    await user.type(within(dialog).getByLabelText(/^new password/i), "brand-new-pass");
    await user.type(within(dialog).getByLabelText(/confirm new password/i), "brand-new-typo");
    await user.click(within(dialog).getByRole("button", { name: /set password/i }));
    expect(await within(dialog).findByRole("alert")).toHaveTextContent(/do not match/i);
    expect(calls.some((c) => c.method === "PUT")).toBe(false);
  }, 20000);
  it("delete operator: the confirmation names the email; not offered for yourself", async () => {
    let operators = [me, other];
    const calls = stubFetch((url, method) => {
      if (url.endsWith("/api/v1/auth/me")) return json(me);
      if (url.endsWith("/api/v1/setup")) return json({ needed: false, public_url: "https://panel.example.com" });
      if (url.includes("/api/v1/audit")) return json({ audit: [] });
      if (url.endsWith("/api/v1/operators/op2") && method === "DELETE") {
        operators = [me];
        return new Response(null, { status: 204 });
      }
      if (url.endsWith("/api/v1/operators")) return json({ operators });
      return notFound();
    });
    const user = userEvent.setup();
    renderWithClient(<SettingsView />);

    const meRow = await screen.findByRole("row", { name: /owner@example\.com/ });
    expect(within(meRow).queryByRole("button", { name: /^delete/i })).not.toBeInTheDocument();
    const otherRow = screen.getByRole("row", { name: /ops@example\.com/ });
    await user.click(within(otherRow).getByRole("button", { name: /^delete/i }));
    const dialog = await screen.findByRole("dialog");
    expect(dialog).toHaveTextContent("Delete ops@example.com?");
    await user.click(within(dialog).getByTestId("confirm-accept"));
    await waitFor(() => expect(screen.queryAllByText("ops@example.com")).toHaveLength(0));
    expect(calls.some((c) => c.method === "DELETE" && c.url.endsWith("/api/v1/operators/op2"))).toBe(true);
  });
  it("panel shows the last control-plane backup", async () => {
    stubFetch((url) => {
      if (url.endsWith("/api/v1/auth/me")) return json(me);
      if (url.endsWith("/api/v1/setup")) return json({ needed: false, public_url: "https://panel.example.com" });
      if (url.includes("/api/v1/audit")) return json({ audit: [] });
      if (url.endsWith("/api/v1/operators")) return json({ operators: [me] });
      if (url.endsWith("/api/v1/panel/backup")) {
        return json({
          last_success_at: "2026-10-10T03:01:00Z",
          file: "setu-20261010T030100Z.dump",
          size_bytes: 2097152,
          last_attempt_at: "2026-10-10T03:01:00Z",
          last_error: null,
          dir: "/var/lib/setu/backups/setu",
        });
      }
      return notFound();
    });
    renderWithClient(<SettingsView />);
    expect(await screen.findByText("Control-plane backup")).toBeInTheDocument();
    expect(await screen.findByText("setu-20261010T030100Z.dump")).toBeInTheDocument();
    expect(screen.getByText(/2\.0 MiB/)).toBeInTheDocument();
    expect(screen.getByText("/var/lib/setu/backups/setu")).toBeInTheDocument();
  });
});
