/**
 * Hostname rules shared by the forms that take one. The server is the
 * judge (`setu_common::domain::normalise`); this only catches typos early.
 */
export const DOMAIN_RE = /^[a-z0-9]([a-z0-9-]*[a-z0-9])?(\.[a-z0-9]([a-z0-9-]*[a-z0-9])?)+$/i;

/** What the server stores for a typed host: trimmed, lower case, no trailing dot. */
export function normaliseHost(input: string): string {
  const t = input.trim().toLowerCase();
  return t.endsWith(".") ? t.slice(0, -1) : t;
}
