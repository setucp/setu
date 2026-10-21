import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import * as React from "react";
import { describe, expect, it, vi } from "vitest";
import { AppsCatalogue } from "@/components/AppsCatalogue";
import { PublishAppDialog } from "@/components/PublishAppDialog";

function json(body: unknown, status = 200) {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json" },
  });
}

const appsBody = {
  apps: [{ id: "hello", name: "Hello", homepage: "https://example.com/hello" }],
};

const versionsBody = {
  versions: [
    {
      id: "v1",
      app_id: "hello",
      version: "0.1.0",
      channel: "stable",
      published_at: "2026-10-01T00:00:00Z",
    },
  ],
};

function renderWithClient(ui: React.ReactElement) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  });
  return render(<QueryClientProvider client={client}>{ui}</QueryClientProvider>);
}

describe("apps", () => {
  it("catalogue lists apps with their versions", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async (url: unknown) => {
        const path = String(url);
        if (path.endsWith("/api/v1/apps")) return json(appsBody);
        if (path.includes("/api/v1/apps/hello/versions")) return json(versionsBody);
        return new Response("not found", { status: 404 });
      }),
    );
    renderWithClient(<AppsCatalogue />);
    expect(await screen.findByText("Hello")).toBeInTheDocument();
    expect(await screen.findByText("0.1.0")).toBeInTheDocument();
  });

  it("a followed app shows its URL and Check now runs a check", async () => {
    const checks: string[] = [];
    vi.stubGlobal(
      "fetch",
      vi.fn(async (url: unknown, init?: RequestInit) => {
        const path = String(url);
        if (path.endsWith("/api/v1/apps/hello/check") && init?.method === "POST") {
          checks.push(path);
          return json({
            published: "0.2.0",
            source: {
              url: "https://m.example/hello.toml",
              checked_at: "2026-10-21T00:00:00Z",
              last_error: null,
            },
          });
        }
        if (path.endsWith("/api/v1/apps")) {
          return json({
            apps: [
              {
                ...appsBody.apps[0],
                source: {
                  url: "https://m.example/hello.toml",
                  checked_at: null,
                  last_error: "manifest fetch failed with status 404",
                },
              },
            ],
          });
        }
        if (path.includes("/api/v1/apps/hello/versions")) return json(versionsBody);
        return new Response("not found", { status: 404 });
      }),
    );
    renderWithClient(<AppsCatalogue />);
    expect(await screen.findByText("m.example/hello.toml")).toBeInTheDocument();
    expect(screen.getByText(/Last check failed: manifest fetch failed/)).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: /Check now/ }));
    await waitFor(() => expect(checks).toHaveLength(1));
  });

  it("publish validates the manifest inline before submitting", async () => {
    const validates: unknown[] = [];
    const publishes: unknown[] = [];
    vi.stubGlobal(
      "fetch",
      vi.fn(async (url: unknown, init?: RequestInit) => {
        const path = String(url);
        if (path.endsWith("/api/v1/apps/validate") && init?.method === "POST") {
          const body = JSON.parse(String(init.body));
          validates.push(body);
          if (String(body.toml).includes("colour")) {
            return json({
              ok: false,
              errors: [{ path: "[app]", line: 3, message: "unknown field 'colour'" }],
            });
          }
          return json({ ok: true, errors: [] });
        }
        if (path.endsWith("/api/v1/apps") && init?.method === "POST") {
          publishes.push(JSON.parse(String(init.body)));
          return json(
            {
              app: { id: "hello", name: "Hello" },
              version: { id: "v1", app_id: "hello", version: "0.1.0" },
            },
            201,
          );
        }
        return new Response("not found", { status: 404 });
      }),
    );
    const user = userEvent.setup();
    const onPublished = vi.fn();
    renderWithClient(<PublishAppDialog open onClose={() => undefined} onPublished={onPublished} />);

    // Publish is gated on a fresh validation: no validate, no submit.
    expect(screen.getByRole("button", { name: /^publish$/i })).toBeDisabled();

    await user.type(screen.getByLabelText(/manifest toml/i), 'colour = "red"');
    await user.click(screen.getByRole("button", { name: /validate/i }));
    expect(await screen.findByText(/unknown field 'colour'/)).toBeInTheDocument();
    expect(validates).toHaveLength(1);
    expect(publishes).toHaveLength(0);

    await user.clear(screen.getByLabelText(/manifest toml/i));
    await user.type(screen.getByLabelText(/manifest toml/i), "schema = 1");
    // Editing after a validation re-arms the gate.
    expect(screen.getByRole("button", { name: /^publish$/i })).toBeDisabled();
    await user.click(screen.getByRole("button", { name: /validate/i }));
    expect(await screen.findByText(/manifest is valid/i)).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: /^publish$/i }));
    await waitFor(() => expect(publishes).toHaveLength(1));
    expect(publishes[0]).toEqual({ toml: "schema = 1" });
    expect(onPublished).toHaveBeenCalled();
  });
});
