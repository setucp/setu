import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { LoginForm } from "@/components/LoginForm";

function renderForm(onSuccess?: (op: { email: string }) => void) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  });
  return render(
    <QueryClientProvider client={client}>
      <LoginForm onSuccess={onSuccess} />
    </QueryClientProvider>,
  );
}

describe("login", () => {
  beforeEach(() => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => new Response(JSON.stringify({}), { status: 500 })),
    );
  });

  it("renders email and password fields", async () => {
    renderForm();
    expect(await screen.findByLabelText(/email/i)).toBeInTheDocument();
    expect(screen.getByLabelText(/password/i)).toBeInTheDocument();
  });

  it("posts credentials and reports the signed-in operator", async () => {
    const fetchMock = vi.fn(async (url: unknown, init?: RequestInit) => {
      expect(String(url)).toContain("/api/v1/auth/login");
      expect(init?.method).toBe("POST");
      expect(JSON.parse(String(init?.body))).toEqual({
        email: "owner@example.com",
        password: "correct-horse",
      });
      return new Response(
        JSON.stringify({
          id: "op1",
          email: "owner@example.com",
          role: "owner",
          created_at: new Date().toISOString(),
        }),
        { status: 200, headers: { "content-type": "application/json" } },
      );
    });
    vi.stubGlobal("fetch", fetchMock);
    const onSuccess = vi.fn();

    renderForm(onSuccess);
    await userEvent.type(await screen.findByLabelText(/email/i), "owner@example.com");
    await userEvent.type(screen.getByLabelText(/password/i), "correct-horse");
    await userEvent.click(screen.getByRole("button", { name: /sign in/i }));

    await waitFor(() => expect(fetchMock).toHaveBeenCalled());
    await waitFor(() =>
      expect(screen.getByText(/signed in as owner@example.com/i)).toBeInTheDocument(),
    );
    expect(onSuccess).toHaveBeenCalledWith(
      expect.objectContaining({ email: "owner@example.com" }),
    );
  });

  it("shows the server message on wrong credentials", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(
        async () =>
          new Response(JSON.stringify({ error: { code: "unauthorized", message: "bad login" } }), {
            status: 401,
            headers: { "content-type": "application/json" },
          }),
      ),
    );
    renderForm();
    await userEvent.type(await screen.findByLabelText(/email/i), "owner@example.com");
    await userEvent.type(screen.getByLabelText(/password/i), "wrong");
    await userEvent.click(screen.getByRole("button", { name: /sign in/i }));
    expect(await screen.findByText(/bad login/i)).toBeInTheDocument();
  });
});
