import * as React from "react";
import { cn } from "@/lib/utils";

interface TabsContextValue {
  value: string;
  setValue: (value: string) => void;
}

const TabsContext = React.createContext<TabsContextValue | null>(null);

function useTabsContext(): TabsContextValue {
  const ctx = React.useContext(TabsContext);
  if (ctx === null) throw new Error("Tabs components must be used inside <Tabs>");
  return ctx;
}

/** Underline-style tab strip classes, shared with route-driven tab links. */
export const tabListClass =
  "scrollbar-thin -mb-px flex items-center gap-1 overflow-x-auto border-b";
export const tabTriggerClass =
  "touch-target relative inline-flex h-10 shrink-0 items-center justify-center gap-1.5 whitespace-nowrap px-3 text-sm font-medium text-muted-foreground transition-colors hover:text-foreground after:absolute after:inset-x-2 after:-bottom-px after:h-0.5 after:rounded-full after:bg-transparent disabled:pointer-events-none disabled:opacity-50";
export const tabTriggerActiveClass = "text-foreground after:bg-primary";

/** Minimal controlled tabs (button-based triggers, no extra deps). */
export function Tabs({
  value,
  onValueChange,
  children,
  className,
}: {
  value: string;
  onValueChange: (value: string) => void;
  children: React.ReactNode;
  className?: string;
}) {
  const ctx = React.useMemo(() => ({ value, setValue: onValueChange }), [value, onValueChange]);
  return (
    <TabsContext.Provider value={ctx}>
      <div className={className}>{children}</div>
    </TabsContext.Provider>
  );
}

export function TabsList({ children, className }: { children: React.ReactNode; className?: string }) {
  const ref = React.useRef<HTMLDivElement>(null);
  // Arrow keys move between tabs (WAI-ARIA tabs pattern).
  function onKeyDown(e: React.KeyboardEvent) {
    if (e.key !== "ArrowRight" && e.key !== "ArrowLeft") return;
    const tabs = Array.from(
      ref.current?.querySelectorAll<HTMLButtonElement>('[role="tab"]:not([disabled])') ?? [],
    );
    const index = tabs.findIndex((t) => t === document.activeElement);
    if (index < 0) return;
    const next = tabs[(index + (e.key === "ArrowRight" ? 1 : tabs.length - 1)) % tabs.length];
    next?.focus();
    next?.click();
  }
  return (
    <div ref={ref} role="tablist" onKeyDown={onKeyDown} className={cn(tabListClass, className)}>
      {children}
    </div>
  );
}

export function TabsTrigger({
  value,
  disabled,
  title,
  children,
  className,
}: {
  value: string;
  disabled?: boolean;
  title?: string;
  children: React.ReactNode;
  className?: string;
}) {
  const { value: active, setValue } = useTabsContext();
  const selected = active === value;
  return (
    <button
      type="button"
      role="tab"
      aria-selected={selected}
      tabIndex={selected ? 0 : -1}
      disabled={disabled}
      title={title}
      onClick={() => setValue(value)}
      className={cn(tabTriggerClass, selected && tabTriggerActiveClass, className)}
    >
      {children}
    </button>
  );
}

export function TabsContent({
  value,
  children,
  className,
}: {
  value: string;
  children: React.ReactNode;
  className?: string;
}) {
  const { value: active } = useTabsContext();
  if (active !== value) return null;
  return (
    <div role="tabpanel" className={cn("mt-4", className)}>
      {children}
    </div>
  );
}
