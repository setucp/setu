/**
 * Hostname rules shared by the forms that take one. The server is the
 * judge (`setu_common::domain::normalise`); these mirror it so the forms
 * neither refuse a name the server takes nor accept one it refuses.
 */
const ASCII_HOST_RE = /^[a-z0-9]([a-z0-9-]*[a-z0-9])?(\.[a-z0-9]([a-z0-9-]*[a-z0-9])?)+$/;

/** What the server stores for a typed host: trimmed, lower case, no trailing dot. */
export function normaliseHost(input: string): string {
  const t = input.trim().toLowerCase();
  return t.endsWith(".") ? t.slice(0, -1) : t;
}

/**
 * The ASCII (IDNA) form of a typed hostname, as the server stores it, or
 * `null` when the server would refuse it: empty, a wildcard, a port, path or
 * IP address, a single label, or a label over 63 characters.
 */
export function asciiHost(input: string): string | null {
  const host = normaliseHost(input);
  // `%` would be decoded by the URL parser.
  if (host === "" || /[*:/?#@%\\[\]\s]/.test(host)) return null;
  let ascii: string;
  try {
    // The URL parser applies the same UTS #46 mapping as the server's `idna`.
    ascii = new URL(`http://${host}/`).hostname;
  } catch {
    return null;
  }
  // Checked after the mapping, as the server does: every IPv4 form the parser
  // takes (`127.1`, `0x7f.1`, full-width `１２７.１`, `127。1`) comes out as a
  // dotted quad, whose last label is all digits; the server refuses those.
  if (/^\d+$/.test(ascii.split(".").at(-1) ?? "")) return null;
  if (ascii.length > 253 || !ASCII_HOST_RE.test(ascii)) return null;
  if (ascii.split(".").some((label) => label.length > 63)) return null;
  return ascii;
}

/** Whether the server would take `input` as a hostname. */
export function isHostname(input: string): boolean {
  return asciiHost(input) !== null;
}

const SECOND_LEVEL = ["co", "com", "org", "net", "gov", "ac", "edu"];

/**
 * The `www.` name the agent serves as an automatic redirect beside an apex
 * domain (`example.com`, `example.co.uk`), or `null` for a subdomain or a
 * name that already starts with `www.`; mirrors the agent's `www_name`.
 */
export function wwwName(domain: string): string | null {
  const [name = "", second = "", third, ...rest] = domain.split(".");
  if (name === "" || name === "www" || rest.length > 0) return null;
  const apex =
    third === undefined ? second.length >= 2 : SECOND_LEVEL.includes(second) && third.length === 2;
  return apex ? `www.${domain}` : null;
}
