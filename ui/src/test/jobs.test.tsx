import { screen, waitFor, within } from "@testing-library/react";
import { beforeEach, describe, expect, it } from "vitest";
import { JobLogView } from "@/components/JobLogView";
import { JobsTable } from "@/components/JobsTable";
import type { Job } from "@/lib/client";
import {
  MockEventSource,
  installEventSource,
  jobFixture,
  json,
  notFound,
  renderWithClient,
  stubFetch,
} from "./helpers";

const jobs = [
  jobFixture({ id: "j1", status: "running", log: "step install: ok" }),
  jobFixture({ id: "j2", kind: "backup_site", status: "done", log: "step backup: ok" }),
  jobFixture({ id: "j3", kind: "update_site", status: "failed", error: "health failed" }),
  jobFixture({ id: "j4", kind: "delete_backup", status: "queued", site_id: null }),
] as Job[];

describe("jobs", () => {
  beforeEach(() => {
    installEventSource();
  });

  it("jobs: list and live log", async () => {
    // List: running/queued first, then history; badges per status.
    const { unmount } = renderWithClient(<JobsTable jobs={jobs} />);
    const running = screen.getByRole("row", { name: /provision_site.*running/ });
    expect(within(running).getByRole("link", { name: /j1/ })).toHaveAttribute("href", "/jobs/j1");
    const done = screen.getByRole("row", { name: /backup_site/ });
    expect(within(done).getByText("done")).toHaveClass("text-green-700");
    const failed = screen.getByRole("row", { name: /update_site/ });
    expect(within(failed).getByText("failed")).toHaveClass("bg-destructive");
    expect(within(failed).getByText("health failed")).toBeInTheDocument();
    const queued = screen.getByRole("row", { name: /delete_backup/ });
    expect(within(queued).getByText("queued")).toHaveClass("text-yellow-700");
    unmount();

    // Live log: the stream replays the stored backlog, then tails; no
    // line renders twice, not even after a reconnect.
    stubFetch((url) =>
      url.endsWith("/api/v1/jobs/j1")
        ? json(jobFixture({ id: "j1", status: "running", log: "step install: ok" }))
        : notFound(),
    );
    renderWithClient(<JobLogView jobId="j1" />);
    const log = await screen.findByTestId("job-log");
    expect(log).toHaveTextContent("step install: ok");
    await waitFor(() => expect(MockEventSource.instances).toHaveLength(1));
    const source = MockEventSource.latest();
    expect(source.url).toBe("/api/v1/jobs/j1/log");
    source.emit("step install: ok");
    source.emit("step health: ok");
    await waitFor(() => expect(log.textContent).toBe("step install: ok\nstep health: ok"));
    source.reopen();
    source.emit("step install: ok");
    source.emit("step health: ok");
    source.emit("step finish: ok");
    await waitFor(() =>
      expect(log.textContent).toBe("step install: ok\nstep health: ok\nstep finish: ok"),
    );
  }, 20000);

  it("finished jobs do not subscribe to the live log", async () => {
    stubFetch((url) =>
      url.endsWith("/api/v1/jobs/j2")
        ? json(jobFixture({ id: "j2", kind: "backup_site", status: "done", log: "all done" }))
        : notFound(),
    );
    renderWithClient(<JobLogView jobId="j2" />);
    expect(await screen.findByText(/all done/)).toBeInTheDocument();
    expect(screen.getByText("done")).toBeInTheDocument();
    await new Promise((resolve) => setTimeout(resolve, 50));
    expect(MockEventSource.instances).toHaveLength(0);
  });
});
