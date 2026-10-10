import * as React from "react";
import { createFileRoute } from "@tanstack/react-router";
import { PlanDialog, PlansTable } from "@/components/PlansTable";
import { Button } from "@/components/ui/button";
import { usePlans } from "@/lib/api";
import { ApiError, type Plan } from "@/lib/client";

export const Route = createFileRoute("/plans/")({
  component: PlansPage,
});

function PlansPage() {
  const plans = usePlans();
  const [dialog, setDialog] = React.useState<{ open: boolean; plan: Plan | null }>({
    open: false,
    plan: null,
  });

  return (
    <div className="flex flex-col gap-4">
      <div className="flex items-center">
        <h1 className="text-xl font-semibold tracking-tight">Plans</h1>
        <Button
          type="button"
          className="ml-auto"
          onClick={() => setDialog({ open: true, plan: null })}
        >
          New plan
        </Button>
      </div>
      {plans.isPending ? (
        <p className="text-sm text-muted-foreground">Loading…</p>
      ) : null}
      {plans.error instanceof ApiError ? (
        <p role="alert" className="text-sm text-destructive">
          {plans.error.message}
        </p>
      ) : null}
      {plans.data !== undefined ? (
        <PlansTable
          plans={plans.data}
          onEdit={(plan) => setDialog({ open: true, plan })}
        />
      ) : null}
      <PlanDialog
        plan={dialog.plan}
        open={dialog.open}
        onClose={() => setDialog({ open: false, plan: null })}
      />
    </div>
  );
}
