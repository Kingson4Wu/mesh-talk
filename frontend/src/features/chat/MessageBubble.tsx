import { CornerUpLeft } from "lucide-react";
import { Avatar } from "@/components/ui/avatar";
import { cn } from "@/lib/utils";
import { formatTime, shortId } from "@/lib/format";
import type { ChatMessage } from "@/store/chat";

export function MessageBubble({
  m,
  parent,
  showAuthor,
}: {
  m: ChatMessage;
  parent: ChatMessage | null;
  showAuthor: boolean;
}) {
  const mine = m.fromMe;
  return (
    <div className={cn("flex gap-2.5 px-4", mine && "flex-row-reverse")}>
      <div className="w-8 shrink-0">
        {showAuthor && !mine && (
          <Avatar name={m.who} id={m.who} className="h-8 w-8" />
        )}
      </div>
      <div
        className={cn(
          "flex max-w-[72%] flex-col",
          mine ? "items-end" : "items-start",
        )}
      >
        {showAuthor && !mine && (
          <span className="mb-1 px-1 text-xs font-medium text-muted-foreground">
            {m.who.length > 20 ? shortId(m.who, 10) : m.who}
          </span>
        )}
        <div
          className={cn(
            "rounded-2xl px-3.5 py-2 text-sm shadow-sm",
            mine
              ? "rounded-br-md bg-primary text-primary-foreground"
              : "rounded-bl-md bg-secondary text-secondary-foreground",
            m.pending && "opacity-60",
          )}
        >
          {parent && (
            <div
              className={cn(
                "mb-1 flex items-center gap-1 rounded-md border-l-2 px-2 py-1 text-xs",
                mine
                  ? "border-primary-foreground/40 bg-primary-foreground/10"
                  : "border-primary/50 bg-background/40",
              )}
            >
              <CornerUpLeft className="h-3 w-3 shrink-0 opacity-70" />
              <span className="truncate opacity-80">{parent.text || "message"}</span>
            </div>
          )}
          <span className="whitespace-pre-wrap break-words">{m.text}</span>
        </div>
        <span className="mt-0.5 px-1 text-[10px] text-muted-foreground">
          {formatTime(m.wallClock)}
        </span>
      </div>
    </div>
  );
}
