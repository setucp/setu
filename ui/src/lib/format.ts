/** Bytes as B / KiB / MiB / GiB with one decimal. */
export function formatBytes(value: number | null | undefined): string {
  if (value === null || value === undefined) return "—";
  if (value < 1024) return `${value} B`;
  if (value < 1024 ** 2) return `${(value / 1024).toFixed(1)} KiB`;
  if (value < 1024 ** 3) return `${(value / 1024 ** 2).toFixed(1)} MiB`;
  return `${(value / 1024 ** 3).toFixed(1)} GiB`;
}

/** Megabytes as `512M` / `1.5G`. */
export function formatMb(value: number | null | undefined): string {
  if (value === null || value === undefined) return "—";
  return value >= 1024 ? `${(value / 1024).toFixed(1)}G` : `${value}M`;
}

export function formatSeconds(value: number | null | undefined): string {
  if (value === null || value === undefined) return "—";
  return `${value.toFixed(1)} s`;
}

/**
 * Orders dotted versions numerically (`0.10.0` > `0.2.0`); a pre-release
 * suffix (`1.0.0-rc1`) sorts before its release. Non-numeric parts
 * compare as strings.
 */
export function compareVersions(a: string, b: string): number {
  const [aCore = "", aPre] = a.split("-", 2);
  const [bCore = "", bPre] = b.split("-", 2);
  const ap = aCore.split(".");
  const bp = bCore.split(".");
  for (let i = 0; i < Math.max(ap.length, bp.length); i += 1) {
    const x = ap[i] ?? "0";
    const y = bp[i] ?? "0";
    const nx = Number(x);
    const ny = Number(y);
    const diff =
      Number.isInteger(nx) && Number.isInteger(ny) ? nx - ny : x.localeCompare(y);
    if (diff !== 0) return diff < 0 ? -1 : 1;
  }
  if (aPre === bPre) return 0;
  if (aPre === undefined) return 1;
  if (bPre === undefined) return -1;
  return aPre.localeCompare(bPre);
}

/** Parses `NAME=value` lines; blank lines and `#` comments are skipped. */
export function parseAssignments(text: string): {
  values: Record<string, string>;
  badLines: number[];
} {
  const values: Record<string, string> = {};
  const badLines: number[] = [];
  text.split("\n").forEach((raw, index) => {
    const line = raw.trim();
    if (line === "" || line.startsWith("#")) return;
    const eq = line.indexOf("=");
    const name = eq > 0 ? line.slice(0, eq).trim() : "";
    if (!/^[A-Z_][A-Z0-9_]*$/.test(name)) {
      badLines.push(index + 1);
      return;
    }
    values[name] = line.slice(eq + 1).trim();
  });
  return { values, badLines };
}
