import { useMemo, useRef, useState } from "react";
import {
  SendHorizontal,
  X,
  CornerUpLeft,
  Paperclip,
  Smile,
} from "lucide-react";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";
import type { ChatMessage } from "@/store/chat";

// A small curated palette — enough for everyday chat without pulling in a heavy emoji library.
const EMOJIS = [
  "😀",
  "😂",
  "🙂",
  "😉",
  "😍",
  "😎",
  "🤔",
  "😅",
  "😭",
  "😡",
  "👍",
  "👎",
  "🙏",
  "👏",
  "🙌",
  "💪",
  "👀",
  "👋",
  "🤝",
  "🤙",
  "🔥",
  "🎉",
  "✨",
  "⭐",
  "❤️",
  "💯",
  "✅",
  "❌",
  "🚀",
  "💡",
  "☕",
  "🍻",
  "🥳",
  "🤯",
  "🙈",
  "😴",
  "🎯",
  "📎",
  "💀",
  "🤗",
];

export function Composer({
  onSend,
  onAttach,
  placeholder,
  replyTo,
  onCancelReply,
  mentionNames,
}: {
  onSend: (text: string) => void;
  onAttach?: () => void;
  placeholder: string;
  replyTo?: ChatMessage | null;
  onCancelReply?: () => void;
  mentionNames: string[];
}) {
  const [text, setText] = useState("");
  const ref = useRef<HTMLTextAreaElement>(null);
  const [mentionQuery, setMentionQuery] = useState<string | null>(null);
  const [showEmoji, setShowEmoji] = useState(false);

  const insertEmoji = (emoji: string) => {
    const el = ref.current;
    const caret = el?.selectionStart ?? text.length;
    setText(text.slice(0, caret) + emoji + text.slice(caret));
    setShowEmoji(false);
    queueMicrotask(() => {
      el?.focus();
      const pos = caret + emoji.length;
      el?.setSelectionRange(pos, pos);
    });
  };

  const suggestions = useMemo(() => {
    if (mentionQuery === null) return [];
    const q = mentionQuery.toLowerCase();
    return mentionNames
      .filter((n) => n && n.toLowerCase().startsWith(q))
      .slice(0, 6);
  }, [mentionQuery, mentionNames]);

  const detectMention = (value: string, caret: number) => {
    const before = value.slice(0, caret);
    const m = before.match(/@([\p{L}\p{N}_-]*)$/u);
    setMentionQuery(m ? m[1] : null);
  };

  const applyMention = (name: string) => {
    const el = ref.current;
    if (!el) return;
    const caret = el.selectionStart ?? text.length;
    const before = text
      .slice(0, caret)
      .replace(/@([\p{L}\p{N}_-]*)$/u, `@${name} `);
    const next = before + text.slice(caret);
    setText(next);
    setMentionQuery(null);
    queueMicrotask(() => el.focus());
  };

  const send = () => {
    const t = text.trim();
    if (!t) return;
    onSend(t);
    setText("");
    setMentionQuery(null);
    if (ref.current) ref.current.style.height = "auto";
  };

  const onKeyDown = (e: React.KeyboardEvent) => {
    if (suggestions.length > 0 && e.key === "Enter") {
      e.preventDefault();
      applyMention(suggestions[0]);
      return;
    }
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      send();
    }
  };

  const onInput = (e: React.FormEvent<HTMLTextAreaElement>) => {
    const el = e.currentTarget;
    setText(el.value);
    el.style.height = "auto";
    el.style.height = `${Math.min(el.scrollHeight, 160)}px`;
    detectMention(el.value, el.selectionStart ?? el.value.length);
  };

  return (
    <div className="border-t bg-card/40 p-3">
      {replyTo && (
        <div className="mb-2 flex items-center gap-2 rounded-lg border bg-background px-3 py-1.5 text-sm">
          <CornerUpLeft className="h-3.5 w-3.5 shrink-0 text-primary" />
          <span className="min-w-0 flex-1 truncate text-muted-foreground">
            Replying to{" "}
            <span className="text-foreground">{replyTo.text || "message"}</span>
          </span>
          <button
            onClick={onCancelReply}
            className="rounded p-0.5 text-muted-foreground hover:bg-accent"
          >
            <X className="h-3.5 w-3.5" />
          </button>
        </div>
      )}

      <div className="relative">
        {suggestions.length > 0 && (
          <div className="absolute bottom-full left-0 mb-2 w-56 overflow-hidden rounded-xl border bg-popover shadow-xl">
            {suggestions.map((n) => (
              <button
                key={n}
                onClick={() => applyMention(n)}
                className="block w-full px-3 py-2 text-left text-sm hover:bg-accent"
              >
                @{n}
              </button>
            ))}
          </div>
        )}

        {showEmoji && (
          <div className="absolute bottom-full left-0 mb-2 w-72 rounded-xl border bg-popover p-2 shadow-xl">
            <div className="grid grid-cols-10 gap-0.5">
              {EMOJIS.map((e) => (
                <button
                  key={e}
                  type="button"
                  onClick={() => insertEmoji(e)}
                  className="rounded-md p-1 text-lg leading-none hover:bg-accent"
                  aria-label={`Insert ${e}`}
                >
                  {e}
                </button>
              ))}
            </div>
          </div>
        )}

        <div className="flex items-end gap-2 rounded-2xl border bg-background p-2 focus-within:ring-2 focus-within:ring-ring">
          {onAttach && (
            <Button
              variant="ghost"
              size="icon"
              className="h-9 w-9 shrink-0 rounded-xl text-muted-foreground"
              title="Attach a file"
              onClick={onAttach}
            >
              <Paperclip className="h-4 w-4" />
            </Button>
          )}
          <Button
            variant="ghost"
            size="icon"
            className="h-9 w-9 shrink-0 rounded-xl text-muted-foreground"
            title="Emoji"
            aria-label="Emoji"
            onClick={() => setShowEmoji((v) => !v)}
          >
            <Smile className="h-4 w-4" />
          </Button>
          <textarea
            ref={ref}
            rows={1}
            value={text}
            onChange={onInput}
            onKeyDown={onKeyDown}
            placeholder={placeholder}
            className="max-h-40 flex-1 resize-none bg-transparent px-1 py-1.5 text-sm outline-none placeholder:text-muted-foreground"
          />
          <Button
            size="icon"
            className={cn(
              "h-9 w-9 shrink-0 rounded-xl",
              !text.trim() && "opacity-50",
            )}
            disabled={!text.trim()}
            onClick={send}
          >
            <SendHorizontal className="h-4 w-4" />
          </Button>
        </div>
      </div>
    </div>
  );
}
