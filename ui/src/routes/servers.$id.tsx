import { createFileRoute } from "@tanstack/react-router";
import { ServerDetail } from "@/components/ServerDetail";
import { useServer, useSites } from "@/lib/api";
import { ApiError } from "@/lib/client";

export const Route = createFileRoute("/servers/$id")({
  component: ServerPage,
});

function ServerPage() {
  const { id } = Route.useParams();
  const server = useServer(id);
  const sites = useSites();
  const error = server.error ?? sites.error;

  if (server.isPending || sites.isPending) {
    return <p className="text-sm text-muted-foreground">Loading…</p>;
  }
  if (error instanceof ApiError) {
    return (
      <p role="alert" className="text-sm text-destructive">
        {error.message}
      </p>
    );
  }
  if (server.data === undefined || sites.data === undefined) {
    return <p className="text-sm text-muted-foreground">Loading…</p>;
  }
  return <ServerDetail server={server.data} sites={sites.data} />;
}
