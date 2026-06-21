import { useTransfer } from "@/store/transfers";

/** A thin progress bar for an in-flight file transfer keyed by `transferKey` (the
 *  per-file conv id for a save, or the conversation target for a send). Renders
 *  nothing when there is no active transfer for the key. Reads ONLY the transfers
 *  store, so its ~10/s updates don't re-render the surrounding message list. */
export function TransferBar({
  transferKey,
  label,
}: {
  transferKey: string | undefined;
  label?: string;
}) {
  const t = useTransfer(transferKey);
  if (!t || t.total === 0) return null;
  const pct = Math.min(100, Math.round((t.done / t.total) * 100));
  const verb = t.direction === "send" ? "Sending" : "Saving";
  return (
    <div className="px-1 py-1">
      <div className="mb-0.5 flex justify-between text-xs text-muted-foreground">
        <span>
          {verb}
          {label ? ` ${label}` : ""}…
        </span>
        <span>{pct}%</span>
      </div>
      <div className="h-1.5 w-full overflow-hidden rounded-full bg-muted">
        <div
          className="h-full rounded-full bg-primary transition-[width] duration-150"
          style={{ width: `${pct}%` }}
        />
      </div>
    </div>
  );
}
