import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { SiteEnv } from "@/components/SiteEnv";
import { installClipboard, json, notFound, renderWithClient, stubFetch } from "./helpers";

const envBody = {
  env: [
    { name: "PORT", value: "20001", masked: false, secret: false, once: false, has_value: true },
    {
      name: "DATABASE_URL",
      value: "[MASKED_VALUE]",
      masked: true,
      secret: true,
      once: false,
      has_value: true,
    },
    {
      name: "VYASA_ADMIN_PASSWORD",
      value: "[MASKED_VALUE]",
      masked: true,
      secret: true,
      once: true,
      has_value: true,
    },
  ],
};

const SECRET = "Zx81pQ7mN2vB4cD6eF8g";

describe("env", () => {
  it("env: masked; reveal asks for the password, shows the value, explains once", async () => {
    const calls = stubFetch((url, method, body) => {
      if (url.endsWith("/api/v1/sites/s1/env")) return json(envBody);
      if (url.endsWith("/api/v1/sites/s1/env/VYASA_ADMIN_PASSWORD/reveal") && method === "POST") {
        const password = (body as { password: string }).password;
        if (password !== "my-own-password") {
          return json({ error: { code: "forbidden", message: "wrong password" } }, 403);
        }
        return json({ name: "VYASA_ADMIN_PASSWORD", value: SECRET });
      }
      return notFound();
    });
    const user = userEvent.setup();
    const writeText = installClipboard();
    renderWithClient(<SiteEnv siteId="s1" />);
    await screen.findByText("VYASA_ADMIN_PASSWORD");

    // Every secret is masked; plain values show.
    expect(screen.getAllByText("Masked value")).toHaveLength(2);
    expect(screen.getByText("20001")).toBeInTheDocument();
    expect(screen.queryByText(SECRET)).not.toBeInTheDocument();
    expect(screen.getAllByRole("button", { name: /reveal/i })).toHaveLength(2);

    const row = screen.getByRole("row", { name: /VYASA_ADMIN_PASSWORD/ });
    await user.click(within(row).getByRole("button", { name: /reveal/i }));
    const dialog = await screen.findByRole("dialog");
    expect(dialog).toHaveTextContent(/reveal VYASA_ADMIN_PASSWORD/i);
    // Explains `once`: first boot only, kept sealed for reveal.
    expect(dialog).toHaveTextContent(/first boot/i);
    expect(calls.some((c) => c.url.includes("/reveal"))).toBe(false);

    // A wrong password is refused and nothing is shown.
    await user.type(within(dialog).getByLabelText(/your password/i), "nope");
    await user.click(within(dialog).getByRole("button", { name: /^reveal$/i }));
    expect(await within(dialog).findByRole("alert")).toHaveTextContent(/wrong password/i);
    expect(screen.queryByText(SECRET)).not.toBeInTheDocument();

    await user.clear(within(dialog).getByLabelText(/your password/i));
    await user.type(within(dialog).getByLabelText(/your password/i), "my-own-password");
    await user.click(within(dialog).getByRole("button", { name: /^reveal$/i }));
    expect(await within(dialog).findByText(SECRET)).toBeInTheDocument();
    const reveal = calls.filter((c) => c.url.includes("/reveal"));
    expect(reveal.at(-1)?.body).toEqual({ password: "my-own-password" });
    // The password field is gone once the value shows.
    expect(within(dialog).queryByLabelText(/your password/i)).not.toBeInTheDocument();

    await user.click(within(dialog).getByRole("button", { name: /copy/i }));
    expect(writeText).toHaveBeenCalledWith(SECRET);

    // Closing forgets the value; the table is still masked.
    await user.click(within(dialog).getByRole("button", { name: /^done$/i }));
    await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
    expect(screen.queryByText(SECRET)).not.toBeInTheDocument();
    expect(screen.getAllByText("Masked value")).toHaveLength(2);
  }, 20000);

  it("env save puts the edited values and reports the restart job", async () => {
    const calls = stubFetch((url, method) => {
      if (url.endsWith("/api/v1/sites/s1/env") && method === "PUT") {
        return json({ site: { id: "s1" }, job: { id: "job-env", status: "queued" } }, 202);
      }
      if (url.endsWith("/api/v1/sites/s1/env")) return json(envBody);
      return notFound();
    });
    const user = userEvent.setup();
    const onSubmitted = vi.fn();
    renderWithClient(<SiteEnv siteId="s1" onSubmitted={onSubmitted} />);
    await screen.findByText("PORT");
    await user.click(screen.getByRole("button", { name: /edit/i }));
    await user.type(screen.getByLabelText(/new value for port/i), "20002");
    await user.click(screen.getByRole("button", { name: /save changes/i }));
    await waitFor(() => expect(onSubmitted).toHaveBeenCalledWith("job-env"));
    expect(calls.find((c) => c.method === "PUT")?.body).toEqual({ env: { PORT: "20002" } });
  });

  it("adds an extra variable and removes one", async () => {
    const withExtra = {
      env: [
        ...envBody.env.map((e) => ({ ...e, extra: false })),
        { name: "DEMO_MODE", value: "true", masked: false, secret: false, once: false, has_value: true, extra: true },
      ],
    };
    const calls = stubFetch((url, method) => {
      if (url.endsWith("/api/v1/sites/s1/env") && method === "PUT") {
        return json({ site: { id: "s1" }, job: { id: "job-env", status: "queued" } }, 202);
      }
      if (url.endsWith("/api/v1/sites/s1/env")) return json(withExtra);
      return notFound();
    });
    const user = userEvent.setup();
    renderWithClient(<SiteEnv siteId="s1" />);
    await screen.findByText("DEMO_MODE");
    // Only extra rows can be removed.
    expect(screen.getAllByRole("button", { name: /^remove /i })).toHaveLength(1);

    await user.click(screen.getByRole("button", { name: /add variable/i }));
    const dialog = await screen.findByRole("dialog");
    await user.type(within(dialog).getByLabelText(/^name/i), "port");
    expect(dialog).toHaveTextContent(/already has a variable/i);
    await user.clear(within(dialog).getByLabelText(/^name/i));
    await user.type(within(dialog).getByLabelText(/^name/i), "trusted_keys");
    await user.type(within(dialog).getByLabelText(/^value/i), "abc");
    await user.click(within(dialog).getByRole("checkbox"));
    await user.click(within(dialog).getByRole("button", { name: /add and restart/i }));
    await waitFor(() =>
      expect(calls.find((c) => c.method === "PUT")?.body).toEqual({
        extra: { TRUSTED_KEYS: { value: "abc", secret: true } },
      }),
    );

    await user.click(screen.getByRole("button", { name: /remove demo_mode/i }));
    await user.click(await screen.findByTestId("confirm-accept"));
    await waitFor(() =>
      expect(calls.filter((c) => c.method === "PUT").at(-1)?.body).toEqual({
        remove: ["DEMO_MODE"],
      }),
    );
  }, 20000);
});
