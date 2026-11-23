import { act, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { daysLeftText, SiteCertificate } from "@/components/SiteCertificate";
import { ServerLogs, SERVER_LOG_POLL_MS } from "@/components/ServerLogs";
import type { SiteDetail } from "@/lib/client";
import { json, notFound, renderWithClient, siteFixture, stubFetch } from "./helpers";

const CERT = "/api/v1/sites/s1/certificate";
const me = (role: string) => ({ id: "op1", email: "op@example.com", role, created_at: "2026-10-01T00:00:00Z" });
const site = (partial: Record<string, unknown> = {}) => siteFixture(partial) as unknown as SiteDetail;
const valid = {
  domain: "blog.example.com",
  state: "valid",
  issuer: "Let's Encrypt R11",
  not_before: "2026-10-01T00:00:00Z",
  not_after: "2026-12-30T00:00:00Z",
  days_left: 63,
  checked_at: "2026-10-28T00:00:00Z",
  error: null,
};
const pending = {
  domain: "blog.example.com",
  state: "pending",
  checked_at: "2026-10-28T00:00:00Z",
  error: "no certificate for blog.example.com yet",
};

afterEach(() => {
  vi.useRealTimers();
});

describe("site certificate", () => {
  it("words the days left", () => {
    expect(daysLeftText(63)).toBe("in 63 days");
    expect(daysLeftText(1)).toBe("in 1 day");
    expect(daysLeftText(0)).toBe("today");
    expect(daysLeftText(-1)).toBe("1 day ago");
    expect(daysLeftText(-4)).toBe("4 days ago");
  });

  it("shows a valid certificate's issuer and expiry; a viewer gets no retry", async () => {
    stubFetch((url) => {
      if (url.endsWith("/api/v1/auth/me")) return json(me("viewer"));
      if (url.endsWith(CERT)) return json(valid);
      return notFound();
    });
    renderWithClient(<SiteCertificate site={site()} />);
    expect(await screen.findByText("valid")).toBeInTheDocument();
    expect(screen.getByText("Let's Encrypt R11")).toBeInTheDocument();
    expect(screen.getByText(/in 63 days/)).toBeInTheDocument();
    await waitFor(() => expect(screen.queryByText(/reading the certificate/i)).not.toBeInTheDocument());
    expect(screen.queryByRole("button", { name: /retry now/i })).not.toBeInTheDocument();
  });

  it("an operator retries a pending certificate, follows the job, and reads it again when the job ends", async () => {
    let cert: unknown = pending;
    let jobStatus = "running";
    const calls = stubFetch((url, method) => {
      if (url.endsWith("/api/v1/auth/me")) return json(me("operator"));
      if (url.endsWith(`${CERT}/retry`) && method === "POST") {
        return json({ id: "job-r", kind: "route_update", status: "queued" }, 202);
      }
      if (url.endsWith(CERT)) return json(cert);
      if (url.endsWith("/api/v1/jobs/job-r")) return json({ id: "job-r", kind: "route_update", status: jobStatus });
      return notFound();
    });
    const user = userEvent.setup();
    renderWithClient(<SiteCertificate site={site()} />);
    expect(await screen.findByText("pending")).toBeInTheDocument();
    expect(screen.getByText(/no certificate for blog\.example\.com yet/)).toBeInTheDocument();

    await user.click(await screen.findByRole("button", { name: /retry now/i }));
    expect(calls.filter((c) => c.method === "POST").map((c) => c.url)).toEqual([`${CERT}/retry`]);
    const link = await screen.findByRole("link", { name: /retry job/i });
    expect(link).toHaveAttribute("href", "/jobs/job-r");
    expect(await screen.findByText("running")).toBeInTheDocument();

    // The job ends: the card reads the certificate again.
    cert = valid;
    jobStatus = "done";
    expect(await screen.findByText("valid", {}, { timeout: 3000 })).toBeInTheDocument();
    expect(calls.filter((c) => c.url === CERT).length).toBeGreaterThanOrEqual(2);
  });

  it("the retry is off while the site is neither running nor stopped", async () => {
    stubFetch((url) => {
      if (url.endsWith("/api/v1/auth/me")) return json(me("owner"));
      if (url.endsWith(CERT)) return json(pending);
      return notFound();
    });
    renderWithClient(<SiteCertificate site={site({ status: "updating" })} />);
    expect(await screen.findByRole("button", { name: /retry now/i })).toBeDisabled();
    expect(screen.getByText(/once the site is running or stopped \(it is updating\)/i)).toBeInTheDocument();
  });

  it("a failed read shows the error, not a state", async () => {
    stubFetch((url) => {
      if (url.endsWith("/api/v1/auth/me")) return json(me("operator"));
      if (url.endsWith(CERT)) return json({ error: { code: "not_found", message: "site not found" } }, 404);
      return notFound();
    });
    renderWithClient(<SiteCertificate site={site()} />);
    expect(await screen.findByText("site not found")).toBeInTheDocument();
    expect(screen.queryByText("valid")).not.toBeInTheDocument();
  });
});

describe("server logs", () => {
  it("reads the tail, then only what follows the cursor; switching unit starts over", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    let page = 0;
    const calls = stubFetch((url) => {
      const u = new URL(url, "http://x");
      if (u.pathname !== "/api/v1/servers/srv1/logs") return notFound();
      const unit = u.searchParams.get("unit");
      const cursor = u.searchParams.get("cursor");
      if (unit === "agent") return json({ lines: ["agent up"], cursor: "a1" });
      if (cursor === null) return json({ lines: ["caddy 1", "caddy 2"], cursor: "c2" });
      page += 1;
      return json({ lines: page === 1 ? ["caddy 3"] : [], cursor: `c${2 + page}` });
    });
    const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime.bind(vi) });
    renderWithClient(<ServerLogs serverId="srv1" serverName="web1" />);
    const pre = await screen.findByTestId("server-logs");
    await waitFor(() => expect(pre.textContent).toBe("caddy 1\ncaddy 2"));

    await act(async () => {
      await vi.advanceTimersByTimeAsync(SERVER_LOG_POLL_MS);
    });
    await waitFor(() => expect(pre.textContent).toBe("caddy 1\ncaddy 2\ncaddy 3"));
    expect(calls[1]?.url).toBe("/api/v1/servers/srv1/logs?unit=caddy&cursor=c2");

    await user.click(screen.getByRole("tab", { name: "Agent" }));
    await waitFor(() => expect(screen.getByTestId("server-logs").textContent).toBe("agent up"));
    expect(calls.at(-1)?.url).toBe("/api/v1/servers/srv1/logs?unit=agent");
  });

  it("pausing stops the reads; resuming goes on from the same cursor", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    const calls = stubFetch((url) => {
      const cursor = new URL(url, "http://x").searchParams.get("cursor");
      return json({ lines: cursor === null ? ["first"] : [`after ${cursor}`], cursor: "k1" });
    });
    const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime.bind(vi) });
    renderWithClient(<ServerLogs serverId="srv1" serverName="web1" />);
    const pre = await screen.findByTestId("server-logs");
    await waitFor(() => expect(pre.textContent).toBe("first"));

    await user.click(screen.getByRole("button", { name: /pause live tail/i }));
    const before = calls.length;
    await act(async () => {
      await vi.advanceTimersByTimeAsync(SERVER_LOG_POLL_MS * 3);
    });
    expect(calls.length).toBe(before);

    await user.click(screen.getByRole("button", { name: /resume live tail/i }));
    await waitFor(() => expect(pre.textContent).toBe("first\nafter k1"));
    expect(calls.at(-1)?.url).toBe("/api/v1/servers/srv1/logs?unit=caddy&cursor=k1");
  });

  it("a read the agent cannot answer shows the error and keeps trying", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    let fail = true;
    stubFetch(() =>
      fail
        ? json({ error: { code: "agent_unavailable", message: "the agent did not answer" } }, 502)
        : json({ lines: ["back"], cursor: "z" }),
    );
    renderWithClient(<ServerLogs serverId="srv1" serverName="web1" />);
    const card = (await screen.findByText("the agent did not answer")).closest("div");
    expect(card).not.toBeNull();
    fail = false;
    await act(async () => {
      await vi.advanceTimersByTimeAsync(SERVER_LOG_POLL_MS);
    });
    await waitFor(() => expect(screen.getByTestId("server-logs").textContent).toBe("back"));
    expect(screen.queryByText("the agent did not answer")).not.toBeInTheDocument();
    expect(within(screen.getByRole("tablist")).getAllByRole("tab")).toHaveLength(2);
  });
});
