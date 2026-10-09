import type { Job } from "@/lib/client";
import { Badge } from "./ui/badge";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "./ui/table";

const variantByStatus: Record<string, "success" | "warning" | "destructive" | "secondary"> = {
  done: "success",
  queued: "warning",
  running: "warning",
  failed: "destructive",
};

function JobRow({ job }: { job: Job }) {
  return (
    <TableRow>
      <TableCell className="font-mono">
        <a
          href={`/jobs/${job.id}`}
          className="text-primary underline-offset-2 hover:underline"
        >
          {job.id}
        </a>
      </TableCell>
      <TableCell>{job.kind}</TableCell>
      <TableCell>
        {job.site_id === null || job.site_id === undefined ? (
          "—"
        ) : (
          <a
            href={`/sites/${job.site_id}`}
            className="font-mono text-primary underline-offset-2 hover:underline"
          >
            {job.site_id}
          </a>
        )}
      </TableCell>
      <TableCell>
        <Badge variant={variantByStatus[job.status] ?? "secondary"}>{job.status}</Badge>
      </TableCell>
      <TableCell className="max-w-64 truncate text-xs text-muted-foreground">
        {job.error ?? "—"}
      </TableCell>
    </TableRow>
  );
}

/** Running jobs first, then recent history — every row links to its job. */
export function JobsTable({ jobs }: { jobs: Job[] }) {
  const open = jobs.filter((j) => j.status === "queued" || j.status === "running");
  const recent = jobs.filter((j) => j.status !== "queued" && j.status !== "running");
  if (jobs.length === 0) {
    return <p className="py-10 text-center text-sm text-muted-foreground">No jobs yet.</p>;
  }
  return (
    <div className="flex flex-col gap-6">
      {open.length > 0 ? (
        <section className="flex flex-col gap-2">
          <h2 className="text-sm font-semibold">Running ({open.length})</h2>
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>Job</TableHead>
                <TableHead>Kind</TableHead>
                <TableHead>Site</TableHead>
                <TableHead>Status</TableHead>
                <TableHead>Error</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {open.map((j) => (
                <JobRow key={j.id} job={j} />
              ))}
            </TableBody>
          </Table>
        </section>
      ) : null}
      <section className="flex flex-col gap-2">
        <h2 className="text-sm font-semibold">Recent</h2>
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead>Job</TableHead>
              <TableHead>Kind</TableHead>
              <TableHead>Site</TableHead>
              <TableHead>Status</TableHead>
              <TableHead>Error</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {recent.map((j) => (
              <JobRow key={j.id} job={j} />
            ))}
          </TableBody>
        </Table>
      </section>
    </div>
  );
}
