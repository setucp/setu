import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { SiteDanger } from "@/components/SiteDanger";
import { SiteLimits } from "@/components/SiteLimits";
import { SiteLogs } from "@/components/SiteLogs";
import { SiteOverview } from "@/components/SiteOverview";
import type { SiteDetail } from "@/lib/client";
import {
  MockEventSource,
  installEventSource,
  json,
  notFound,
  planFixture,
  renderWithClient,
  siteFixture,
  stubFetch,
} from "./helpers";

const detail = {
  ...siteFixture(),
  metrics: {
    at: "2026-10-09T00:00:00Z",
    cpu_seconds: 12.5,
    memory_bytes: 128 * 1024 * 1024,
    disk_bytes: 300 * 1024 * 1024,
  },
} as SiteDetail;

describe("site detail", () => {
  beforeEach(() => {
    installEventSource();
  });

  it("overview shows status, version, links and metrics", () => {
    renderWithClient(<SiteOverview site={detail} />);
    expect(screen.getByText("running")).toBeInTheDocument();
    expect(screen.getByText(/0\.1\.0/)).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "blog.example.com" })).toHaveAttribute(
      "href",
      "https://blog.example.com",
    );
    expect(screen.getByText("128.0 MiB")).toBeInTheDocument();
    expect(screen.getByText("12.5 s")).toBeInTheDocument();
    expect(screen.getByText("300.0 MiB")).toBeInTheDocument();
  });

  it("overview links the app's admin next to the domain", () => {
    renderWithClient(<SiteOverview site={{ ...detail, admin_url: "https://blog.example.com/admin" }} />);
    const admin = screen.getByRole("link", { name: /admin/i });
    expect(admin).toHaveAttribute("href", "https://blog.example.com/admin");
    expect(admin).toHaveAttribute("target", "_blank");
  });

  it("overview shows no admin link when the manifest names no usable path", () => {
    renderWithClient(<SiteOverview site={{ ...detail, admin_url: null }} />);
    expect(screen.queryByRole("link", { name: /admin/i })).not.toBeInTheDocument();
  });

  it("overview says when no metrics sample exists yet", () => {
    renderWithClient(<SiteOverview site={{ ...detail, metrics: null }} />);
    expect(screen.getByText(/no sample yet/i)).toBeInTheDocument();
  });

  it("logs: SSE append without duplicates", async () => {
    const user = userEvent.setup();
    renderWithClient(<SiteLogs siteId="s1" />);
    await waitFor(() => expect(MockEventSource.instances).toHaveLength(1));
    const first = MockEventSource.latest();
    expect(first.url).toBe("/api/v1/sites/s1/logs");
    first.emit("GET / 200");
    first.emit("GET /a 200");
    first.emit("GET / 200");
    const pre = await screen.findByTestId("site-logs");
    await waitFor(() => expect(pre.textContent).toBe("GET / 200\nGET /a 200\nGET / 200"));

    // Pause and resume: a new connection replays the journal tail,
    // which must not render twice; a genuinely new line appends.
    await user.click(screen.getByRole("button", { name: /pause/i }));
    expect(first.closed).toBe(true);
    await user.click(screen.getByRole("button", { name: /resume/i }));
    await waitFor(() => expect(MockEventSource.instances).toHaveLength(2));
    const second = MockEventSource.latest();
    second.emit("GET /a 200");
    second.emit("GET / 200");
    second.emit("GET /b 200");
    await waitFor(() =>
      expect(pre.textContent).toBe("GET / 200\nGET /a 200\nGET / 200\nGET /b 200"),
    );

    // A browser auto-reconnect on the same instance replays too.
    second.reopen();
    second.emit("GET /b 200");
    second.emit("GET /c 200");
    await waitFor(() =>
      expect(pre.textContent).toBe("GET / 200\nGET /a 200\nGET / 200\nGET /b 200\nGET /c 200"),
    );
  }, 20000);

  it("limits: shows plan and current numbers", async () => {
    stubFetch((url) => (url.endsWith("/api/v1/plans") ? json({ plans: [planFixture] }) : notFound()));
    renderWithClient(<SiteLimits site={detail} />);
    expect(await screen.findByText("standard")).toBeInTheDocument();
    const memory = screen.getByRole("row", { name: /memory/i });
    expect(within(memory).getByText("512 MB")).toBeInTheDocument();
    expect(within(memory).getByText("128.0 MiB")).toBeInTheDocument();
    const disk = screen.getByRole("row", { name: /disk/i });
    expect(within(disk).getByText("5120 MB")).toBeInTheDocument();
    expect(within(disk).getByText("300.0 MiB")).toBeInTheDocument();
    const cpu = screen.getByRole("row", { name: /cpu/i });
    expect(within(cpu).getByText(/100% of one core/)).toBeInTheDocument();
    expect(within(cpu).getByText("12.5 s")).toBeInTheDocument();
  });

  it("danger: stop/delete confirmations", async () => {
    const calls = stubFetch((url, method) => {
      if (url.endsWith("/api/v1/sites/s1/stop") && method === "POST") {
        return json({ site: detail, job: { id: "job-stop", status: "queued" } }, 202);
      }
      if (url.endsWith("/api/v1/sites/s1") && method === "DELETE") {
        return json({ job: { id: "job-del", status: "queued" } }, 202);
      }
      return notFound();
    });
    const user = userEvent.setup();
    const onJob = vi.fn();
    const onDeleted = vi.fn();
    renderWithClient(<SiteDanger site={detail} onJob={onJob} onDeleted={onDeleted} />);

    // Stop: a confirmation naming the site; cancel sends nothing.
    await user.click(screen.getByRole("button", { name: /^stop$/i }));
    let dialog = await screen.findByRole("dialog");
    expect(dialog).toHaveTextContent(/stop blog\?/i);
    await user.click(within(dialog).getByRole("button", { name: /cancel/i }));
    expect(calls.some((c) => c.method === "POST")).toBe(false);
    await user.click(screen.getByRole("button", { name: /^stop$/i }));
    await user.click(await screen.findByTestId("confirm-accept"));
    await waitFor(() => expect(onJob).toHaveBeenCalledWith("job-stop"));

    // Delete: only once the site's name is typed.
    await user.click(screen.getByRole("button", { name: /delete blog/i }));
    dialog = await screen.findByRole("dialog");
    expect(dialog).toHaveTextContent(/delete blog\?/i);
    const accept = within(dialog).getByTestId("confirm-accept");
    expect(accept).toBeDisabled();
    await user.type(within(dialog).getByLabelText(/type blog to confirm/i), "blog");
    expect(accept).toBeEnabled();
    await user.click(accept);
    await waitFor(() => expect(onDeleted).toHaveBeenCalledWith("job-del"));
    expect(calls.filter((c) => c.method === "DELETE")).toHaveLength(1);
  }, 20000);
});

describe("site logs", () => {
  beforeEach(() => {
    installEventSource();
  });

  it("another site starts a fresh buffer", async () => {
    // Plain render (SiteLogs needs no providers) so rerender reuses the
    // same component instance, as a route param change does.
    const { rerender } = render(<SiteLogs siteId="s1" />);
    await waitFor(() => expect(MockEventSource.instances).toHaveLength(1));
    MockEventSource.latest().emit("from s1");
    const pre = await screen.findByTestId("site-logs");
    await waitFor(() => expect(pre.textContent).toBe("from s1"));
    rerender(<SiteLogs siteId="s2" />);
    await waitFor(() => expect(MockEventSource.instances).toHaveLength(2));
    expect(MockEventSource.instances[0]?.closed).toBe(true);
    MockEventSource.latest().emit("from s2");
    await waitFor(() => expect(pre.textContent).toBe("from s2"));
  });
});
