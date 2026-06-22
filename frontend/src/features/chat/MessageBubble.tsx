import { useState } from "react";
import { CornerUpLeft, RotateCw, SmilePlus } from "lucide-react";
import { motion } from "framer-motion";
import { useTranslation } from "react-i18next";
import { IdentityGlyph } from "@/components/identity";
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from "@/components/ui/popover";
import { cn } from "@/lib/utils";
import { formatTime, shortId } from "@/lib/format";
import { fadeSlideUp } from "@/lib/motion";
import { EMOJIS, mentionsName, renderWithMentions } from "@/lib/mentions";
import type { ReactionInfo } from "@/lib/types";
import type { ChatMessage } from "@/store/chat";

export function MessageBubble({
  m,
  parent,
  showAuthor,
  fresh,
  reactions,
  selfReactionId,
  myName,
  onReply,
  onReact,
  onRetry,
}: {
  m: ChatMessage;
  parent: ChatMessage | null;
  showAuthor: boolean;
  /** True only for a genuinely newly-arriving/sent message → plays an entrance. */
  fresh?: boolean;
  reactions: ReactionInfo[];
  // The id that represents "me" in a reaction's `who`: the ACCOUNT id for account
  // conversations (who is account-keyed there), the device user-id for channels.
  selfReactionId: string;
  myName: string;
  onReply: (m: ChatMessage) => void;
  onReact: (target: string, emoji: string) => void;
  onRetry: (clientId: string) => void;
}) {
  const { t } = useTranslation();
  const mine = m.fromMe;
  const [pickerOpen, setPickerOpen] = useState(false);
  const mentioned = !mine && mentionsName(m.text, myName);
  // A long `who` is a raw user id — render it mono (it's an identifier).
  const isIdName = m.who.length > 20;
  const authorLabel = isIdName ? shortId(m.who, 10) : m.who;

  // Accessible summary of the bubble: who, the text, and the time — folding the transient
  // pending/failed state so a screen reader announces the message's status too.
  const speaker = mine ? t("common.you") : authorLabel;
  const status = m.pending
    ? t("message.sending")
    : m.failed
      ? t(`message.fail.${m.failReason ?? "unknown"}`)
      : "";
  const ariaLabel = `${speaker}: ${m.text} · ${formatTime(m.wallClock)}${
    status ? ` · ${status}` : ""
  }`;

  return (
    <motion.div
      role="article"
      aria-label={ariaLabel}
      initial={fresh ? "hidden" : false}
      animate="visible"
      variants={fadeSlideUp}
      className={cn(
        "group flex gap-2.5 px-4 py-0.5",
        mine && "flex-row-reverse",
      )}
    >
      <div className="w-8 shrink-0">
        {showAuthor && !mine && (
          <IdentityGlyph seed={m.who} size={32} title={authorLabel} />
        )}
      </div>

      <div
        className={cn(
          "flex max-w-[72%] flex-col",
          mine ? "items-end" : "items-start",
        )}
      >
        {showAuthor && !mine && (
          <span
            className={cn(
              "mb-1 px-1 text-xs font-medium text-muted-foreground",
              isIdName ? "font-mono" : "font-display tracking-tight",
            )}
          >
            {authorLabel}
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
              "rounded-2xl px-3.5 py-2 text-sm transition-shadow",
              mine
                ? "rounded-br-md bg-signal text-primary-foreground shadow-sm"
                : "rounded-bl-md border border-border bg-card text-card-foreground shadow-elevation",
              mentioned && "ring-2 ring-mention/70",
              m.pending && "opacity-60",
            )}
          >
            {parent && (
              <div
                className={cn(
                  "mb-1 flex items-center gap-1 rounded-md border-l-2 px-2 py-1 text-xs",
                  mine
                    ? "border-primary-foreground/40 bg-primary-foreground/10"
                    : "border-signal/50 bg-muted/50",
                )}
              >
                <CornerUpLeft className="h-3 w-3 shrink-0 opacity-70" />
                <span className="truncate opacity-80">
                  {parent.text || t("composer.message")}
                </span>
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
          <div
            className={cn("mt-1 flex flex-wrap gap-1", mine && "justify-end")}
          >
            {reactions.map((r) => {
              const me = r.who.includes(selfReactionId);
              return (
                <button
                  key={`${r.target}:${r.emoji}`}
                  type="button"
                  aria-label={`${me ? "Remove your" : "Add"} ${r.emoji} reaction`}
                  onClick={() => m.id && onReact(m.id, r.emoji)}
                  className={cn(
                    "flex items-center gap-1 rounded-full border px-1.5 py-0.5 text-xs transition-colors",
                    me
                      ? "border-signal bg-signal/15 text-signal"
                      : "border-border bg-muted/50 hover:bg-muted",
                  )}
                >
                  <span>{r.emoji}</span>
                  <span className="font-mono text-[10px]">{r.who.length}</span>
                </button>
              );
            })}
          </div>
        )}

        <span className="mt-0.5 px-1 font-mono text-[10px] text-muted-foreground">
          {formatTime(m.wallClock)}
          {m.pending && ` · ${t("message.sending")}`}
        </span>

        {m.failed && (
          <span
            className={cn(
              "mt-0.5 flex items-center gap-1.5 px-1 text-[10px] text-destructive",
              mine && "flex-row-reverse",
            )}
          >
            <span>{t(`message.fail.${m.failReason ?? "unknown"}`)}</span>
            <button
              type="button"
              onClick={() => m.clientId && onRetry(m.clientId)}
              disabled={!m.clientId}
              aria-label={t("message.retry")}
              className="inline-flex items-center gap-0.5 rounded px-1 py-0.5 font-medium text-destructive underline-offset-2 hover:bg-destructive/10 hover:underline disabled:opacity-40"
            >
              <RotateCw className="h-2.5 w-2.5" />
              {t("message.retry")}
            </button>
          </span>
        )}
      </div>
    </motion.div>
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
  const { t } = useTranslation();
  return (
    <div className="flex items-center gap-0.5 opacity-0 transition-opacity group-hover:opacity-100">
      <button
        onClick={onReply}
        disabled={disabled}
        title={t("message.reply")}
        aria-label={t("message.reply")}
        className="rounded-md p-1 text-muted-foreground hover:bg-accent hover:text-foreground disabled:opacity-30"
      >
        <CornerUpLeft className="h-3.5 w-3.5" />
      </button>
      <Popover open={pickerOpen} onOpenChange={setPickerOpen}>
        <PopoverTrigger asChild>
          <button
            disabled={disabled}
            title={t("message.react")}
            aria-label={t("message.react")}
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
