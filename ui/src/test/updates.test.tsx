import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { SiteUpdates } from "@/components/SiteUpdates";
import { jobFixture, json, notFound, renderWithClient, stubFetch } from "./helpers";

function version(v: string, day: number) {
  return {
    id: `v-${v}`,
    app_id: "hello",
    version: v,
    channel: "stable",
    published_at: `2026-10-0${day}T00:00:00Z`,
  };
}

const versionsBody = {
  versions: [version("0.10.0", 4), version("0.2.0", 3), version("0.1.0", 2), version("0.0.9", 1)],
};

describe("updates", () => {
  it("updates: newer versions, start update, rollback outcome shown", async () => {
    let jobs = [jobFixture({ id: "j-old", kind: "update_site", status: "done", site_id: "other" })];
    const calls = stubFetch((url, method) => {
      if (url.endsWith("/api/v1/apps/hello/versions")) return json(versionsBody);
      if (url.endsWith("/api/v1/apps/hello/versions/0.2.0/prompts")) {
        return json({ prompts: [{ name: "SMTP_HOST", label: "Mail server", secret: false }] });
      }
      if (url.includes("/api/v1/jobs")) return json({ jobs });
      if (url.endsWith("/api/v1/sites/s1/update") && method === "POST") {
        return json(
          { site: { id: "s1" }, job: jobFixture({ id: "job-update", kind: "update_site" }) },
          202,
        );
      }
      return notFound();
    });
    const user = userEvent.setup();
    const onJob = vi.fn();
    const { unmount } = renderWithClient(
      <SiteUpdates siteId="s1" appId="hello" currentVersion="0.1.0" onJob={onJob} />,
    );

    // Newer versions only, numeric order (0.10.0 > 0.2.0).
    expect(await screen.findByText("0.10.0")).toBeInTheDocument();
    expect(screen.getByText("0.2.0")).toBeInTheDocument();
    expect(screen.queryByText("0.0.9")).not.toBeInTheDocument();
    expect(screen.getByText(/running 0\.1\.0/i)).toBeInTheDocument();
    // Another site's update job is not this site's outcome.
    expect(screen.queryByText(/updated to/i)).not.toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: /update to 0\.2\.0/i }));
    const dialog = await screen.findByRole("dialog");
    expect(dialog).toHaveTextContent(/update blog|update to 0\.2\.0/i);
    // Answers for prompt variables the new version adds.
    await user.type(await within(dialog).findByLabelText("Mail server"), "mail.example.com");
    expect(calls.some((c) => c.method === "POST")).toBe(false);
    await user.click(within(dialog).getByTestId("confirm-accept"));
    await waitFor(() => expect(onJob).toHaveBeenCalledWith("job-update"));
    expect(calls.find((c) => c.method === "POST")?.body).toEqual({
      version: "0.2.0",
      prompts: { SMTP_HOST: "mail.example.com" },
    });
    unmount();

    // The latest update job for this site rolled back: say so plainly.
    jobs = [
      jobFixture({
        id: "job-update",
        kind: "update_site",
        status: "failed",
        error: "rollback: health failed (no 200 within 60 s); running 0.1.0 again",
        created_at: "2026-10-09T01:00:00Z",
      }),
      jobFixture({
        id: "j-earlier",
        kind: "update_site",
        status: "done",
        created_at: "2026-10-08T00:00:00Z",
      }),
    ];
    renderWithClient(<SiteUpdates siteId="s1" appId="hello" currentVersion="0.1.0" />);
    const outcome = await screen.findByTestId("update-outcome");
    expect(outcome).toHaveTextContent(/rolled back/i);
    expect(outcome).toHaveTextContent(/health failed/i);
    expect(within(outcome).getByRole("link", { name: /job/i })).toHaveAttribute(
      "href",
      "/jobs/job-update",
    );
  }, 20000);

  it("update dialog asks one labelled field per prompt of the target version", async () => {
    const calls = stubFetch((url, method) => {
      if (url.endsWith("/api/v1/apps/hello/versions")) return json(versionsBody);
      if (url.endsWith("/api/v1/apps/hello/versions/0.10.0/prompts")) {
        return json({
          prompts: [
            { name: "SMTP_HOST", label: "Mail server", secret: false },
            { name: "SMTP_PASSWORD", label: "Mail password", secret: true },
          ],
        });
      }
      if (url.includes("/api/v1/jobs")) return json({ jobs: [] });
      if (url.endsWith("/api/v1/sites/s1/update") && method === "POST") {
        return json({ site: { id: "s1" }, job: jobFixture({ id: "j9", kind: "update_site" }) }, 202);
      }
      return notFound();
    });
    const user = userEvent.setup();
    renderWithClient(<SiteUpdates siteId="s1" appId="hello" currentVersion="0.1.0" />);
    await user.click(await screen.findByRole("button", { name: /update to 0\.10\.0/i }));
    const dialog = await screen.findByRole("dialog");
    const host = await within(dialog).findByLabelText("Mail server");
    const pw = within(dialog).getByLabelText("Mail password");
    expect(pw).toHaveAttribute("type", "password");
    expect(within(dialog).queryByLabelText(/new prompt answers/i)).not.toBeInTheDocument();
    await user.type(host, "mail.example.com");
    await user.type(pw, "pw-secret-1");
    await user.click(within(dialog).getByTestId("confirm-accept"));
    await waitFor(() => expect(calls.some((c) => c.method === "POST")).toBe(true));
    expect(calls.find((c) => c.method === "POST")?.body).toEqual({
      version: "0.10.0",
      prompts: { SMTP_HOST: "mail.example.com", SMTP_PASSWORD: "pw-secret-1" },
    });
  });
});
