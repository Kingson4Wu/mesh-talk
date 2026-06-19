import { Fragment } from "react";

export const EMOJIS = ["👍", "❤️", "😂", "🎉", "🙏", "🔥", "😮", "😢"];

const MENTION_RE = /(@[\p{L}\p{N}_-]+)/u;

/** Split text into plain + @mention segments for highlighting. */
export function renderWithMentions(text: string) {
  return text.split(MENTION_RE).map((seg, i) =>
    seg.startsWith("@") ? (
      <span key={i} className="rounded bg-primary/15 px-0.5 font-medium text-primary">
        {seg}
      </span>
    ) : (
      <Fragment key={i}>{seg}</Fragment>
    ),
  );
}

/** Does this text @-mention the given display name? */
export function mentionsName(text: string, name: string): boolean {
  if (!name) return false;
  const re = new RegExp(`@${name.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}\\b`, "i");
  return re.test(text);
}
