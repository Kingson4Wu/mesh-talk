import { useEffect, useMemo, useRef, useState } from "react";
import { Virtuoso, type VirtuosoHandle } from "react-virtuoso";
import { ChevronDown } from "lucide-react";
import { useTranslation } from "react-i18next";
import { MessageBubble } from "./MessageBubble";
import { formatDay } from "@/lib/format";
import { useMotionOK } from "@/lib/motion";
import { useAuth } from "@/store/auth";
import {
  convKey,
  useChat,
  type ChatMessage,
  type Conversation,
} from "@/store/chat";
import type { ReactionInfo } from "@/lib/types";

// Stable empty references. Returning a fresh `[]` from a zustand selector makes
// useSyncExternalStore read a new snapshot on every render → an infinite render loop
// ("Maximum update depth exceeded") that tears the whole app down to a blank screen.
const NO_MESSAGES: ChatMessage[] = [];
const NO_REACTIONS: ReactionInfo[] = [];

/** A quiet centered date separator between days in the message log. */
function DaySeparator({ label }: { label: string }) {
  return (
    <div className="flex items-center gap-3 px-4 py-3">
      <span className="h-px flex-1 bg-border" />
      <span className="font-mono text-[11px] uppercase tracking-wider text-muted-foreground">
        {label}
      </span>
      <span className="h-px flex-1 bg-border" />
    </div>
  );
}

/**
 * The scrollable, virtualized message log for a conversation — shared by the desktop
 * ConversationView and the mobile conversation screen. Owns ONLY the log (loading/empty states,
 * the Virtuoso list, reactions, day separators, fresh-message animation, jump-to-bottom); the
 * surrounding header + Composer are the shell's. Layout-neutral (`flex-1`, no fixed widths), so it
 * fits either a two-pane or single-pane parent. Raises replies via `onReply`.
 */
