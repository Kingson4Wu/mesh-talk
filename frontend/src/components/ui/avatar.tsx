import { cn } from "@/lib/utils";

/** Deterministic gradient avatar from an id/name + initials. */
function hashHue(s: string): number {
  let h = 0;
  for (let i = 0; i < s.length; i++) h = (h * 31 + s.charCodeAt(i)) % 360;
  return h;
}

export function Avatar({
  name,
  id,
  className,
}: {
  name: string;
  id: string;
  className?: string;
}) {
  const initials = (name || id)
    .split(/\s+/)
    .map((w) => w[0])
    .filter(Boolean)
    .slice(0, 2)
    .join("")
    .toUpperCase();
  const hue = hashHue(id || name);
  return (
    <div
      className={cn(
        "flex shrink-0 items-center justify-center rounded-full text-xs font-semibold text-white",
        className,
      )}
      style={{
        background: `linear-gradient(135deg, hsl(${hue} 65% 55%), hsl(${(hue + 40) % 360} 65% 45%))`,
      }}
    >
      {initials || "?"}
    </div>
  );
}
