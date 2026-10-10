import * as React from "react";
import { toast } from "sonner";
import { useCreateOperator, useMe, useOperators, useSetOperatorPassword } from "@/lib/api";
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
 * Operators: list, create, and a password change per row. The API has no
 * operator update or delete endpoint, so the panel offers exactly these.
 * Your own password needs the current one; the owner sets anyone else's
 * (R102). Every session of the target ends on change.
 */
export function OperatorsPanel() {
  const operators = useOperators();
  const me = useMe();
  const [open, setOpen] = React.useState(false);
  const [passwordFor, setPasswordFor] = React.useState<Operator | null>(null);

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
            <TableHead>
              <span className="sr-only">Actions</span>
            </TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          {rows.map((o) => (
            <TableRow key={o.id}>
              <TableCell className="font-medium">{o.email}</TableCell>
              <TableCell>{o.role}</TableCell>
              <TableCell>{new Date(o.created_at).toLocaleDateString()}</TableCell>
              <TableCell className="text-right">
                {me.data?.id === o.id || me.data?.role === "owner" ? (
                  <Button
                    type="button"
                    variant="ghost"
                    size="sm"
                    onClick={() => setPasswordFor(o)}
                  >
                    Change password
                  </Button>
                ) : null}
              </TableCell>
            </TableRow>
          ))}
        </TableBody>
      </Table>
      <CreateOperatorDialog open={open} onClose={() => setOpen(false)} />
      <PasswordDialog
        operator={passwordFor}
        self={passwordFor !== null && me.data?.id === passwordFor.id}
        onClose={() => setPasswordFor(null)}
      />
    </div>
  );
}

function PasswordDialog({
  operator,
  self,
  onClose,
}: {
  operator: Operator | null;
  self: boolean;
  onClose: () => void;
}) {
  const [current, setCurrent] = React.useState("");
  const [password, setPassword] = React.useState("");
  const [again, setAgain] = React.useState("");
  const [localError, setLocalError] = React.useState<string | null>(null);
  const change = useSetOperatorPassword();
  const reset = change.reset;

  React.useEffect(() => {
    setCurrent("");
    setPassword("");
    setAgain("");
    setLocalError(null);
    reset();
  }, [operator, reset]);

  const error =
    localError ??
    (change.isError
      ? change.error instanceof ApiError
        ? change.error.message
        : "password change failed"
      : null);

  return (
    <Modal
      open={operator !== null}
      onClose={onClose}
      title={`Change password for ${operator?.email ?? ""}`}
      description={
        self
          ? "Your other sessions are signed out; this browser stays signed in."
          : "Every session of this operator is signed out."
      }
    >
      <form
        aria-label="Change password"
        className="flex flex-col gap-3"
        onSubmit={(e) => {
          e.preventDefault();
          if (operator === null) return;
          if (password !== again) {
            setLocalError("The new passwords do not match.");
            return;
          }
          setLocalError(null);
          change.mutate(
            { id: operator.id, password, currentPassword: self ? current : undefined },
            {
              onSuccess: () => {
                toast.success(`Password changed for ${operator.email}`);
                onClose();
              },
            },
          );
        }}
      >
        {self ? (
          <label className="flex flex-col gap-1.5 text-sm font-medium">
            Current password
            <Input
              type="password"
              autoComplete="current-password"
              value={current}
              onChange={(e) => setCurrent(e.target.value)}
              required
            />
          </label>
        ) : null}
        <label className="flex flex-col gap-1.5 text-sm font-medium">
          New password
          <Input
            type="password"
            autoComplete="new-password"
            minLength={8}
            value={password}
            onChange={(e) => setPassword(e.target.value)}
            required
          />
        </label>
        <label className="flex flex-col gap-1.5 text-sm font-medium">
          Confirm new password
          <Input
            type="password"
            autoComplete="new-password"
            minLength={8}
            value={again}
            onChange={(e) => setAgain(e.target.value)}
            required
          />
        </label>
        {error !== null ? (
          <p role="alert" className="text-sm text-destructive">
            {error}
          </p>
        ) : null}
        <Button type="submit" disabled={change.isPending} className="self-start">
          {change.isPending ? "Saving…" : "Set password"}
        </Button>
      </form>
    </Modal>
  );
}

function CreateOperatorDialog({ open, onClose }: { open: boolean; onClose: () => void }) {
  const [email, setEmail] = React.useState("");
  const [password, setPassword] = React.useState("");
  const [role, setRole] = React.useState<string>("operator");
  const create = useCreateOperator();
  const reset = create.reset;

  React.useEffect(() => {
    if (open) {
      setEmail("");
      setPassword("");
      setRole("operator");
      reset();
    }
  }, [open, reset]);

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
