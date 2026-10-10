const UNITS: Array<[Intl.RelativeTimeFormatUnit, number]> = [
  ["year", 365 * 24 * 3600],
  ["month", 30 * 24 * 3600],
  ["week", 7 * 24 * 3600],
  ["day", 24 * 3600],
  ["hour", 3600],
  ["minute", 60],
];

const rtf = new Intl.RelativeTimeFormat("en", { numeric: "auto" });

/** "just now", "5 minutes ago", "yesterday", "in 2 hours". */
export function relativeTime(
  iso: string | null | undefined,
  now: number = Date.now(),
): string {
  if (iso === null || iso === undefined || iso === "") return "—";
  const at = new Date(iso).getTime();
  if (Number.isNaN(at)) return "—";
  const seconds = Math.round((at - now) / 1000);
  if (Math.abs(seconds) < 45) return "just now";
  for (const [unit, size] of UNITS) {
    if (Math.abs(seconds) >= size) return rtf.format(Math.round(seconds / size), unit);
  }
  return rtf.format(Math.round(seconds / 60) || (seconds < 0 ? -1 : 1), "minute");
}

/** Absolute timestamp in the visitor's locale, for `title` attributes. */
export function absoluteTime(iso: string | null | undefined): string {
  if (iso === null || iso === undefined || iso === "") return "";
  return new Date(iso).toLocaleString();
}

/** "850 ms", "42 s", "3 m 12 s", "1 h 4 m"; "—" while unknown. */
export function duration(
  start: string | null | undefined,
  end: string | null | undefined,
  now: number = Date.now(),
): string {
  if (start === null || start === undefined) return "—";
  const from = new Date(start).getTime();
  const to = end === null || end === undefined ? now : new Date(end).getTime();
  const ms = Math.max(0, to - from);
  if (ms < 1000) return `${ms} ms`;
  const s = Math.round(ms / 1000);
  if (s < 60) return `${s} s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m} m ${s % 60} s`;
  return `${Math.floor(m / 60)} h ${m % 60} m`;
}
