import { Link } from "@tanstack/react-router";
import { cn } from "@/lib/utils";
import { Tooltip } from "@/components/ui/tooltip";
import { NAV } from "./nav";

const linkBase =
  "touch-target group relative flex h-9 items-center gap-3 rounded-md px-3 text-sm font-medium text-muted-foreground outline-hidden transition-colors hover:bg-accent hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring";
const linkActive =
  "bg-accent text-foreground before:absolute before:inset-y-2 before:-left-3 before:w-[3px] before:rounded-r-full before:bg-primary [&_svg]:text-link";

/**
 * Primary navigation. `collapsible` (the sidebar) hides labels between
 * 768 and 1023 px and shows them as tooltips; the drawer always shows them.
 */
export function NavList({
  collapsible = false,
  onNavigate,
}: {
  collapsible?: boolean;
  onNavigate?: () => void;
}) {
  return (
    <ul className="flex flex-col gap-0.5">
      {NAV.map((item) => {
        const Icon = item.icon;
        const link = (
          <Link
            to={item.to}
            onClick={onNavigate}
            className={cn(linkBase, collapsible && "md:max-lg:justify-center md:max-lg:px-0")}
            activeProps={{ className: linkActive }}
          >
            <Icon className="size-4 shrink-0" aria-hidden="true" />
            <span className={cn(collapsible && "md:max-lg:sr-only")}>{item.label}</span>
          </Link>
        );
        return (
          <li key={item.to}>
            {collapsible ? (
              <Tooltip content={item.label} className="hidden md:max-lg:block">
                {link}
              </Tooltip>
            ) : (
              link
            )}
          </li>
        );
      })}
    </ul>
  );
}
