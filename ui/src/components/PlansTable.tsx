import * as React from "react";
import { toast } from "sonner";
import { useCreatePlan, useUpdatePlan } from "@/lib/api";
import { ApiError, type Plan } from "@/lib/client";
import { Button } from "./ui/button";
import { Modal } from "./ui/dialog";
import { Input } from "./ui/input";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "./ui/table";

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
}: {
  plans: Plan[];
  onEdit: (plan: Plan) => void;
}) {
  if (plans.length === 0) {
    return <p className="py-10 text-center text-sm text-muted-foreground">No plans yet.</p>;
  }
  return (
    <Table>
      <TableHeader>
        <TableRow>
          <TableHead>Name</TableHead>
          <TableHead>Memory</TableHead>
          <TableHead>CPU</TableHead>
          <TableHead>Disk</TableHead>
          <TableHead>Allowed apps</TableHead>
          <TableHead>
            <span className="sr-only">Actions</span>
          </TableHead>
        </TableRow>
      </TableHeader>
      <TableBody>
        {plans.map((p) => (
          <TableRow key={p.id}>
            <TableCell className="font-medium">{p.name}</TableCell>
            <TableCell>{p.limits.memory_mb ?? "—"} MB</TableCell>
            <TableCell>{p.limits.cpu_percent ?? "—"}%</TableCell>
            <TableCell>{p.limits.disk_mb ?? "—"} MB</TableCell>
            <TableCell className="max-w-56 truncate font-mono text-xs">
              {p.allowed_app_ids.length === 0 ? "all" : p.allowed_app_ids.join(", ")}
            </TableCell>
            <TableCell>
              <Button type="button" variant="outline" size="sm" onClick={() => onEdit(p)}>
                Edit
              </Button>
            </TableCell>
          </TableRow>
        ))}
      </TableBody>
    </Table>
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

  React.useEffect(() => {
    if (open) {
      setName(plan?.name ?? "");
      setMemory(String(plan?.limits.memory_mb ?? 512));
      setCpu(String(plan?.limits.cpu_percent ?? 100));
      setDisk(String(plan?.limits.disk_mb ?? 5120));
      setAppIds((plan?.allowed_app_ids ?? []).join(", "));
      create.reset();
      update.reset();
    }
  }, [open ]);

  return (
    <Modal
      open={open}
      onClose={onClose}
      title={plan === null ? "New plan" : `Edit ${plan.name}`}
    >
      <form
        aria-label={plan === null ? "New plan" : "Edit plan"}
        className="flex flex-col gap-3"
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
        <label className="flex flex-col gap-1.5 text-sm font-medium">
          Name
          <Input value={name} onChange={(e) => setName(e.target.value)} required />
        </label>
        <div className="grid grid-cols-3 gap-3">
          <label className="flex flex-col gap-1.5 text-sm font-medium">
            Memory (MB)
            <Input
              inputMode="numeric"
              value={memory}
              onChange={(e) => setMemory(e.target.value)}
              required
            />
          </label>
          <label className="flex flex-col gap-1.5 text-sm font-medium">
            CPU (%)
            <Input
              inputMode="numeric"
              value={cpu}
              onChange={(e) => setCpu(e.target.value)}
              required
            />
          </label>
          <label className="flex flex-col gap-1.5 text-sm font-medium">
            Disk (MB)
            <Input
              inputMode="numeric"
              value={disk}
              onChange={(e) => setDisk(e.target.value)}
              required
            />
          </label>
        </div>
        <label className="flex flex-col gap-1.5 text-sm font-medium">
          Allowed app ids (comma-separated, blank = all)
          <Input
            value={appIds}
            onChange={(e) => setAppIds(e.target.value)}
            placeholder="hello, vyasa"
          />
        </label>
        {error instanceof ApiError ? (
          <p role="alert" className="text-sm text-destructive">
            {error.message}
          </p>
        ) : null}
        <Button type="submit" disabled={pending} className="self-start">
          {pending ? "Saving…" : plan === null ? "Create plan" : "Save plan"}
        </Button>
      </form>
    </Modal>
  );
}
