import * as React from "react";
import { useRouter } from "@tanstack/react-router";

/**
 * In-app link that works with or without a router: a real `<a href>` (so
 * it opens in a new tab, and components render in isolation in tests),
 * upgraded to client-side navigation when a router is mounted.
 */
export const AppLink = React.forwardRef<
  HTMLAnchorElement,
  React.AnchorHTMLAttributes<HTMLAnchorElement> & { href: string }
>(({ href, onClick, ...props }, ref) => {
  const router = useRouter({ warn: false }) as ReturnType<typeof useRouter> | undefined;
  return (
    <a
      ref={ref}
      href={href}
      onClick={(e) => {
        onClick?.(e);
        if (
          e.defaultPrevented ||
          router === undefined ||
          router === null ||
          e.button !== 0 ||
          e.metaKey ||
          e.ctrlKey ||
          e.shiftKey ||
          e.altKey ||
          props.target === "_blank"
        ) {
          return;
        }
        e.preventDefault();
        router.history.push(href);
      }}
      {...props}
    />
  );
});
AppLink.displayName = "AppLink";
