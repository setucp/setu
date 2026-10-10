import { useNavigate } from "@tanstack/react-router";
import { ChevronsUpDown, KeyRound, LogOut, Palette } from "lucide-react";
import { useLogout } from "@/lib/api";
import type { Operator } from "@/lib/client";
import { cn } from "@/lib/utils";
import { Avatar } from "@/components/ui/avatar";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuSub,
  DropdownMenuSubContent,
  DropdownMenuSubTrigger,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { ThemeRadioItems } from "./ThemeToggle";

/**
 * The signed-in operator and their menu: theme, change password (in
 * Settings) and log out. `variant="sidebar"` is the full-width block at
 * the bottom of the sidebar; `variant="avatar"` is the compact top-bar button.
 */
export function OperatorMenu({
  operator,
  variant = "sidebar",
  className,
}: {
  operator: Pick<Operator, "email" | "role">;
  variant?: "sidebar" | "avatar";
  className?: string;
}) {
  const logout = useLogout();
  const navigate = useNavigate();
  return (
    <DropdownMenu>
      <DropdownMenuTrigger
        aria-label={`Account menu for ${operator.email}`}
        className={cn(
          "touch-target flex items-center gap-3 rounded-md text-left outline-hidden transition-colors hover:bg-accent focus-visible:ring-2 focus-visible:ring-ring data-[state=open]:bg-accent",
          variant === "sidebar"
            ? "w-full p-2 md:max-lg:justify-center md:max-lg:p-1"
            : "p-0.5",
          className,
        )}
      >
        <Avatar name={operator.email} />
        {variant === "sidebar" ? (
          <>
            <span className="min-w-0 flex-1 md:max-lg:hidden">
              <span className="block truncate text-sm font-medium">{operator.email}</span>
              <span className="block truncate text-xs capitalize text-muted-foreground">
                {operator.role}
              </span>
            </span>
            <ChevronsUpDown
              className="size-4 shrink-0 text-muted-foreground md:max-lg:hidden"
              aria-hidden="true"
            />
          </>
        ) : null}
      </DropdownMenuTrigger>
      <DropdownMenuContent
        align={variant === "sidebar" ? "start" : "end"}
        side={variant === "sidebar" ? "top" : "bottom"}
        className="w-60"
      >
        <DropdownMenuLabel className="font-normal">
          <span className="block truncate text-sm font-medium text-foreground">
            {operator.email}
          </span>
          <span className="block text-xs capitalize">{operator.role}</span>
        </DropdownMenuLabel>
        <DropdownMenuSeparator />
        <DropdownMenuSub>
          <DropdownMenuSubTrigger>
            <Palette aria-hidden="true" />
            Theme
          </DropdownMenuSubTrigger>
          <DropdownMenuSubContent>
            <ThemeRadioItems />
          </DropdownMenuSubContent>
        </DropdownMenuSub>
        <DropdownMenuItem onSelect={() => void navigate({ to: "/settings", hash: "profile" })}>
          <KeyRound aria-hidden="true" />
          Change password
        </DropdownMenuItem>
        <DropdownMenuSeparator />
        <DropdownMenuItem onSelect={() => logout.mutate()}>
          <LogOut aria-hidden="true" />
          Log out
        </DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