export function MessageStream({
  active,
  headerName,
  onReply,
}: {
  active: Conversation;
  headerName: string;
  onReply: (m: ChatMessage) => void;
}) {
  const { t } = useTranslation();
  const motionOK = useMotionOK();
  const retry = useChat((s) => s.retry);
  const toggleReaction = useChat((s) => s.toggleReaction);
  const myId = useChat((s) => s.myId);
  const myAccountId = useChat((s) => s.myAccountId);
  const members = useChat((s) => s.members);
  const peers = useChat((s) => s.peers);
  const loading = useChat((s) => s.loading);
  const myName = useAuth((s) => s.user?.username ?? "");

  const key = convKey(active);
  const messages = useChat((s) => s.messages[key] ?? NO_MESSAGES);
  const reactions = useChat((s) => s.reactions[key] ?? NO_REACTIONS);

  const isChannel = active.kind === "channel";
  // Reaction `who` is keyed by account id for account conversations, device user-id for channels.
  const selfReactionId = isChannel ? myId : myAccountId;

  // Map a channel author's device user_id → account id, so the per-message author glyph resolves
  // the account-keyed (propagated/custom) avatar instead of a device id that never matches.
  const accountByDevice = useMemo(() => {
    const map = new Map<string, string>();
    for (const p of peers) if (p.account_id) map.set(p.user_id, p.account_id);
    return map;
  }, [peers]);
  // Resolve a channel author's device user_id → display name (channel members win over the roster).
  const nameByDevice = useMemo(() => {
    const map = new Map<string, string>();
    for (const p of peers) if (p.name) map.set(p.user_id, p.name);
    for (const m of members) if (m.name) map.set(m.user_id, m.name);
    return map;
  }, [peers, members]);

  const virtuosoRef = useRef<VirtuosoHandle>(null);
  // `showJump` reveals the jump-to-bottom button whenever the user has scrolled up.
  const [showJump, setShowJump] = useState(false);
  useEffect(() => {
    setShowJump(false);
  }, [key]);

  // Message-enter motion: animate ONLY messages that arrive after a conversation's initial load,
  // never the initial backlog and never on scroll. `seenKeys` is the baseline already shown (READ
  // in render, MUTATED only post-commit); `primed` excludes the very first backlog load.
  const rowKey = (m: ChatMessage, i: number) =>
    m.id ?? m.clientId ?? `pending-${i}`;
  const seenKeys = useRef<Set<string>>(new Set());
  const primed = useRef(false);
  useEffect(() => {
    seenKeys.current = new Set();
    primed.current = false;
  }, [key]);
  useEffect(() => {
    messages.forEach((m, i) => seenKeys.current.add(rowKey(m, i)));
    if (!primed.current && messages.length > 0) primed.current = true;
  }, [messages, key]);
  const isFresh = (m: ChatMessage, i: number) =>
    primed.current && !seenKeys.current.has(rowKey(m, i));

  const byId = useMemo(() => {
    const m = new Map<string, ChatMessage>();
    for (const msg of messages) if (msg.id) m.set(msg.id, msg);
    return m;
  }, [messages]);

  const reactionsByTarget = useMemo(() => {
    const m = new Map<string, ReactionInfo[]>();
    for (const r of reactions) {
      const arr = m.get(r.target) ?? [];
      arr.push(r);
      m.set(r.target, arr);
    }
    return m;
  }, [reactions]);

  return (
    <div className="relative flex-1 overflow-hidden">
      {loading && messages.length === 0 && (
        <p className="py-8 text-center text-sm text-muted-foreground">
          {t("common.loading")}
        </p>
      )}
      {!loading && messages.length === 0 && (
        <p
          data-testid="conversation-empty"
          className="py-8 text-center text-sm text-muted-foreground"
        >
          {t("conversation.noMessages")}
        </p>
      )}
      {messages.length > 0 && (
        <Virtuoso
          ref={virtuosoRef}
          key={key}
          data={messages}
          role="log"
          aria-live="polite"
          aria-label={t("conversation.messageLog", { name: headerName })}
          className="h-full py-4"
          alignToBottom
          initialTopMostItemIndex={messages.length - 1}
          followOutput={(isAtBottom) => (isAtBottom ? "auto" : false)}
          atBottomStateChange={(b) => setShowJump(!b)}
          atBottomThreshold={48}
          increaseViewportBy={400}
          itemContent={(i, m) => {
            const prev = messages[i - 1];
            const showAuthor =
              isChannel &&
              (!prev || prev.who !== m.who || prev.fromMe !== m.fromMe);
            const showDay =
              !prev || formatDay(prev.wallClock) !== formatDay(m.wallClock);
            const parent = m.replyTo ? (byId.get(m.replyTo) ?? null) : null;
            return (
              <div className="px-0 pb-0.5">
                {showDay && <DaySeparator label={formatDay(m.wallClock)} />}
                <MessageBubble
                  m={m}
                  parent={parent}
                  showAuthor={showAuthor}
                  isChannel={isChannel}
                  authorAvatarId={accountByDevice.get(m.who) ?? m.who}
                  authorName={nameByDevice.get(m.who)}
                  fresh={motionOK && isFresh(m, i)}
                  reactions={m.id ? (reactionsByTarget.get(m.id) ?? []) : []}
                  selfReactionId={selfReactionId}
                  myName={myName}
                  onReply={onReply}
                  onReact={toggleReaction}
                  onRetry={retry}
                />
              </div>
            );
          }}
          computeItemKey={(i, m) => rowKey(m, i)}
        />
      )}
      {showJump && (
        <button
          type="button"
          aria-label={t("conversation.jumpToLatest")}
          onClick={() =>
            virtuosoRef.current?.scrollToIndex({
              index: messages.length - 1,
              behavior: "smooth",
            })
          }
          className="absolute bottom-3 right-4 z-10 rounded-full border bg-background/90 p-2 shadow-md backdrop-blur hover:bg-accent"
        >
          <ChevronDown className="h-4 w-4" />
        </button>
      )}
    </div>
  );
}
