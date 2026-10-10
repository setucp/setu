import { describe, expect, it } from "vitest";
import { parseSteps } from "@/components/JobLogView";
import { initials } from "@/components/ui/avatar";
import { crumbsFor } from "@/components/shell/Breadcrumbs";
import { duration, relativeTime } from "@/lib/time";

const NOW = Date.parse("2026-10-10T12:00:00Z");

describe("small helpers", () => {
  it("relative times read naturally", () => {
    expect(relativeTime("2026-10-10T11:59:40Z", NOW)).toBe("just now");
    expect(relativeTime("2026-10-10T11:55:00Z", NOW)).toBe("5 minutes ago");
    expect(relativeTime("2026-10-10T09:00:00Z", NOW)).toBe("3 hours ago");
    expect(relativeTime("2026-10-09T12:00:00Z", NOW)).toBe("yesterday");
    expect(relativeTime("2026-10-10T14:00:00Z", NOW)).toBe("in 2 hours");
    expect(relativeTime(null, NOW)).toBe("—");
  });

  it("durations", () => {
    expect(duration("2026-10-10T12:00:00Z", "2026-10-10T12:00:00.250Z")).toBe("250 ms");
    expect(duration("2026-10-10T12:00:00Z", "2026-10-10T12:00:42Z")).toBe("42 s");
    expect(duration("2026-10-10T12:00:00Z", "2026-10-10T12:03:12Z")).toBe("3 m 12 s");
    expect(duration(null, null)).toBe("—");
  });

  it("job steps come from the log", () => {
    const steps = parseSteps(
      "starting\nstep install: ok\nstep health: error no 200 within 60 s\nstep backup: ignored (the site is failed)\nstep health: ok",
    );
    expect(steps).toEqual([
      { name: "install", outcome: "ok", detail: "" },
      { name: "health", outcome: "ok", detail: "" },
      { name: "backup", outcome: "ignored", detail: "(the site is failed)" },
    ]);
  });

  it("initials and breadcrumbs", () => {
    expect(initials("ada.lovelace@example.com")).toBe("AL");
    expect(initials("ops@example.com")).toBe("OP");
    expect(crumbsFor("/sites/new").map((c) => (c.kind === "text" ? c.label : c.kind))).toEqual([
      "Sites",
      "New site",
    ]);
    expect(crumbsFor("/servers/srv1").map((c) => c.kind)).toEqual(["text", "server"]);
    expect(crumbsFor("/elsewhere")[0]).toMatchObject({ label: "Not found" });
  });
});
