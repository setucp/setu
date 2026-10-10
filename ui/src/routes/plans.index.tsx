import * as React from "react";
import { createFileRoute } from "@tanstack/react-router";
import { Plus } from "lucide-react";
import { ErrorText } from "@/components/ErrorText";
import { PageHeader } from "@/components/PageHeader";
import { PlanDialog, PlansTable } from "@/components/PlansTable";
import { Button } from "@/components/ui/button";
import { SkeletonRows } from "@/components/ui/skeleton";
import { usePlans } from "@/lib/api";
import type { Plan } from "@/lib/client";

export const Route = createFileRoute("/plans/")({
  component: PlansPage,
});

function PlansPage() {
  const plans = usePlans();
  const [dialog, setDialog] = React.useState<{ open: boolean; plan: Plan | null }>({
    open: false,
    plan: null,
  });
  const create = () => setDialog({ open: true, plan: null });

  return (
    <div className="flex flex-col gap-6">
      <PageHeader
        title="Plans"
        description="Resource limits and app allowances that sites are created on."
        actions={
          <Button type="button" onClick={create}>
            <Plus aria-hidden="true" />
            New plan
          </Button>
        }
      />
      {plans.isPending ? <SkeletonRows rows={3} label="Loading plans…" /> : null}
      <ErrorText error={plans.error} />
      {plans.data !== undefined ? (
        <PlansTable
          plans={plans.data}
          onEdit={(plan) => setDialog({ open: true, plan })}
          onCreate={create}
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
