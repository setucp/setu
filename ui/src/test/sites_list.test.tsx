import { render, screen, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { SitesTable } from "@/components/SitesTable";
import type { Plan, Server, Site } from "@/lib/client";

const sites: Site[] = [
  {
    id: "site1",
    name: "blog",
    app_id: "hello",
    app_version: "0.1.0",
    server_id: "srv1",
    plan_id: "plan1",
    domain: "blog.example.com",
    status: "running",
    port: 20001,
    db_name: "site_abcd1234",
    owner_email: "owner@example.com",
    failed_step: null,
    created_at: "2026-10-01T00:00:00Z",
    updated_at: "2026-10-01T00:00:00Z",
  },
  {
    id: "site2",
    name: "shop",
    app_id: "hello",
    app_version: "0.1.1",
    server_id: "srv1",
    plan_id: "plan1",
    domain: "shop.example.com",
    status: "failed",
    port: 20002,
    db_name: "site_efgh5678",
    owner_email: "owner@example.com",
    failed_step: "health",
    created_at: "2026-10-02T00:00:00Z",
    updated_at: "2026-10-02T00:00:00Z",
  },
];

const plans: Plan[] = [
  {
    id: "plan1",
    name: "standard",
    limits: { memory_mb: 512, disk_mb: 5120 },
    allowed_app_ids: ["hello"],
    created_at: "2026-10-01T00:00:00Z",
    updated_at: "2026-10-01T00:00:00Z",
  },
];

const servers: Server[] = [
  {
    id: "srv1",
    name: "web1",
    status: "online",
    cordoned: false,
    is_control_plane: false,
    created_at: "2026-10-01T00:00:00Z",
    updated_at: "2026-10-01T00:00:00Z",
  },
];

describe("sites list", () => {
  it("shows one row per site with badges and links", () => {
    render(<SitesTable sites={sites} plans={plans} servers={servers} />);
    const row = screen.getByRole("row", { name: /blog\.example\.com/i });
    expect(within(row).getByText("blog")).toBeInTheDocument();
    expect(within(row).getByText("hello 0.1.0")).toBeInTheDocument();
    const link = within(row).getByRole("link", { name: /blog/i });
    expect(link).toHaveAttribute("href", "/sites/site1");
    expect(within(row).getByText("running")).toBeInTheDocument();
    expect(within(row).getByText("512M")).toBeInTheDocument();
  });

  it("marks failed sites distinctly", () => {
    render(<SitesTable sites={sites} plans={plans} servers={servers} />);
    const row = screen.getByRole("row", { name: /shop\.example\.com/i });
    expect(within(row).getByText("failed")).toBeInTheDocument();
    expect(within(row).getByText(/health/i)).toBeInTheDocument();
  });

  it("renders an empty state when there are no sites", () => {
    render(<SitesTable sites={[]} plans={plans} servers={servers} />);
    expect(screen.getByText(/no sites yet/i)).toBeInTheDocument();
  });
});
