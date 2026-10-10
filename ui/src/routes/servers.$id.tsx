import { createFileRoute } from "@tanstack/react-router";
import { ErrorText } from "@/components/ErrorText";
import { ServerDetail } from "@/components/ServerDetail";
import { Skeleton } from "@/components/ui/skeleton";
import { useServer, useSites } from "@/lib/api";

export const Route = createFileRoute("/servers/$id")({
  component: ServerPage,
});

function ServerPage() {
  const { id } = Route.useParams();
  const server = useServer(id);
  const sites = useSites();
  const error = server.error ?? sites.error;

  if (error !== null) return <ErrorText error={error} />;
  if (server.data === undefined || sites.data === undefined) {
    return (
      <div role="status" className="flex flex-col gap-6">
        <span className="sr-only">Loading server…</span>
        <Skeleton className="h-8 w-48" />
        <div className="grid grid-cols-2 gap-3 lg:grid-cols-4">
          {Array.from({ length: 4 }, (_, i) => (
            <Skeleton key={i} className="h-20" />
          ))}
        </div>
      </div>
    );
  }
  return <ServerDetail server={server.data} sites={sites.data} />;
}
