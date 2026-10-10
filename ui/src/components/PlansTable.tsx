import * as React from "react";
import { Layers, Pencil } from "lucide-react";
import { toast } from "sonner";
import { useCreatePlan, useUpdatePlan } from "@/lib/api";
import type { Plan } from "@/lib/client";
import { formatMb } from "@/lib/format";
import { EmptyState } from "./EmptyState";
import { ErrorText } from "./ErrorText";
import { ResponsiveTable, type Column } from "./ResponsiveTable";
import { Badge } from "./ui/badge";
import { Button } from "./ui/button";
import { Modal } from "./ui/dialog";
import { Field } from "./ui/field";
import { Input } from "./ui/input";

/** Parse a comma-separated app-id list, ignoring blanks. */
export function parseAppIds(text: string): string[] {
  return text
    .split(",")
    .map((s) => s.trim())
    .filter((s) => s !== "");
}

export function PlansTable({
  plans,
  onEdit,
  onCreate,
}: {
  plans: Plan[];
  onEdit: (plan: Plan) => void;
  onCreate?: () => void;
}) {
  if (plans.length === 0) {
    return (
      <EmptyState
        icon={<Layers className="size-6 text-link" aria-hidden="true" />}
        title="No plans yet."
        description="A plan caps a site's memory, CPU and disk, and can limit which apps it may run."
        action={onCreate !== undefined ? <Button onClick={onCreate}>New plan</Button> : undefined}
      />
    );
  }
  const edit = (p: Plan) => (
    <Button type="button" variant="outline" size="sm" onClick={() => onEdit(p)}>
      <Pencil aria-hidden="true" />
      Edit
    </Button>
  );
  const columns: Column<Plan>[] = [
    { id: "name", header: "Name", cell: (p) => <span className="font-semibold">{p.name}</span> },
    {
      id: "memory",
      header: "Memory",
      cell: (p) => <span className="tabular-nums">{formatMb(p.limits.memory_mb)}</span>,
    },
    {
      id: "cpu",
      header: "CPU",
      cell: (p) => <span className="tabular-nums">{p.limits.cpu_percent ?? "—"}%</span>,
    },
    {
      id: "disk",
      header: "Disk",
      cell: (p) => <span className="tabular-nums">{formatMb(p.limits.disk_mb)}</span>,
    },
    {
      id: "apps",
      header: "Allowed apps",
      cell: (p) =>
        p.allowed_app_ids.length === 0 ? (
          <Badge variant="secondary">all apps</Badge>
        ) : (
          <span className="flex flex-wrap gap-1">
            {p.allowed_app_ids.map((id) => (
              <Badge key={id} variant="outline" className="font-mono">
                {id}
              </Badge>
            ))}
          </span>
        ),
    },
    {
      id: "actions",
      header: <span className="sr-only">Actions</span>,
      hideOnCard: true,
      className: "text-right",
      cell: edit,
    },
  ];
  return (
    <ResponsiveTable
      label="Plans"
      rows={plans}
      columns={columns}
      getKey={(p) => p.id}
      cardActions={edit}
    />
  );
}

function num(value: string, fallback: number): number {
  const n = Number.parseInt(value, 10);
  return Number.isNaN(n) ? fallback : n;
}

/** Create / edit dialog. Plans carry limits plus the allowed app ids. */
export function PlanDialog({
  plan,
  open,
  onClose,
}: {
  plan: Plan | null;
  open: boolean;
  onClose: () => void;
}) {
  const [name, setName] = React.useState(plan?.name ?? "");
  const [memory, setMemory] = React.useState(String(plan?.limits.memory_mb ?? 512));
  const [cpu, setCpu] = React.useState(String(plan?.limits.cpu_percent ?? 100));
  const [disk, setDisk] = React.useState(String(plan?.limits.disk_mb ?? 5120));
  const [appIds, setAppIds] = React.useState((plan?.allowed_app_ids ?? []).join(", "));
  const create = useCreatePlan();
  const update = useUpdatePlan();
  const pending = create.isPending || update.isPending;
  const error = create.error ?? update.error;
  const resetCreate = create.reset;
  const resetUpdate = update.reset;

  React.useEffect(() => {
    if (open) {
      setName(plan?.name ?? "");
      setMemory(String(plan?.limits.memory_mb ?? 512));
      setCpu(String(plan?.limits.cpu_percent ?? 100));
      setDisk(String(plan?.limits.disk_mb ?? 5120));
      setAppIds((plan?.allowed_app_ids ?? []).join(", "));
      resetCreate();
      resetUpdate();
    }
  }, [open, plan, resetCreate, resetUpdate]);

  return (
    <Modal
      open={open}
      onClose={onClose}
      title={plan === null ? "New plan" : `Edit ${plan.name}`}
      description="Limits apply to every site on the plan; changes take effect on each site's next restart."
      footer={
        <>
          <Button variant="outline" onClick={onClose}>
            Cancel
          </Button>
          <Button type="submit" form="plan-form" loading={pending}>
            {pending ? "Saving…" : plan === null ? "Create plan" : "Save plan"}
          </Button>
        </>
      }
    >
      <form
        id="plan-form"
        aria-label={plan === null ? "New plan" : "Edit plan"}
        className="flex flex-col gap-4"
        onSubmit={(e) => {
          e.preventDefault();
          const input = {
            name: name.trim(),
            limits: {
              memory_mb: num(memory, 0),
              cpu_percent: num(cpu, 0),
              disk_mb: num(disk, 0),
            },
            allowed_app_ids: parseAppIds(appIds),
          };
          if (plan === null) {
            create.mutate(input, {
              onSuccess: () => {
                toast.success("Plan created");
                onClose();
              },
            });
          } else {
            update.mutate(
              { id: plan.id, input },
              {
                onSuccess: () => {
                  toast.success("Plan updated");
                  onClose();
                },
              },
            );
          }
        }}
      >
        <Field label="Name">
          <Input value={name} onChange={(e) => setName(e.target.value)} placeholder="standard" required />
        </Field>
        <div className="grid grid-cols-1 gap-4 sm:grid-cols-3">
          <Field label="Memory (MB)">
            <Input inputMode="numeric" value={memory} onChange={(e) => setMemory(e.target.value)} required />
          </Field>
          <Field label="CPU (%)" hint="100 = one core">
            <Input inputMode="numeric" value={cpu} onChange={(e) => setCpu(e.target.value)} required />
          </Field>
          <Field label="Disk (MB)">
            <Input inputMode="numeric" value={disk} onChange={(e) => setDisk(e.target.value)} required />
          </Field>
        </div>
        <Field label="Allowed app ids" hint="Comma-separated; leave blank to allow every app.">
          <Input
            value={appIds}
            onChange={(e) => setAppIds(e.target.value)}
            placeholder="hello, vyasa"
            className="font-mono"
          />
        </Field>
        <ErrorText error={error} />
      </form>
    </Modal>
  );
}
