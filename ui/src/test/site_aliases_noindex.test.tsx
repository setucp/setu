import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { Toaster } from "sonner";
import { describe, expect, it, vi } from "vitest";
import { aliasError, MAX_ALIASES, SiteAliases } from "@/components/SiteAliases";
import { SiteNoindex } from "@/components/SiteNoindex";
import type { Alias, SiteDetail } from "@/lib/client";
import { json, notFound, renderWithClient, siteFixture, stubFetch } from "./helpers";

const ALIASES = "/api/v1/sites/s1/aliases";
const NOINDEX = "/api/v1/sites/s1/noindex";
const me = (role: string) => ({
  id: "op1",
  email: "op@example.com",
  role,
  created_at: "2026-10-01T00:00:00Z",
});
const site = (partial: Record<string, unknown> = {}) =>
  siteFixture(partial) as unknown as SiteDetail;
const job = (id: string) => ({ id, kind: "route_update", status: "queued" });
const domainOk = {
  domain: "old.example.com",
  server_ip: "203.0.113.7",
  ok: true,
  findings: [{ id: "points_at_server", level: "ok", message: "points at the server (203.0.113.7)" }],
};

// The DNS pre-check waits for typing to stop (600 ms), as in new_site.test.tsx.
describe("site aliases", { timeout: 15_000 }, () => {
  it("checks a typed host like the server: shape, own domain, duplicates", () => {
    const rows: Alias[] = [{ host: "old.example.com", mode: "redirect" }];
    expect(aliasError("", rows, "blog.example.com")).toBeNull();
    expect(aliasError("  new.example.com. ", rows, "blog.example.com")).toBeNull();
    expect(aliasError("https://x.example.com", rows, "blog.example.com")).toMatch(/bare hostname/);
    expect(aliasError("localhost", rows, "blog.example.com")).toMatch(/bare hostname/);
    expect(aliasError("Blog.Example.com", rows, "blog.example.com")).toMatch(/own domain/);
    expect(aliasError("OLD.example.com.", rows, "blog.example.com")).toMatch(/Already listed/);
    // The server takes IDN names (stored as punycode) and refuses IP addresses.
    expect(aliasError("bücher.example", rows, "blog.example.com")).toBeNull();
    expect(aliasError("xn--bcher-kva.example", [{ host: "xn--bcher-kva.example", mode: "redirect" }], "x.example")).toMatch(/Already listed/);
    expect(aliasError("bücher.example", [{ host: "xn--bcher-kva.example", mode: "redirect" }], "x.example")).toMatch(/Already listed/);
    expect(aliasError("1.2.3.4", rows, "blog.example.com")).toMatch(/bare hostname/);
    expect(aliasError("*.example.com", rows, "blog.example.com")).toMatch(/bare hostname/);
    // Short and hex IP forms the URL parser would turn into a dotted quad, and escapes.
    for (const ip of ["1.2.3", "127.1", "0x7f.1", "10.0x1", "ex%41mple.com", "１２７.１", "127。1", "127.１"]) {
      expect(aliasError(ip, rows, "blog.example.com"), ip).toMatch(/bare hostname/);
    }
  });

  it("names the automatic www redirect only where the agent makes one", async () => {
    stubFetch((url) => {
      if (url.endsWith("/api/v1/auth/me")) return json(me("viewer"));
      if (url.endsWith(ALIASES)) return json({ aliases: [{ host: "www.example.com", mode: "serve" }] });
      return notFound();
    });
    const { unmount } = renderWithClient(
      <SiteAliases site={site({ domain: "example.com", www_redirect: true })} />,
    );
    expect(await screen.findByText(/replaces the automatic www redirect/i)).toBeInTheDocument();
    unmount();
    renderWithClient(<SiteAliases site={site({ domain: "example.com", www_redirect: false })} />);
    await screen.findByText("www.example.com");
    expect(screen.queryByText(/replaces the automatic www redirect/i)).not.toBeInTheDocument();
  });

  it("edits a draft (add, change mode, remove) and saves the whole list once", async () => {
    let saved: Alias[] = [
      { host: "old.example.com", mode: "redirect" },
      { host: "www.blog.example.com", mode: "redirect" },
    ];
    const calls = stubFetch((url, method, body) => {
      if (url.endsWith("/api/v1/auth/me")) return json(me("operator"));
      if (url.includes("/api/v1/domain-check")) return json({ ...domainOk, domain: "new.example.com" });
      if (url.endsWith(ALIASES) && method === "PUT") {
        saved = (body as { aliases: Alias[] }).aliases;
        return json({ aliases: saved, job: job("job-al") }, 202);
      }
      if (url.endsWith(ALIASES)) return json({ aliases: saved });
      return notFound();
    });
    const user = userEvent.setup();
    const onSubmitted = vi.fn();
    renderWithClient(<SiteAliases site={site()} onSubmitted={onSubmitted} />);

    const list = await screen.findByRole("list", { name: "Hostnames" });
    expect(within(list).getByText("old.example.com")).toBeInTheDocument();
    // blog.example.com is a subdomain: the agent makes no automatic www redirect for it.
    expect(within(list).queryByText(/replaces the automatic www redirect/i)).not.toBeInTheDocument();
    const save = await screen.findByRole("button", { name: /save hostnames/i });
    expect(save).toBeDisabled();

    // A typo is refused before anything is sent; a valid name gets the DNS check.
    const input = screen.getByLabelText("Hostname");
    await user.type(input, "blog.example.com");
    expect(screen.getByText("That is the site's own domain.")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /^add$/i })).toBeDisabled();
    await user.clear(input);
    await user.click(input);
    await user.paste(" New.Example.com. ");
    await user.selectOptions(screen.getByLabelText("Answers with"), "serve");
    await waitFor(
      () =>
        expect(calls.some((c) => c.url.includes("domain=new.example.com&server_id=srv1"))).toBe(
          true,
        ),
      { timeout: 3000 },
    );
    await user.click(screen.getByRole("button", { name: /^add$/i }));
    expect(input).toHaveValue("");
    expect(within(list).getByText("new.example.com")).toBeInTheDocument();
    expect(screen.getByText("Not saved yet.")).toBeInTheDocument();

    await user.selectOptions(screen.getByLabelText("How old.example.com answers"), "serve");
    await user.click(screen.getByRole("button", { name: "Remove www.blog.example.com" }));
    expect(calls.some((c) => c.method === "PUT")).toBe(false);

    await user.click(save);
    await waitFor(() => expect(onSubmitted).toHaveBeenCalledWith("job-al"));
    expect(calls.filter((c) => c.method === "PUT")).toHaveLength(1);
    expect(calls.find((c) => c.method === "PUT")?.body).toEqual({
      aliases: [
        { host: "old.example.com", mode: "serve" },
        { host: "new.example.com", mode: "serve" },
      ],
    });
    expect(await screen.findByText(/the app does not restart/i)).toBeInTheDocument();
    expect(save).toBeDisabled();
  });

  it("discards a draft back to the saved list", async () => {
    stubFetch((url) => {
      if (url.endsWith("/api/v1/auth/me")) return json(me("operator"));
      if (url.endsWith(ALIASES)) return json({ aliases: [{ host: "old.example.com", mode: "redirect" }] });
      return notFound();
    });
    const user = userEvent.setup();
    renderWithClient(<SiteAliases site={site()} />);
    await user.click(await screen.findByRole("button", { name: "Remove old.example.com" }));
    expect(screen.getByText(/no extra hostnames/i)).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: /discard changes/i }));
    expect(screen.getByText("old.example.com")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /save hostnames/i })).toBeDisabled();
  });

  it("keeps the draft and shows the server's refusal (another site's name)", async () => {
    stubFetch((url, method) => {
      if (url.endsWith("/api/v1/auth/me")) return json(me("operator"));
      if (url.includes("/api/v1/domain-check")) return json(domainOk);
      if (url.endsWith(ALIASES) && method === "PUT") {
        return json({ error: { code: "conflict", message: "old.example.com is already used by site shop" } }, 409);
      }
      if (url.endsWith(ALIASES)) return json({ aliases: [] });
      return notFound();
    });
    const user = userEvent.setup();
    const onSubmitted = vi.fn();
    renderWithClient(
      <>
        <Toaster />
        <SiteAliases site={site()} onSubmitted={onSubmitted} />
      </>,
    );
    await user.type(await screen.findByLabelText("Hostname"), "old.example.com");
    await user.click(screen.getByRole("button", { name: /^add$/i }));
    await user.click(screen.getByRole("button", { name: /save hostnames/i }));
    expect(await screen.findByText(/already used by site shop/)).toBeInTheDocument();
    expect(onSubmitted).not.toHaveBeenCalled();
    expect(screen.getByText("old.example.com")).toBeInTheDocument();
    expect(screen.getByText("Not saved yet.")).toBeInTheDocument();
  });

  it("stops adding at the server's limit", async () => {
    const many = Array.from({ length: MAX_ALIASES }, (_, i) => ({
      host: `a${i}.example.com`,
      mode: "redirect",
    }));
    stubFetch((url) => {
      if (url.endsWith("/api/v1/auth/me")) return json(me("operator"));
      if (url.endsWith(ALIASES)) return json({ aliases: many });
      return notFound();
    });
    renderWithClient(<SiteAliases site={site()} />);
    expect(await screen.findByLabelText("Hostname")).toBeDisabled();
    expect(screen.getByText(`The site has the maximum of ${MAX_ALIASES}.`)).toBeInTheDocument();
  });

  it("waits for a settled site before saving", async () => {
    stubFetch((url) => {
      if (url.endsWith("/api/v1/auth/me")) return json(me("operator"));
      if (url.endsWith(ALIASES)) return json({ aliases: [{ host: "old.example.com", mode: "redirect" }] });
      return notFound();
    });
    const user = userEvent.setup();
    renderWithClient(<SiteAliases site={site({ status: "updating" })} />);
    await user.click(await screen.findByRole("button", { name: "Remove old.example.com" }));
    expect(screen.getByText(/the site is updating/i)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /save hostnames/i })).toBeDisabled();
  });

  it("shows viewers the list without controls", async () => {
    stubFetch((url) => {
      if (url.endsWith("/api/v1/auth/me")) return json(me("viewer"));
      if (url.endsWith(ALIASES)) return json({ aliases: [{ host: "old.example.com", mode: "serve" }] });
      return notFound();
    });
    renderWithClient(<SiteAliases site={site()} />);
    expect(await screen.findByText("serves the site")).toBeInTheDocument();
    expect(screen.queryByLabelText("Hostname")).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /save hostnames/i })).not.toBeInTheDocument();
  });
});

