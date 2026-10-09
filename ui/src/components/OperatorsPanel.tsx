import * as React from "react";
import { toast } from "sonner";
import { useCreateOperator, useOperators } from "@/lib/api";
import { ApiError, type AuditEntry, type Operator } from "@/lib/client";
import { Button } from "./ui/button";
import { Modal } from "./ui/dialog";
import { Input } from "./ui/input";
import { Select } from "./ui/select";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "./ui/table";

const ROLES = ["owner", "operator", "viewer"] as const;

/**
 * Operators list with create. The API offers list + create only (no update or
 * delete endpoints), so the panel matches exactly that.
 */
export function OperatorsPanel() {
  const operators = useOperators();
  const [open, setOpen] = React.useState(false);

  if (operators.isPending) {
    return <p className="text-sm text-muted-foreground">Loading…</p>;
  }
  if (operators.error instanceof ApiError) {
    return (
      <p role="alert" className="text-sm text-destructive">
        {operators.error.message}
      </p>
    );
  }
  const rows: Operator[] = operators.data ?? [];

  return (
    <div className="flex flex-col gap-3">
      <div className="flex items-center">
        <h2 className="text-sm font-semibold">Operators ({rows.length})</h2>
        <Button
          type="button"
          variant="outline"
          size="sm"
          className="ml-auto"
          onClick={() => setOpen(true)}
        >
          Add operator
        </Button>
      </div>
      <Table>
        <TableHeader>
          <TableRow>
            <TableHead>Email</TableHead>
            <TableHead>Role</TableHead>
            <TableHead>Created</TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          {rows.map((o) => (
            <TableRow key={o.id}>
              <TableCell className="font-medium">{o.email}</TableCell>
              <TableCell>{o.role}</TableCell>
              <TableCell>{new Date(o.created_at).toLocaleDateString()}</TableCell>
            </TableRow>
          ))}
        </TableBody>
      </Table>
      <CreateOperatorDialog open={open} onClose={() => setOpen(false)} />
    </div>
  );
}

function CreateOperatorDialog({ open, onClose }: { open: boolean; onClose: () => void }) {
  const [email, setEmail] = React.useState("");
  const [password, setPassword] = React.useState("");
  const [role, setRole] = React.useState<string>("operator");
  const create = useCreateOperator();

  React.useEffect(() => {
    if (open) {
      setEmail("");
      setPassword("");
      setRole("operator");
      create.reset();
    }
  }, [open ]);

  return (
    <Modal open={open} onClose={onClose} title="Add operator">
      <form
        aria-label="Add operator"
        className="flex flex-col gap-3"
        onSubmit={(e) => {
          e.preventDefault();
          create.mutate(
            { email: email.trim(), password, role },
            {
              onSuccess: () => {
                toast.success("Operator created");
                onClose();
              },
              onError: (err) => {
                toast.error(err instanceof ApiError ? err.message : "create failed");
              },
            },
          );
        }}
      >
        <label className="flex flex-col gap-1.5 text-sm font-medium">
          Email
          <Input
            type="email"
            value={email}
            onChange={(e) => setEmail(e.target.value)}
            required
          />
        </label>
        <label className="flex flex-col gap-1.5 text-sm font-medium">
          Password
          <Input
            type="password"
            autoComplete="new-password"
            value={password}
            onChange={(e) => setPassword(e.target.value)}
            required
            minLength={8}
          />
        </label>
        <label className="flex flex-col gap-1.5 text-sm font-medium">
          Role
          <Select
            aria-label="Role"
            value={role}
            onChange={(e) => setRole(e.target.value)}
          >
            {ROLES.map((r) => (
              <option key={r} value={r}>
                {r}
              </option>
            ))}
          </Select>
        </label>
        {create.isError && create.error instanceof ApiError ? (
          <p role="alert" className="text-sm text-destructive">
            {create.error.message}
          </p>
        ) : null}
        <Button type="submit" disabled={create.isPending} className="self-start">
          {create.isPending ? "Creating…" : "Create operator"}
        </Button>
      </form>
    </Modal>
  );
}

/** Recent audit rows, newest first from the API. */
export function AuditTable({ entries }: { entries: AuditEntry[] }) {
  if (entries.length === 0) {
    return <p className="text-sm text-muted-foreground">No audit entries yet.</p>;
  }
  return (
    <Table>
      <TableHeader>
        <TableRow>
          <TableHead>When</TableHead>
          <TableHead>Action</TableHead>
          <TableHead>Target</TableHead>
          <TableHead>Operator</TableHead>
          <TableHead>IP</TableHead>
        </TableRow>
      </TableHeader>
      <TableBody>
        {entries.map((e) => (
          <TableRow key={e.id}>
            <TableCell>{new Date(e.at).toLocaleString()}</TableCell>
            <TableCell className="font-mono">{e.action}</TableCell>
            <TableCell className="max-w-56 truncate font-mono text-xs">
              {e.target ?? "—"}
            </TableCell>
            <TableCell className="max-w-40 truncate font-mono text-xs">
              {e.operator_id ?? "—"}
            </TableCell>
            <TableCell className="font-mono text-xs">{e.ip ?? "—"}</TableCell>
          </TableRow>
        ))}
      </TableBody>
    </Table>
  );
}
