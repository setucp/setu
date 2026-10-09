import { createFileRoute, useNavigate } from "@tanstack/react-router";
import { LoginForm } from "@/components/LoginForm";

export const Route = createFileRoute("/login")({
  validateSearch: (search: Record<string, unknown>): { redirect?: string } =>
    typeof search["redirect"] === "string" && isLocalPath(search["redirect"])
      ? { redirect: search["redirect"] }
      : {},
  component: LoginPage,
});

/// Post-login target: same-origin paths only (`/sites`, never
/// `https://…` or `//evil`). Anything else falls back to `/sites`.
function isLocalPath(to: string): boolean {
  return to.startsWith("/") && !to.startsWith("//");
}

function LoginPage() {
  const navigate = useNavigate({ from: Route.fullPath });
  const search = Route.useSearch();
  return (
    <div className="flex min-h-dvh items-center justify-center px-5">
      <LoginForm
        className="w-full max-w-sm"
        onSuccess={() => {
          void navigate({ to: search.redirect ?? "/sites" });
        }}
      />
    </div>
  );
}
