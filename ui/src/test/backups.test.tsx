import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { SiteBackups } from "@/components/SiteBackups";
import { json, notFound, renderWithClient, stubFetch } from "./helpers";

const backupsBody = {
  backups: [
    {
      id: "b1",
      site_id: "s1",
      site_name: "blog",
      domain: "blog.example.com",
      taken_at: "2026-10-05T00:00:00Z",
      kind: "manual",
      status: "completed",
      size_bytes: 2 * 1024 * 1024,
    },
    {
      id: "b2",
      site_id: "s1",
      site_name: "blog",
      domain: "blog.example.com",
      taken_at: "2026-10-06T00:00:00Z",
      kind: "pre_update",
      status: "failed",
      size_bytes: null,
    },
  ],
};

describe("backups", () => {
  it("backups: take, restore with confirmation naming the site, delete", async () => {
    const calls = stubFetch((url, method) => {
      if (url.endsWith("/api/v1/sites/s1/backups") && method === "POST") {
        return json({ job: { id: "job-take", status: "queued" } }, 202);
      }
      if (url.endsWith("/api/v1/sites/s1/backups")) return json(backupsBody);
      if (url.endsWith("/api/v1/sites/s1/backups/b1/restore") && method === "POST") {
        return json({ job: { id: "job-restore", status: "queued" } }, 202);
      }
      if (url.endsWith("/api/v1/sites/s1/backups/b1") && method === "DELETE") {
        return json({ job: { id: "job-delete", status: "queued" } }, 202);
      }
      return notFound();
    });
    const user = userEvent.setup();
    const onJob = vi.fn();
    renderWithClient(<SiteBackups siteId="s1" siteName="blog" onJob={onJob} />);

    // List: kind, taken at, size, status.
    const row = await screen.findByRole("row", { name: /manual/ });
    expect(within(row).getByText("2.0 MiB")).toBeInTheDocument();
    expect(within(row).getByText("completed")).toBeInTheDocument();
    // Relative time on screen, the exact time on hover.
    expect(within(row).getByTitle(new Date("2026-10-05T00:00:00Z").toLocaleString())).toBeInTheDocument();
    const failed = screen.getByRole("row", { name: /pre_update/ });
    expect(within(failed).getByText("failed")).toBeInTheDocument();
    // Only a completed backup can be restored.
    expect(within(failed).queryByRole("button", { name: /restore/i })).not.toBeInTheDocument();

    // Take.
    await user.click(screen.getByRole("button", { name: /take backup/i }));
    await waitFor(() => expect(onJob).toHaveBeenCalledWith("job-take"));

    // Restore: the confirmation names the site; nothing posts before accept.
    await user.click(within(row).getByRole("button", { name: /restore/i }));
    let dialog = await screen.findByRole("dialog");
    expect(dialog).toHaveTextContent(/restore blog\b/i);
    expect(calls.some((c) => c.url.endsWith("/restore"))).toBe(false);
    await user.click(within(dialog).getByTestId("confirm-accept"));
    await waitFor(() => expect(onJob).toHaveBeenCalledWith("job-restore"));

    // Delete: confirmed, then DELETE /sites/s1/backups/b1.
    await user.click(within(row).getByRole("button", { name: /delete/i }));
    dialog = await screen.findByRole("dialog");
    expect(dialog).toHaveTextContent(/delete backup of blog/i);
    expect(calls.some((c) => c.method === "DELETE")).toBe(false);
    await user.click(within(dialog).getByTestId("confirm-accept"));
    await waitFor(() => expect(onJob).toHaveBeenCalledWith("job-delete"));
    expect(calls.filter((c) => c.method === "DELETE").map((c) => c.url)).toEqual([
      "/api/v1/sites/s1/backups/b1",
    ]);
  }, 20000);

  it("sets a reset point with an interval, shows it, and stops it", async () => {
    let reset: unknown = null;
    const calls = stubFetch((url, method, body) => {
      if (url.endsWith("/api/v1/sites/s1/backups")) return json(backupsBody);
      if (url.endsWith("/api/v1/sites/s1/reset") && method === "PUT") {
        const b = body as { backup_id: string; every_minutes: number };
        reset = {
          backup_id: b.backup_id,
          every_minutes: b.every_minutes,
          last_reset_at: "2026-10-20T12:00:00Z",
          next_reset_at: "2026-10-20T13:00:00Z",
        };
        return json({ reset });
      }
      if (url.endsWith("/api/v1/sites/s1/reset") && method === "DELETE") {
        reset = null;
        return new Response(null, { status: 204 });
      }
      if (url.endsWith("/api/v1/sites/s1/reset")) return json({ reset });
      return notFound();
    });
    const user = userEvent.setup();
    renderWithClient(<SiteBackups siteId="s1" siteName="blog" />);
    await screen.findAllByText("manual");
    // Only the completed backup can become the reset point.
    const buttons = screen.getAllByRole("button", { name: /use as reset point/i });
    expect(buttons).toHaveLength(1);
    await user.click(buttons[0]!);
    const dialog = await screen.findByRole("dialog");
    await user.selectOptions(within(dialog).getByRole("combobox"), "hourly");
    await user.click(within(dialog).getByRole("button", { name: /start resets/i }));
    await waitFor(() =>
      expect(calls.find((c) => c.method === "PUT")?.body).toEqual({
        backup_id: "b1",
        every_minutes: 60,
      }),
    );
    expect(await screen.findByText(/restored from its reset point hourly/i)).toBeInTheDocument();
    expect(screen.getAllByText("reset point").length).toBeGreaterThan(0);

    await user.click(screen.getByRole("button", { name: /stop resets/i }));
    await user.click(await screen.findByTestId("confirm-accept"));
    await waitFor(() => expect(calls.some((c) => c.method === "DELETE")).toBe(true));
    await waitFor(() =>
      expect(screen.queryByText(/restored from its reset point/i)).not.toBeInTheDocument(),
    );
  }, 20000);
});
