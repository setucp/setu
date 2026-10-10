import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import { SetupStepper, SetupWizard } from "@/components/SetupWizard";
import { LoginForm } from "@/components/LoginForm";
import { renderWithClient, json, notFound, stubFetch } from "./helpers";

describe("setup stepper", () => {
  it("marks done, current and upcoming steps", () => {
    render(<SetupStepper current={3} />);
    const steps = within(screen.getByRole("list", { name: /setup progress/i })).getAllByRole("listitem");
    expect(steps.map((s) => s.getAttribute("data-state"))).toEqual([
      "complete",
      "complete",
      "current",
      "upcoming",
    ]);
    expect(steps[2]).toHaveAttribute("aria-current", "step");
    expect(steps[2]).toHaveTextContent("Server");
    expect(steps[0]).toHaveTextContent(/token.*done/i);
  });

  it("the wizard advances the stepper from token to owner", async () => {
    stubFetch((url) =>
      url.endsWith("/api/v1/setup") ? json({ needed: true, public_url: "https://p" }) : notFound(),
    );
    const user = userEvent.setup();
    render(<SetupWizard />);
    const field = await screen.findByLabelText(/setup token/i);
    let current = screen.getByRole("list", { name: /setup progress/i }).querySelector('[aria-current="step"]');
    expect(current).toHaveTextContent("Token");
    await user.type(field, "tok");
    await user.click(screen.getByRole("button", { name: /continue/i }));
    await screen.findByLabelText(/email/i);
    current = screen.getByRole("list", { name: /setup progress/i }).querySelector('[aria-current="step"]');
    expect(current).toHaveTextContent("Owner");
  });
});

describe("login error state", () => {
  it("announces the error, marks the fields invalid and clears on success", async () => {
    let attempt = 0;
    stubFetch(() => {
      attempt += 1;
      return attempt === 1
        ? json({ error: { code: "unauthorized", message: "invalid email or password" } }, 401)
        : json({ id: "op1", email: "owner@example.com", role: "owner", created_at: "2026-10-01T00:00:00Z" });
    });
    const user = userEvent.setup();
    renderWithClient(<LoginForm />);
    await user.type(screen.getByLabelText(/email/i), "owner@example.com");
    await user.type(screen.getByLabelText(/password/i), "wrong");
    await user.click(screen.getByRole("button", { name: /sign in/i }));

    expect(await screen.findByRole("alert")).toHaveTextContent(/invalid email or password/i);
    expect(screen.getByLabelText(/email/i)).toHaveAttribute("aria-invalid", "true");
    expect(screen.getByLabelText(/password/i)).toHaveAttribute("aria-invalid", "true");

    await user.click(screen.getByRole("button", { name: /sign in/i }));
    expect(await screen.findByText(/signed in as owner@example\.com/i)).toBeInTheDocument();
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
    expect(screen.getByLabelText(/email/i)).not.toHaveAttribute("aria-invalid");
  });
});
