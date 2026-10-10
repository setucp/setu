import { ListChecks } from "lucide-react";
import type { Job } from "@/lib/client";
import { duration } from "@/lib/time";
import { AppLink } from "./AppLink";
import { EmptyState } from "./EmptyState";
import { SectionHeader } from "./PageHeader";
import { RelativeTime } from "./RelativeTime";
import { ResponsiveTable, type Column } from "./ResponsiveTable";
import { StatusBadge, StatusDot, jobVariant } from "./StatusBadge";

function JobTitle({ job }: { job: Job }) {
  return (
    <div className="flex min-w-0 items-start gap-3">
      <StatusDot status={job.status} variants={jobVariant} className="mt-1.5" />
      <div className="min-w-0">
        <span className="block font-mono text-xs font-medium">{job.kind}</span>
        <AppLink href={`/jobs/${job.id}`} className="block truncate font-mono text-xs text-link hover:underline">
          {job.id}
        </AppLink>
      </div>
    </div>
  );
}

const columns: Column<Job>[] = [
  { id: "job", header: "Job", cell: (job) => <JobTitle job={job} /> },
  {
    id: "site",
    header: "Site",
    cell: (job) =>
      job.site_id === null || job.site_id === undefined ? (
        <span className="text-muted-foreground">—</span>
      ) : (
        <AppLink href={`/sites/${job.site_id}`} className="font-mono text-xs text-link hover:underline">
          {job.site_id}
        </AppLink>
      ),
  },
  {
    id: "status",
    header: "Status",
    cell: (job) => <StatusBadge status={job.status} variants={jobVariant} />,
  },
  {
    id: "duration",
    header: "Duration",
    cell: (job) => (
      <span className="whitespace-nowrap tabular-nums text-muted-foreground">
        {duration(job.started_at, job.finished_at)}
      </span>
    ),
  },
  {
    id: "created",
    header: "Created",
    cell: (job) => <RelativeTime iso={job.created_at} className="whitespace-nowrap text-muted-foreground" />,
  },
  {
    id: "error",
    header: "Error",
    className: "max-w-64 truncate text-xs text-destructive",
    cell: (job) =>
      job.error === null || job.error === undefined ? (
        <span className="text-muted-foreground">—</span>
      ) : (
        <span title={job.error}>{job.error}</span>
      ),
  },
];

/** Running jobs first, then recent history — every row links to its job. */
export function JobsTable({ jobs }: { jobs: Job[] }) {
  const open = jobs.filter((j) => j.status === "queued" || j.status === "running");
  const recent = jobs.filter((j) => j.status !== "queued" && j.status !== "running");
  if (jobs.length === 0) {
    return (
      <EmptyState
        icon={<ListChecks className="size-6 text-link" aria-hidden="true" />}
        title="No jobs yet."
        description="Provisioning, updates, backups and restarts each run as a job you can follow here."
      />
    );
  }
  return (
    <div className="flex flex-col gap-8">
      {open.length > 0 ? (
        <section className="flex flex-col gap-3">
          <SectionHeader title={`Running (${open.length})`} />
          <ResponsiveTable
            label="Running jobs"
            rows={open}
            columns={columns}
            getKey={(j) => j.id}
            cardTitle={(job) => <JobTitle job={job} />}
          />
        </section>
      ) : null}
      <section className="flex flex-col gap-3">
        <SectionHeader title="Recent" />
        {recent.length === 0 ? (
          <p className="text-sm text-muted-foreground">Nothing finished yet.</p>
        ) : (
          <ResponsiveTable
            label="Recent jobs"
            rows={recent}
            columns={columns}
            getKey={(j) => j.id}
            density="compact"
            cardTitle={(job) => <JobTitle job={job} />}
          />
        )}
      </section>
    </div>
  );
}
