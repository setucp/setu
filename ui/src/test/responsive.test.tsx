import { render, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { SitesTable } from "@/components/SitesTable";
import { PHONE_QUERY } from "@/lib/media";
import type { Plan, Server, Site } from "@/lib/client";
import { mockMatchMedia, planFixture, serverFixture, siteFixture } from "./helpers";

const sites = [
  siteFixture({ id: "s1", name: "blog", domain: "blog.example.com" }),
  siteFixture({ id: "s2", name: "shop", domain: "shop.example.com", status: "failed", failed_step: "health" }),
] as Site[];
const plans = [planFixture] as Plan[];
const servers = [serverFixture({ id: "srv1", name: "web1" })] as Server[];

let restore: (() => void) | null = null;
afterEach(() => {
  restore?.();
  restore = null;
});

describe("responsive table", () => {
  it("renders a table from 768 px up", () => {
    render(<SitesTable sites={sites} plans={plans} servers={servers} />);
    expect(screen.getByRole("table", { name: "Sites" })).toBeInTheDocument();
    expect(screen.queryByRole("list", { name: "Sites" })).not.toBeInTheDocument();
  });

  it("switches to one card per row on phones", () => {
    restore = mockMatchMedia([PHONE_QUERY]);
    render(<SitesTable sites={sites} plans={plans} servers={servers} />);
    expect(screen.queryByRole("table")).not.toBeInTheDocument();
    const list = screen.getByRole("list", { name: "Sites" });
    const cards = within(list).getAllByRole("listitem");
    expect(cards).toHaveLength(2);
    // Each card keeps the key fields: name link, domain, status, plan, server.
    const shop = cards[1]!;
    expect(within(shop).getByRole("link", { name: "shop" })).toHaveAttribute("href", "/sites/s2");
    expect(within(shop).getByText("shop.example.com")).toBeInTheDocument();
    expect(within(shop).getByText("failed")).toHaveAttribute("data-variant", "destructive");
    expect(within(shop).getByText(/failed at health/)).toBeInTheDocument();
    expect(within(shop).getByText("standard")).toBeInTheDocument();
    expect(within(shop).getByText("web1")).toBeInTheDocument();
    expect(within(shop).getByText("Memory")).toBeInTheDocument();
  });
});
