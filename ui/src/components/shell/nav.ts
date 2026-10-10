import { Globe, Layers, ListChecks, Package, Server, Settings2, type LucideIcon } from "lucide-react";

export interface NavItem {
  to: "/sites" | "/servers" | "/apps" | "/plans" | "/jobs" | "/settings";
  label: string;
  icon: LucideIcon;
}

export const NAV: NavItem[] = [
  { to: "/sites", label: "Sites", icon: Globe },
  { to: "/servers", label: "Servers", icon: Server },
  { to: "/apps", label: "Apps", icon: Package },
  { to: "/plans", label: "Plans", icon: Layers },
  { to: "/jobs", label: "Jobs", icon: ListChecks },
  { to: "/settings", label: "Settings", icon: Settings2 },
];
