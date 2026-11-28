import { createFileRoute } from "@tanstack/react-router";
import { SitesListPage } from "@/components/SitesListPage";

export const Route = createFileRoute("/sites/")({
  component: SitesListPage,
});
