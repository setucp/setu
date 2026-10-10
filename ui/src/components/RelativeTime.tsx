import { absoluteTime, relativeTime } from "@/lib/time";

/** "5 minutes ago", with the exact time on hover and for machines. */
export function RelativeTime({ iso, className }: { iso: string | null | undefined; className?: string }) {
  if (iso === null || iso === undefined || iso === "") {
    return <span className={className}>—</span>;
  }
  return (
    <time dateTime={iso} title={absoluteTime(iso)} className={className}>
      {relativeTime(iso)}
    </time>
  );
}
