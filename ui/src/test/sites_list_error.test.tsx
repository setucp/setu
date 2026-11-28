import { screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import { SitesListPage } from "@/components/SitesListPage";
import {
  json,
  planFixture,
  renderWithClient,
  serverFixture,
  siteFixture,
  stubFetch,
} from "./helpers";

describe("sites list load failure", () => {
  it("shows an error with Retry instead of a permanent skeleton, and recovers", async () => {
    let sitesCalls = 0;
    stubFetch((url) => {
      if (url.startsWith("/api/v1/sites")) {
        sitesCalls += 1;
        if (sitesCalls === 1) {
          return json({ error: { code: "internal", message: "database is slow" } }, 500);
        }
        return json({ sites: [{ ...siteFixture({ id: "site1" }), metrics: null }] });
      }
      if (url.startsWith("/api/v1/plans")) return json({ plans: [planFixture] });
      return json({ servers: [serverFixture({ id: "srv1" })] });
    });
    renderWithClient(<SitesListPage />);

    expect(await screen.findByText(/database is slow/i)).toBeInTheDocument();
    expect(screen.queryByText("Loading sites…")).not.toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Retry" }));

    await waitFor(() => expect(screen.getAllByRole("row").length).toBeGreaterThan(1));
    expect(screen.queryByRole("button", { name: "Retry" })).not.toBeInTheDocument();
    expect(sitesCalls).toBe(2);
  });
});
