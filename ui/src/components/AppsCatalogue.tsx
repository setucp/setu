
import { useApps, useAppVersions } from "@/lib/api";
import { ApiError } from "@/lib/client";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "./ui/table";

function AppRow({ appId, name, homepage }: { appId: string; name: string; homepage?: string | null }) {
  const versions = useAppVersions(appId);
  return (
    <TableRow>
      <TableCell className="font-medium">{name}</TableCell>
      <TableCell className="font-mono">{appId}</TableCell>
      <TableCell>
        {homepage ? (
          <a
            href={homepage}
            target="_blank"
            rel="noreferrer"
            className="text-primary underline-offset-2 hover:underline"
          >
            {homepage}
          </a>
        ) : (
          "—"
        )}
      </TableCell>
      <TableCell>
        {versions.isPending
          ? "…"
          : versions.data?.map((v) => v.version).join(", ") || "—"}
      </TableCell>
    </TableRow>
  );
}

/** App catalogue with per-app versions (newest first from the API). */
export function AppsCatalogue() {
  const apps = useApps();

  if (apps.isPending) {
    return <p className="text-sm text-muted-foreground">Loading…</p>;
  }
  if (apps.error instanceof ApiError) {
    return (
      <p role="alert" className="text-sm text-destructive">
        {apps.error.message}
      </p>
    );
  }
  const rows = apps.data ?? [];
  if (rows.length === 0) {
    return <p className="py-10 text-center text-sm text-muted-foreground">No apps yet.</p>;
  }
  return (
    <Table>
      <TableHeader>
        <TableRow>
          <TableHead>Name</TableHead>
          <TableHead>ID</TableHead>
          <TableHead>Homepage</TableHead>
          <TableHead>Versions</TableHead>
        </TableRow>
      </TableHeader>
      <TableBody>
        {rows.map((a) => (
          <AppRow key={a.id} appId={a.id} name={a.name} homepage={a.homepage} />
        ))}
      </TableBody>
    </Table>
  );
}

