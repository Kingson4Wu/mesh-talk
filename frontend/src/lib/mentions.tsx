import { Fragment } from "react";

export const EMOJIS = ["👍", "❤️", "😂", "🎉", "🙏", "🔥", "😮", "😢"];

const MENTION_RE = /(@[\p{L}\p{N}_-]+)/u;

/** Split `text` into ordered plain / @mention segments (pure — unit-testable). */
export function mentionSegments(text: string): { text: string; mention: boolean }[] {
  return text
    .split(MENTION_RE)
    .filter((s) => s.length > 0)
    .map((s) => ({ text: s, mention: s.startsWith("@") }));
}

/** Render `text` with @mentions highlighted. */
export function renderWithMentions(text: string) {
  return mentionSegments(text).map((seg, i) =>
    seg.mention ? (
      <span key={i} className="rounded bg-primary/15 px-0.5 font-medium text-primary">
        {seg.text}
      </span>
    ) : (
      <Fragment key={i}>{seg.text}</Fragment>
    ),
  );
}

/** Does this text @-mention the given display name? */
export function mentionsName(text: string, name: string): boolean {
  if (!name) return false;
  const re = new RegExp(`@${name.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}\\b`, "i");
  return re.test(text);
}