describe("site noindex", () => {
  it("asks before hiding a site, then turns it back on without asking", async () => {
    let enabled = false;
    const calls = stubFetch((url, method, body) => {
      if (url.endsWith("/api/v1/auth/me")) return json(me("operator"));
      if (url.endsWith(NOINDEX) && method === "PUT") {
        enabled = (body as { enabled: boolean }).enabled;
        return json({ enabled, job: job(enabled ? "job-on" : "job-off") }, 202);
      }
      if (url.endsWith(NOINDEX)) return json({ enabled });
      return notFound();
    });
    const user = userEvent.setup();
    const onSubmitted = vi.fn();
    renderWithClient(<SiteNoindex site={site()} onSubmitted={onSubmitted} />);

    expect(await screen.findByText("allowed")).toBeInTheDocument();
    await user.click(await screen.findByRole("button", { name: /hide from search engines/i }));
    const dialog = await screen.findByRole("dialog");
    expect(dialog).toHaveTextContent(/hide blog from search engines/i);
    expect(calls.some((c) => c.method === "PUT")).toBe(false);
    await user.click(within(dialog).getByRole("button", { name: /hide from search engines/i }));

    await waitFor(() => expect(onSubmitted).toHaveBeenCalledWith("job-on"));
    expect(calls.find((c) => c.method === "PUT")?.body).toEqual({ enabled: true });
    expect(await screen.findByText("hidden")).toBeInTheDocument();
    expect(screen.getByText(/X-Robots-Tag: noindex, nofollow/)).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: /allow search engines/i }));
    await waitFor(() => expect(onSubmitted).toHaveBeenCalledWith("job-off"));
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    expect(calls.filter((c) => c.method === "PUT").map((c) => c.body)).toEqual([
      { enabled: true },
      { enabled: false },
    ]);
  });

  it("cancelling the confirm leaves the site indexed and sends nothing", async () => {
    const calls = stubFetch((url) => {
      if (url.endsWith("/api/v1/auth/me")) return json(me("operator"));
      if (url.endsWith(NOINDEX)) return json({ enabled: false });
      return notFound();
    });
    const user = userEvent.setup();
    renderWithClient(<SiteNoindex site={site()} />);
    await user.click(await screen.findByRole("button", { name: /hide from search engines/i }));
    const dialog = await screen.findByRole("dialog");
    await user.click(within(dialog).getByRole("button", { name: /cancel/i }));
    await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
    expect(screen.getByText("allowed")).toBeInTheDocument();
    expect(calls.some((c) => c.method === "PUT")).toBe(false);
  });

  it("shows viewers the setting without the button", async () => {
    stubFetch((url) => {
      if (url.endsWith("/api/v1/auth/me")) return json(me("viewer"));
      if (url.endsWith(NOINDEX)) return json({ enabled: true });
      return notFound();
    });
    renderWithClient(<SiteNoindex site={site()} />);
    expect(await screen.findByText("hidden")).toBeInTheDocument();
    expect(screen.queryByRole("button")).not.toBeInTheDocument();
  });
});
