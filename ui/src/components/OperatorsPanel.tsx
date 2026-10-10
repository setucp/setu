import * as React from "react";
import { KeyRound, Trash2, UserPlus } from "lucide-react";
import { toast } from "sonner";
import {
  useCreateOperator,
  useDeleteOperator,
  useMe,
  useOperators,
  useSetOperatorPassword,
} from "@/lib/api";
import { ApiError, type AuditEntry, type Operator } from "@/lib/client";
import { ErrorText } from "./ErrorText";
import { RelativeTime } from "./RelativeTime";
import { ResponsiveTable, type Column } from "./ResponsiveTable";
import { Alert } from "./ui/alert";
import { Avatar } from "./ui/avatar";
import { Badge } from "./ui/badge";
import { Button } from "./ui/button";
import { Modal, useConfirm } from "./ui/dialog";
import { Field } from "./ui/field";
import { Input } from "./ui/input";
import { Select } from "./ui/select";
import { SkeletonRows } from "./ui/skeleton";

const ROLES = ["owner", "operator", "viewer"] as const;

const roleVariant: Record<string, "info" | "secondary" | "outline"> = {
  owner: "info",
  operator: "secondary",
  viewer: "outline",
};

/**
 * Operators: list, create, a password change per row, and (owner only)
 * delete for anyone but yourself and the last owner. Your own password
 * needs the current one; the owner sets anyone else's (R102). Every
 * session of the target ends on change or delete.
 */
export function OperatorsPanel() {
  const operators = useOperators();
  const me = useMe();
  const [open, setOpen] = React.useState(false);
  const [passwordFor, setPasswordFor] = React.useState<Operator | null>(null);
  const del = useDeleteOperator();
  const confirm = useConfirm();

  if (operators.isPending) return <SkeletonRows rows={2} label="Loading operators…" />;
  if (operators.error instanceof ApiError) return <ErrorText error={operators.error} />;
  const rows: Operator[] = operators.data ?? [];

  function remove(o: Operator) {
    void (async () => {
      const ok = await confirm({
        title: `Delete ${o.email}?`,
        description: `${o.email} is signed out everywhere and can no longer sign in. Their audit entries stay.`,
        confirmLabel: `Delete ${o.email}`,
        destructive: true,
      });
      if (!ok) return;
      del.mutate(o.id, {
        onSuccess: () => toast.success(`Deleted ${o.email}`),
        onError: (err) => toast.error(err instanceof ApiError ? err.message : "Delete failed"),
      });
    })();
  }

  const action = (o: Operator) => (
    <span className="flex justify-end gap-1">
      {me.data?.id === o.id || me.data?.role === "owner" ? (
        <Button type="button" variant="ghost" size="sm" onClick={() => setPasswordFor(o)}>
          <KeyRound aria-hidden="true" />
          Change password
        </Button>
      ) : null}
      {me.data?.role === "owner" && me.data.id !== o.id ? (
        <Button type="button" variant="ghost-destructive" size="sm" onClick={() => remove(o)}>
          <Trash2 aria-hidden="true" />
          Delete
        </Button>
      ) : null}
    </span>
  );

  const columns: Column<Operator>[] = [
    {
      id: "email",
      header: "Email",
      cell: (o) => (
        <span className="flex min-w-0 items-center gap-3">
          <Avatar name={o.email} className="size-7 text-[11px]" />
          <span className="truncate font-medium">{o.email}</span>
          {me.data?.id === o.id ? <Badge variant="outline">you</Badge> : null}
        </span>
      ),
    },
    {
      id: "role",
      header: "Role",
      cell: (o) => <Badge variant={roleVariant[o.role] ?? "secondary"}>{o.role}</Badge>,
    },
    {
      id: "created",
      header: "Added",
      cell: (o) => <RelativeTime iso={o.created_at} className="text-muted-foreground" />,
    },
    {
      id: "actions",
      header: <span className="sr-only">Actions</span>,
      className: "text-right",
      hideOnCard: true,
      cell: action,
    },
  ];

  return (
    <div className="flex flex-col gap-4">
      <div className="flex flex-col gap-3 sm:flex-row sm:items-center">
        <p className="flex-1 text-sm text-muted-foreground">
          {rows.length} {rows.length === 1 ? "operator" : "operators"} can sign in to this panel.
        </p>
        <Button type="button" variant="outline" onClick={() => setOpen(true)}>
          <UserPlus aria-hidden="true" />
          Add operator
        </Button>
      </div>
      <ResponsiveTable
        label="Operators"
        rows={rows}
        columns={columns}
        getKey={(o) => o.id}
        cardTitle={(o) => columns[0]!.cell(o)}
        cardActions={action}
      />
      <CreateOperatorDialog open={open} onClose={() => setOpen(false)} />
      <PasswordDialog
        operator={passwordFor}
        self={passwordFor !== null && me.data?.id === passwordFor.id}
        onClose={() => setPasswordFor(null)}
      />
    </div>
  );
}

