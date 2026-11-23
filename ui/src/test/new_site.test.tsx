import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { NewSiteWizard } from "@/components/NewSiteWizard";

const apps = {
  apps: [{ id: "hello", name: "Hello", homepage: null }],
};
const versions = {
  versions: [
    { id: "v1", app_id: "hello", version: "0.1.0", channel: "stable", published_at: "2026-10-01T00:00:00Z" },
  ],
};
const plans = {
  plans: [
    {
      id: "plan1",
      name: "standard",
      limits: { memory_mb: 512, disk_mb: 5120 },
      allowed_app_ids: ["hello"],
      created_at: "2026-10-01T00:00:00Z",
      updated_at: "2026-10-01T00:00:00Z",
    },
  ],
};
const servers = {
  servers: [{ id: "srv1", name: "web1", status: "online", created_at: "", updated_at: "" }],
};

const domainOk = {
  domain: "blog.example.com",
  server_ip: "203.0.113.7",
  ok: true,
  findings: [
    { id: "dns", level: "ok", message: "resolves to 203.0.113.7" },
    { id: "points_at_server", level: "ok", message: "points at the server (203.0.113.7)" },
    { id: "proxy", level: "ok", message: "not behind a proxy" },
    { id: "caa", level: "ok", message: "no CAA record; any CA may issue" },
  ],
};

const domainBad = {
  domain: "blog.example.com",
  server_ip: "203.0.113.7",
  ok: false,
  findings: [
    { id: "dns", level: "ok", message: "resolves to 198.51.100.1" },
    { id: "points_at_server", level: "error", message: "points at 198.51.100.1, not 203.0.113.7" },
    { id: "proxy", level: "warn", message: "behind Cloudflare's proxy" },
    { id: "caa", level: "ok", message: "no CAA record; any CA may issue" },
  ],
};

function json(body: unknown, status = 200) {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json" },
  });
}

function stubApi(posts: { url: string; init?: RequestInit }[] = []) {
  const fetchMock = vi.fn(async (url: unknown, init?: RequestInit) => {
    const path = String(url);
    if (init?.method === "POST" && path.endsWith("/api/v1/sites")) {
      posts.push({ url: path, init });
      return json({
        site: { id: "site9", domain: "blog.example.com" },
        job: { id: "job7", status: "queued" },
      });
    }
    if (path.endsWith("/api/v1/apps")) return json(apps);
    if (path.includes("/api/v1/apps/hello/versions")) return json(versions);
    if (path.endsWith("/api/v1/plans")) return json(plans);
    if (path.endsWith("/api/v1/servers")) return json(servers);
    if (path.includes("/api/v1/domain-check")) return json(domainOk);
    return new Response("not found", { status: 404 });
  });
  vi.stubGlobal("fetch", fetchMock);
  return fetchMock;
}

function renderWizard(onCreated?: (jobId: string) => void) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  });
  return render(
    <QueryClientProvider client={client}>
      <NewSiteWizard onCreated={onCreated} />
    </QueryClientProvider>,
  );
}

async function fillValid(user: ReturnType<typeof userEvent.setup>) {
  await screen.findByRole("option", { name: "Hello" });
  await user.selectOptions(await screen.findByLabelText(/application/i), "hello");
  await screen.findByRole("option", { name: "0.1.0" });
  await user.selectOptions(screen.getByLabelText(/version/i), "0.1.0");
  await screen.findByRole("option", { name: "standard" });
  await user.selectOptions(screen.getByLabelText(/plan/i), "plan1");
  await user.type(screen.getByLabelText(/site name/i), "blog");
  await user.type(screen.getByLabelText(/domain/i), "blog.example.com");
  await user.type(screen.getByLabelText(/owner email/i), "owner@example.com");
}

describe("new site wizard", () => {
  beforeEach(() => {
    stubApi();
  });

  it("rejects an invalid domain without posting", async () => {
    const user = userEvent.setup();
    const posts: { url: string; init?: RequestInit }[] = [];
    stubApi(posts);
    renderWizard();
    await fillValid(user);
    const domain = screen.getByLabelText(/domain/i);
    await user.clear(domain);
    await user.type(domain, "not a domain");
    await user.click(screen.getByRole("button", { name: /create site/i }));
    expect(
      await screen.findByText(/enter a valid domain/i, {}, { timeout: 5000 }),
    ).toBeInTheDocument();
    expect(posts).toHaveLength(0);
  });

  it("posts the payload then reports the job", async () => {
    const user = userEvent.setup();
    const posts: { url: string; init?: RequestInit }[] = [];
    stubApi(posts);
    const onCreated = vi.fn();
    renderWizard(onCreated);
    await fillValid(user);
    await user.type(screen.getByLabelText(/prompt values/i), "HELLO_NAME=blog\n");
    await user.click(screen.getByRole("button", { name: /create site/i }));

    await waitFor(() => expect(posts).toHaveLength(1));
    const body = JSON.parse(String(posts[0]?.init?.body));
    expect(body).toEqual({
      name: "blog",
      app_id: "hello",
      version: "0.1.0",
      plan_id: "plan1",
      domain: "blog.example.com",
      owner_email: "owner@example.com",
      prompts: { HELLO_NAME: "blog" },
    });
    await waitFor(() => expect(onCreated).toHaveBeenCalledWith("job7"));
  });

  it("rejects unparsable prompt lines without posting", async () => {
    const user = userEvent.setup();
    const posts: { url: string; init?: RequestInit }[] = [];
    stubApi(posts);
    renderWizard();
    await fillValid(user);
    await user.type(screen.getByLabelText(/prompt values/i), "HELLO_NAME blog\n");
    await user.click(screen.getByRole("button", { name: /create site/i }));
    expect(await screen.findByText(/must be KEY=value lines/i)).toBeInTheDocument();
    expect(posts).toHaveLength(0);
  });

  it("shows the server message when the domain is taken", async () => {
    const user = userEvent.setup();
    vi.stubGlobal(
      "fetch",
      vi.fn(async (url: unknown, init?: RequestInit) => {
        const path = String(url);
        if (init?.method === "POST" && path.endsWith("/api/v1/sites")) {
          return json(
            { error: { code: "conflict", message: "domain is already taken" } },
            409,
          );
        }
        if (path.endsWith("/api/v1/apps")) return json(apps);
        if (path.includes("/versions")) return json(versions);
        if (path.endsWith("/api/v1/plans")) return json(plans);
        if (path.endsWith("/api/v1/servers")) return json(servers);
        return new Response("not found", { status: 404 });
      }),
    );
    renderWizard();
    await fillValid(user);
    await user.click(screen.getByRole("button", { name: /create site/i }));
    expect(await screen.findByText(/domain is already taken/i)).toBeInTheDocument();
  });
});

