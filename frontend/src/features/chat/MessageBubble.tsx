import { useState } from "react";
import { CornerUpLeft, SmilePlus } from "lucide-react";
import { Avatar } from "@/components/ui/avatar";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { cn } from "@/lib/utils";
import { formatTime, shortId } from "@/lib/format";
import { EMOJIS, mentionsName, renderWithMentions } from "@/lib/mentions";
import type { ReactionInfo } from "@/lib/types";
import type { ChatMessage } from "@/store/chat";

export function MessageBubble({
  m,
  parent,
  showAuthor,
  reactions,
  myId,
  myName,
  onReply,
  onReact,
}: {
  m: ChatMessage;
  parent: ChatMessage | null;
  showAuthor: boolean;
  reactions: ReactionInfo[];
  myId: string;
  myName: string;
  onReply: (m: ChatMessage) => void;
  onReact: (target: string, emoji: string) => void;
}) {
  const mine = m.fromMe;
  const [pickerOpen, setPickerOpen] = useState(false);
  const mentioned = !mine && mentionsName(m.text, myName);

  return (
    <div
      className={cn(
        "group flex gap-2.5 px-4 py-0.5",
        mine && "flex-row-reverse",
      )}
    >
      <div className="w-8 shrink-0">
        {showAuthor && !mine && <Avatar name={m.who} id={m.who} className="h-8 w-8" />}
      </div>

      <div className={cn("flex max-w-[72%] flex-col", mine ? "items-end" : "items-start")}>
        {showAuthor && !mine && (
          <span className="mb-1 px-1 text-xs font-medium text-muted-foreground">
            {m.who.length > 20 ? shortId(m.who, 10) : m.who}
          </span>
        )}

        <div className="flex items-center gap-1">
          {/* hover actions (left of bubble for own messages) */}
          {mine && (
            <Actions
              onReply={() => onReply(m)}
              pickerOpen={pickerOpen}
              setPickerOpen={setPickerOpen}
              onPick={(e) => m.id && onReact(m.id, e)}
              disabled={!m.id}
            />
          )}

          <div
            className={cn(
              "rounded-2xl px-3.5 py-2 text-sm shadow-sm",
              mine
                ? "rounded-br-md bg-primary text-primary-foreground"
                : "rounded-bl-md bg-secondary text-secondary-foreground",
              mentioned && "ring-2 ring-amber-400/70",
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
            <span className="whitespace-pre-wrap break-words">
              {renderWithMentions(m.text)}
            </span>
          </div>

          {!mine && (
            <Actions
              onReply={() => onReply(m)}
              pickerOpen={pickerOpen}
              setPickerOpen={setPickerOpen}
              onPick={(e) => m.id && onReact(m.id, e)}
              disabled={!m.id}
            />
          )}
        </div>

        {/* reaction chips */}
        {reactions.length > 0 && (
          <div className={cn("mt-1 flex flex-wrap gap-1", mine && "justify-end")}>
            {reactions.map((r) => {
              const me = r.who.includes(myId);
              return (
                <button
                  key={r.emoji}
                  onClick={() => m.id && onReact(m.id, r.emoji)}
                  className={cn(
                    "flex items-center gap-1 rounded-full border px-1.5 py-0.5 text-xs transition-colors",
                    me
                      ? "border-primary bg-primary/15 text-primary"
                      : "border-border bg-muted/50 hover:bg-muted",
                  )}
                >
                  <span>{r.emoji}</span>
                  <span className="text-[10px]">{r.who.length}</span>
                </button>
              );
            })}
          </div>
        )}

        <span className="mt-0.5 px-1 text-[10px] text-muted-foreground">
          {formatTime(m.wallClock)}
          {m.pending && " · sending…"}
        </span>
      </div>
    </div>
  );
}

function Actions({
  onReply,
  onPick,
  pickerOpen,
  setPickerOpen,
  disabled,
}: {
  onReply: () => void;
  onPick: (emoji: string) => void;
  pickerOpen: boolean;
  setPickerOpen: (v: boolean) => void;
  disabled: boolean;
}) {
  return (
    <div className="flex items-center gap-0.5 opacity-0 transition-opacity group-hover:opacity-100">
      <button
        onClick={onReply}
        disabled={disabled}
        title="Reply"
        className="rounded-md p-1 text-muted-foreground hover:bg-accent hover:text-foreground disabled:opacity-30"
      >
        <CornerUpLeft className="h-3.5 w-3.5" />
      </button>
      <Popover open={pickerOpen} onOpenChange={setPickerOpen}>
        <PopoverTrigger asChild>
          <button
            disabled={disabled}
            title="React"
            className="rounded-md p-1 text-muted-foreground hover:bg-accent hover:text-foreground disabled:opacity-30"
          >
            <SmilePlus className="h-3.5 w-3.5" />
          </button>
        </PopoverTrigger>
        <PopoverContent className="w-auto">
          <div className="flex gap-1">
            {EMOJIS.map((e) => (
              <button
                key={e}
                onClick={() => {
                  onPick(e);
                  setPickerOpen(false);
                }}
                className="rounded-md p-1 text-lg hover:bg-accent"
              >
                {e}
              </button>
            ))}
          </div>
        </PopoverContent>
      </Popover>
    </div>
  );
}