export function PasswordDialog({
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
        : "Password change failed"
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
      footer={
        <>
          <Button variant="outline" onClick={onClose}>
            Cancel
          </Button>
          <Button type="submit" form="password-form" loading={change.isPending}>
            {change.isPending ? "Saving…" : "Set password"}
          </Button>
        </>
      }
    >
      <form
        id="password-form"
        aria-label="Change password"
        className="flex flex-col gap-4"
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
          <Field label="Current password">
            <Input
              type="password"
              autoComplete="current-password"
              value={current}
              onChange={(e) => setCurrent(e.target.value)}
              required
            />
          </Field>
        ) : null}
        <Field label="New password" hint="At least 8 characters.">
          <Input
            type="password"
            autoComplete="new-password"
            minLength={8}
            value={password}
            onChange={(e) => setPassword(e.target.value)}
            required
          />
        </Field>
        <Field label="Confirm new password">
          <Input
            type="password"
            autoComplete="new-password"
            minLength={8}
            value={again}
            onChange={(e) => setAgain(e.target.value)}
            required
          />
        </Field>
        {error !== null ? <Alert variant="destructive">{error}</Alert> : null}
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
    <Modal
      open={open}
      onClose={onClose}
      title="Add operator"
      description="Operators sign in with this email and password. Owners can do everything; viewers can only look."
      footer={
        <>
          <Button variant="outline" onClick={onClose}>
            Cancel
          </Button>
          <Button type="submit" form="operator-form" loading={create.isPending}>
            {create.isPending ? "Creating…" : "Create operator"}
          </Button>
        </>
      }
    >
      <form
        id="operator-form"
        aria-label="Add operator"
        className="flex flex-col gap-4"
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
                toast.error(err instanceof ApiError ? err.message : "Create failed");
              },
            },
          );
        }}
      >
        <Field label="Email">
          <Input type="email" value={email} onChange={(e) => setEmail(e.target.value)} required />
        </Field>
        <Field label="Password" hint="At least 8 characters.">
          <Input
            type="password"
            autoComplete="new-password"
            value={password}
            onChange={(e) => setPassword(e.target.value)}
            required
            minLength={8}
          />
        </Field>
        <Field label="Role">
          <Select value={role} onChange={(e) => setRole(e.target.value)}>
            {ROLES.map((r) => (
              <option key={r} value={r}>
                {r}
              </option>
            ))}
          </Select>
        </Field>
        <ErrorText error={create.isError ? create.error : null} />
      </form>
    </Modal>
  );
}

/** Recent audit rows, newest first from the API. */
export function AuditTable({
  entries,
  operators = [],
}: {
  entries: AuditEntry[];
  operators?: Operator[];
}) {
  if (entries.length === 0) {
    return <p className="text-sm text-muted-foreground">No audit entries yet.</p>;
  }
  const emailById = new Map(operators.map((o) => [o.id, o.email]));
  const columns: Column<AuditEntry>[] = [
    {
      id: "when",
      header: "When",
      cell: (e) => <RelativeTime iso={e.at} className="whitespace-nowrap text-muted-foreground" />,
    },
    { id: "action", header: "Action", cell: (e) => <code className="text-xs font-medium">{e.action}</code> },
    {
      id: "target",
      header: "Target",
      className: "max-w-56 truncate font-mono text-xs",
      cell: (e) => e.target ?? "—",
    },
    {
      id: "operator",
      header: "Operator",
      className: "max-w-48 truncate text-xs",
      cell: (e) =>
        e.operator_id === null || e.operator_id === undefined
          ? "—"
          : (emailById.get(e.operator_id) ?? <span className="font-mono">{e.operator_id}</span>),
    },
    { id: "ip", header: "IP", cell: (e) => <span className="font-mono text-xs">{e.ip ?? "—"}</span> },
  ];
  return (
    <ResponsiveTable
      label="Audit log"
      rows={entries}
      columns={columns}
      getKey={(e) => String(e.id)}
      density="compact"
    />
  );
}