describe("domain pre-check", { timeout: 15_000 }, () => {
  function stubWith(opts: { serverList?: unknown; check?: unknown; posts?: { url: string }[] }) {
    const checks: string[] = [];
    vi.stubGlobal(
      "fetch",
      vi.fn(async (url: unknown, init?: RequestInit) => {
        const path = String(url);
        if (init?.method === "POST" && path.endsWith("/api/v1/sites")) {
          opts.posts?.push({ url: path });
          return json({ site: { id: "site9" }, job: { id: "job7", status: "queued" } });
        }
        if (path.endsWith("/api/v1/apps")) return json(apps);
        if (path.includes("/versions")) return json(versions);
        if (path.endsWith("/api/v1/plans")) return json(plans);
        if (path.endsWith("/api/v1/servers")) return json(opts.serverList ?? servers);
        if (path.includes("/api/v1/domain-check")) {
          checks.push(path);
          return json(opts.check ?? domainOk);
        }
        return new Response("not found", { status: 404 });
      }),
    );
    return checks;
  }

  it("checks the domain against the only server once typing stops", async () => {
    const user = userEvent.setup();
    const checks = stubWith({});
    renderWizard();
    await fillValid(user);
    const section = await screen.findByRole("region", { name: /dns check/i }, { timeout: 3000 });
    expect(section).toHaveTextContent(/points at the server; https can be issued/i);
    expect(section).toHaveTextContent(/points at the server \(203\.0\.113\.7\)/);
    expect(section).toHaveTextContent("203.0.113.7");
    // One request, for the whole domain, not one per keystroke.
    expect(checks).toHaveLength(1);
    const q = new URL(checks[0] ?? "", "http://x").searchParams;
    expect(q.get("domain")).toBe("blog.example.com");
    expect(q.get("server_id")).toBe("srv1");
  });

  it("reports an error but still lets the site be created", async () => {
    const user = userEvent.setup();
    const posts: { url: string }[] = [];
    stubWith({ check: domainBad, posts });
    renderWizard();
    await fillValid(user);
    const section = await screen.findByRole("region", { name: /dns check/i }, { timeout: 3000 });
    expect(section).toHaveTextContent(/https will not work until this is fixed/i);
    expect(screen.getByRole("img", { name: "Problem" })).toBeInTheDocument();
    expect(screen.getByRole("img", { name: "Warning" })).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: /create site/i }));
    await waitFor(() => expect(posts).toHaveLength(1));
  });

  it("asks for a server when placement is automatic among several", async () => {
    const user = userEvent.setup();
    const two = {
      servers: [
        ...servers.servers,
        { id: "srv2", name: "web2", status: "online", created_at: "", updated_at: "" },
      ],
    };
    const checks = stubWith({ serverList: two });
    renderWizard();
    await fillValid(user);
    expect(await screen.findByText(/choose a server in step 2/i)).toBeInTheDocument();
    expect(checks).toHaveLength(0);
    await user.selectOptions(screen.getByLabelText(/^server$/i), "srv2");
    await screen.findByRole("region", { name: /dns check/i }, { timeout: 3000 });
    expect(new URL(checks[0] ?? "", "http://x").searchParams.get("server_id")).toBe("srv2");
  });

  it("checks again on request", async () => {
    const user = userEvent.setup();
    const checks = stubWith({});
    renderWizard();
    await fillValid(user);
    await screen.findByRole("region", { name: /dns check/i }, { timeout: 3000 });
    await user.click(screen.getByRole("button", { name: /check again/i }));
    await waitFor(() => expect(checks).toHaveLength(2));
  });

  it("does not check an invalid domain", async () => {
    const user = userEvent.setup();
    const checks = stubWith({});
    renderWizard();
    await screen.findByRole("option", { name: "Hello" });
    await user.type(screen.getByLabelText(/domain/i), "not a domain");
    await new Promise((r) => setTimeout(r, 800));
    expect(screen.queryByText(/checking dns/i)).not.toBeInTheDocument();
    expect(checks).toHaveLength(0);
  });
});
