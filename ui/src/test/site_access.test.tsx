import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { Toaster } from "sonner";
import { describe, expect, it, vi } from "vitest";
import { passwordError, SiteAccess, usernameError } from "@/components/SiteAccess";
import type { SiteDetail } from "@/lib/client";
import { json, notFound, renderWithClient, siteFixture, stubFetch } from "./helpers";

const URL = "/api/v1/sites/s1/basic-auth";
const me = (role: string) => ({
  id: "op1",
  email: "op@example.com",
  role,
  created_at: "2026-10-01T00:00:00Z",
});
const site = (partial: Record<string, unknown> = {}) =>
  siteFixture(partial) as unknown as SiteDetail;
const job = (id: string) => ({ id, kind: "route_update", status: "queued" });

describe("site access (basic auth)", () => {
  it("validates like the server: user name shape, password bytes, control characters", () => {
    expect(usernameError("")).toBeNull();
    expect(usernameError("ada.l_1-x")).toBeNull();
    expect(usernameError("a b")).not.toBeNull();
    expect(usernameError("x".repeat(65))).not.toBeNull();
    expect(passwordError("")).toBeNull();
    expect(passwordError("1234567")).toMatch(/is 7/);
    expect(passwordError("12345678")).toBeNull();
    // Bytes, not characters: 36 two-byte letters are 72 bytes, 37 are 74.
    expect(passwordError("é".repeat(36))).toBeNull();
    expect(passwordError("é".repeat(37))).toMatch(/is 74/);
    expect(passwordError("tab\there!")).toMatch(/control/);
    expect(passwordError("c1\u0085control")).toMatch(/control/);
  });

  it("protects an open site, then replaces and removes the credentials", async () => {
    let current: unknown = null;
    const calls = stubFetch((url, method, body) => {
      if (url.endsWith("/api/v1/auth/me")) return json(me("operator"));
      if (url.endsWith(URL) && method === "PUT") {
        const b = body as { username: string };
        current = { username: b.username, updated_at: "2026-11-21T10:00:00Z" };
        return json({ basic_auth: current, job: job("job-put") }, 202);
      }
      if (url.endsWith(URL) && method === "DELETE") {
        current = null;
        return json({ basic_auth: null, job: job("job-del") }, 202);
      }
      if (url.endsWith(URL)) return json({ basic_auth: current });
      return notFound();
    });
    const user = userEvent.setup();
    const onSubmitted = vi.fn();
    renderWithClient(<SiteAccess site={site()} onSubmitted={onSubmitted} />);

    expect(await screen.findByText("Open to everyone")).toBeInTheDocument();
    const protect = await screen.findByRole("button", { name: /protect site/i });
    expect(protect).toBeDisabled();

    await user.type(screen.getByLabelText("User name"), "ada");
    await user.type(screen.getByLabelText("Password"), "short");
    expect(screen.getByText(/8 to 72 bytes \(this one is 5\)/)).toBeInTheDocument();
    await user.clear(screen.getByLabelText("Password"));
    await user.type(screen.getByLabelText("Password"), "correct horse");
    await user.type(screen.getByLabelText("Password again"), "correct hors");
    await user.tab();
    expect(screen.getByText("The two passwords differ.")).toBeInTheDocument();
    expect(protect).toBeDisabled();
    await user.type(screen.getByLabelText("Password again"), "e");
    await user.click(protect);

    await waitFor(() => expect(onSubmitted).toHaveBeenCalledWith("job-put"));
    expect(calls.find((c) => c.method === "PUT")?.body).toEqual({
      username: "ada",
      password: "correct horse",
    });
    expect(await screen.findByText("Password protected")).toBeInTheDocument();
    expect(screen.getByText("ada")).toBeInTheDocument();
    // The password fields are cleared; the user name stays for a replace.
    expect(screen.getByLabelText("Password")).toHaveValue("");
    expect(screen.getByLabelText("User name")).toHaveValue("ada");

    // Remove: confirmed by name before anything is sent.
    await user.click(screen.getByRole("button", { name: /remove password/i }));
    const dialog = await screen.findByRole("dialog");
    expect(dialog).toHaveTextContent(/open blog to everyone/i);
    expect(calls.some((c) => c.method === "DELETE")).toBe(false);
    await user.click(within(dialog).getByTestId("confirm-accept"));
    await waitFor(() => expect(onSubmitted).toHaveBeenCalledWith("job-del"));
    expect(await screen.findByText("Open to everyone")).toBeInTheDocument();
  }, 20000);

  it("shows the user name of a protected site; a viewer gets no form", async () => {
    const calls = stubFetch((url) => {
      if (url.endsWith("/api/v1/auth/me")) return json(me("viewer"));
      if (url.endsWith(URL)) {
        return json({ basic_auth: { username: "team", updated_at: "2026-11-20T10:00:00Z" } });
      }
      return notFound();
    });
    renderWithClient(<SiteAccess site={site()} />);
    expect(await screen.findByText("Password protected")).toBeInTheDocument();
    expect(screen.getByText("team")).toBeInTheDocument();
    // Wait until the role is known, then for the re-render it causes.
    await waitFor(() => expect(calls.some((c) => c.url.endsWith("/api/v1/auth/me"))).toBe(true));
    await new Promise((r) => setTimeout(r, 50));
    expect(screen.queryByRole("form", { name: "Basic auth" })).toBeNull();
    expect(screen.queryByRole("button", { name: /replace credentials/i })).toBeNull();
  });

  it("disables changes while the site is busy and shows the server's refusal", async () => {
    stubFetch((url, method) => {
      if (url.endsWith("/api/v1/auth/me")) return json(me("owner"));
      if (url.endsWith(URL) && method === "PUT") {
        return json(
          { error: { code: "conflict", message: "a job is already running for this site" } },
          409,
        );
      }
      if (url.endsWith(URL)) return json({ basic_auth: null });
      return notFound();
    });
    const user = userEvent.setup();
    const view = renderWithClient(<SiteAccess site={site({ status: "provisioning" })} />);
    expect(await screen.findByText(/the site is provisioning/i)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /protect site/i })).toBeDisabled();

    view.unmount();
    const onSubmitted = vi.fn();
    renderWithClient(
      <>
        <SiteAccess site={site()} onSubmitted={onSubmitted} />
        <Toaster />
      </>,
    );
    await user.type(await screen.findByLabelText("User name"), "ada");
    await user.type(screen.getByLabelText("Password"), "12345678");
    await user.type(screen.getByLabelText("Password again"), "12345678");
    await user.click(screen.getByRole("button", { name: /protect site/i }));
    await waitFor(() => expect(screen.getByLabelText("Password")).toHaveValue("12345678"));
    expect(onSubmitted).not.toHaveBeenCalled();
    // The server's reason reaches the operator.
    expect(await screen.findByText("a job is already running for this site")).toBeInTheDocument();
  });

  it('a failed load is an error, never "open to everyone"', async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async (url: unknown) => {
        if (String(url).endsWith("/api/v1/auth/me")) return json(me("owner"));
        throw new TypeError("Failed to fetch");
      }),
    );
    renderWithClient(<SiteAccess site={site()} />);
    expect(await screen.findByText("Failed to fetch")).toBeInTheDocument();
    expect(screen.queryByText("Open to everyone")).toBeNull();
    expect(screen.queryByText("Password protected")).toBeNull();
    expect(screen.queryByRole("button", { name: /protect site/i })).toBeNull();
  });
});
