import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { RouterProvider, createMemoryHistory, createRouter } from "@tanstack/react-router";
import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import { routeTree } from "@/routeTree.gen";
import {
  json,
  notFound,
  planFixture,
  serverFixture,
  siteFixture,
  stubFetch,
} from "./helpers";

const me = { id: "op1", email: "ada.lovelace@example.com", role: "owner", created_at: "2026-10-01T00:00:00Z" };

function stubApi() {
  return stubFetch((url) => {
    if (url === "/healthz") return json({ status: "ok" });
    if (url.endsWith("/api/v1/auth/me")) return json(me);
    if (url.endsWith("/api/v1/sites")) return json({ sites: [siteFixture({ name: "vyasa-test" })] });
    if (url.endsWith("/api/v1/sites/s1")) return json({ ...siteFixture({ name: "vyasa-test" }), metrics: null });
    if (url.endsWith("/api/v1/sites/s1/backups")) return json({ backups: [] });
    if (url.endsWith("/api/v1/plans")) return json({ plans: [planFixture] });
    if (url.endsWith("/api/v1/servers")) return json({ servers: [serverFixture({ id: "srv1", name: "web1" })] });
    if (url.includes("/api/v1/jobs")) return json({ jobs: [] });
    return notFound();
  });
}

function renderApp(path: string) {
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  });
  const router = createRouter({
    routeTree,
    context: { queryClient },
    history: createMemoryHistory({ initialEntries: [path] }),
  });
  render(
    <QueryClientProvider client={queryClient}>
      <RouterProvider router={router} />
    </QueryClientProvider>,
  );
  return router;
}

describe("app shell", () => {
  it("renders the navigation, status pill and operator menu", async () => {
    stubApi();
    const user = userEvent.setup();
    renderApp("/sites");

    const nav = await screen.findByRole("navigation", { name: "Main" });
    for (const label of ["Sites", "Servers", "Apps", "Plans", "Jobs", "Settings"]) {
      expect(within(nav).getByRole("link", { name: label })).toBeInTheDocument();
    }
    expect(within(nav).getByRole("link", { name: "Sites" })).toHaveAttribute("aria-current", "page");
    expect(within(nav).getByRole("link", { name: "Servers" })).not.toHaveAttribute("aria-current");
    expect(screen.getByRole("main")).toBeInTheDocument();
    expect(await screen.findByText("Control plane healthy")).toBeInTheDocument();

    const [menu] = screen.getAllByRole("button", { name: /account menu for ada\.lovelace@example\.com/i });
    await user.click(menu!);
    const opened = await screen.findByRole("menu");
    expect(within(opened).getByText("owner")).toBeInTheDocument();
    expect(within(opened).getByRole("menuitem", { name: /change password/i })).toBeInTheDocument();
    expect(within(opened).getByRole("menuitem", { name: /theme/i })).toBeInTheDocument();
    expect(within(opened).getByRole("menuitem", { name: /log out/i })).toBeInTheDocument();
  }, 20000);

  it("the phone drawer opens and navigates", async () => {
    stubApi();
    const user = userEvent.setup();
    const router = renderApp("/sites");
    const open = await screen.findByRole("button", { name: /open navigation/i });
    expect(open).toHaveAttribute("aria-expanded", "false");
    await user.click(open);
    const drawer = await screen.findByRole("dialog", { name: "Navigation" });
    await user.click(within(drawer).getByRole("link", { name: "Servers" }));
    await waitFor(() => expect(router.state.location.pathname).toBe("/servers"));
    await waitFor(() =>
      expect(screen.queryByRole("dialog", { name: "Navigation" })).not.toBeInTheDocument(),
    );
    expect(await screen.findByRole("heading", { level: 1, name: "Servers" })).toBeInTheDocument();
  }, 20000);

  it("breadcrumbs follow the route and name the site", async () => {
    stubApi();
    renderApp("/sites/s1/backups");
    const [trail] = await screen.findAllByRole("navigation", { name: "Breadcrumb" });
    await waitFor(() => expect(trail).toHaveTextContent("Sites"));
    await waitFor(() => expect(within(trail!).getByText("vyasa-test")).toBeInTheDocument());
    expect(within(trail!).getByRole("link", { name: "Sites" })).toHaveAttribute("href", "/sites");
    expect(within(trail!).getByRole("link", { name: "vyasa-test" })).toHaveAttribute("href", "/sites/s1");
    expect(within(trail!).getByText("Backups")).toHaveAttribute("aria-current", "page");
  }, 20000);

  it("unknown paths show the not-found card inside the shell", async () => {
    stubApi();
    renderApp("/no/such/page");
    expect(await screen.findByRole("heading", { name: /page not found/i })).toBeInTheDocument();
    expect(screen.getByRole("link", { name: /back to sites/i })).toHaveAttribute("href", "/sites");
  }, 20000);
});
