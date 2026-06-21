import { useEffect, useMemo, useRef, useState } from "react";
import { Virtuoso, type VirtuosoHandle } from "react-virtuoso";
import { open as openFileDialog } from "@tauri-apps/plugin-dialog";
import { ChevronDown, Hash, Loader2, MessagesSquare } from "lucide-react";
import { Avatar } from "@/components/ui/avatar";
import { Composer } from "./Composer";
import { MessageBubble } from "./MessageBubble";
import { MembersDialog } from "./MembersDialog";
import { VerifyContactDialog } from "./VerifyContactDialog";
import { TransferBar } from "./TransferBar";
import { errorMessage } from "@/lib/error";
import { shortId } from "@/lib/format";
import { useAuth } from "@/store/auth";
import { convKey, useChat, type ChatMessage } from "@/store/chat";
import type { ReactionInfo } from "@/lib/types";

function EmptyState() {
  return (
    <div className="flex flex-1 flex-col items-center justify-center gap-3 text-center">
      <div className="flex h-16 w-16 items-center justify-center rounded-2xl bg-muted text-muted-foreground">
        <MessagesSquare className="h-8 w-8" />
      </div>
      <div>
        <p className="font-medium">No conversation selected</p>
        <p className="text-sm text-muted-foreground">
          Pick a contact or channel to start chatting.
        </p>
      </div>
    </div>
  );
}

// Shown on first boot while the node's encrypted stores are still being unlocked in the
// background (the heavy argon2 KDF + store opens). The login invoke already returned, so
// this is a non-blocking "two-phase startup" state — not a frozen window.
function UnlockingState() {
  return (
    <div className="flex flex-1 flex-col items-center justify-center gap-3 text-center">
      <Loader2 className="h-7 w-7 animate-spin text-muted-foreground" />
      <div>
        <p className="font-medium">Unlocking your encrypted stores…</p>
        <p className="text-sm text-muted-foreground">
          Decrypting keys and starting peer discovery.
        </p>
      </div>
    </div>
  );
}

// Stable empty references. Returning a fresh `[]` from a zustand selector makes
// useSyncExternalStore read a new snapshot on every render → an infinite render loop
// ("Maximum update depth exceeded") that tears the whole app down to a blank screen.
const NO_MESSAGES: ChatMessage[] = [];
const NO_REACTIONS: ReactionInfo[] = [];

export function ConversationView() {
  const active = useChat((s) => s.active);
  const send = useChat((s) => s.send);
  const sendFile = useChat((s) => s.sendFile);
  const setError = useChat((s) => s.setError);
  const toggleReaction = useChat((s) => s.toggleReaction);
  const myId = useChat((s) => s.myId);
  const myAccountId = useChat((s) => s.myAccountId);
  const members = useChat((s) => s.members);
  const key = active ? convKey(active) : "";
  const messages = useChat((s) =>
    active ? (s.messages[key] ?? NO_MESSAGES) : NO_MESSAGES,
  );
  const reactions = useChat((s) =>
    active ? (s.reactions[key] ?? NO_REACTIONS) : NO_REACTIONS,
  );
  const loading = useChat((s) => s.loading);
  const ready = useChat((s) => s.ready);
  const myName = useAuth((s) => s.user?.username ?? "");

  const virtuosoRef = useRef<VirtuosoHandle>(null);

  const [replyTo, setReplyTo] = useState<ChatMessage | null>(null);
  // `showJump` reveals the jump-to-bottom button whenever the user has scrolled up.
  // Virtuoso's `followOutput` only sticks to the newest message while already at the
  // bottom, so reading history is never interrupted by an inbound message.
  const [showJump, setShowJump] = useState(false);
  useEffect(() => {
    setReplyTo(null);
    setShowJump(false);
  }, [key]);

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

  const mentionNames = useMemo(
    () =>
      active?.kind === "channel"
        ? members.map((m) => m.name).filter(Boolean)
        : active
          ? [active.name]
          : [],
    [active, members],
  );

  if (!active) {
    return (
      <main className="flex flex-1 flex-col">
        {ready ? <EmptyState /> : <UnlockingState />}
      </main>
    );
  }

  const isChannel = active.kind === "channel";
  // Reaction `who` is keyed by account id for account conversations, device user-id for channels.
  const selfReactionId = isChannel ? myId : myAccountId;

  return (
    <main className="flex flex-1 flex-col">
      <header className="flex items-center gap-3 border-b px-5 py-3">
        {isChannel ? (
          <div className="flex h-9 w-9 items-center justify-center rounded-full bg-muted text-muted-foreground">
            <Hash className="h-4 w-4" />
          </div>
        ) : (
          <Avatar name={active.name} id={active.id} className="h-9 w-9" />
        )}
        <div className="min-w-0 flex-1">
          <div className="truncate font-semibold">{active.name}</div>
          <div className="truncate text-xs text-muted-foreground">
            {isChannel
              ? `${members.length || ""} ${members.length ? "members" : "channel"}`.trim()
              : `Direct message · ${shortId(active.id, 12)}`}
          </div>
        </div>
        {isChannel ? (
          <MembersDialog />
        ) : (
          active.kind === "account" && (
            <VerifyContactDialog accountId={active.id} name={active.name} />
          )
        )}
      </header>

      <div className="relative flex-1 overflow-hidden">
        {loading && messages.length === 0 && (
          <p className="py-8 text-center text-sm text-muted-foreground">
            Loading…
          </p>
        )}
        {!loading && messages.length === 0 && (
          <p className="py-8 text-center text-sm text-muted-foreground">
            No messages yet — say hello.
          </p>
        )}
        {messages.length > 0 && (
          <Virtuoso
            ref={virtuosoRef}
            // `key` resets all virtualization state (scroll pos, measured heights) when
            // switching conversations — equivalent to the old `[messages.length, key]` reset.
            key={key}
            data={messages}
            className="h-full py-4"
            // Start pinned to the newest message (chat opens at the bottom).
            initialTopMostItemIndex={messages.length - 1}
            // Stick to the bottom on new messages only while the user is already there
            // (preserves scroll position when reading history / prepending older items).
            followOutput={(isAtBottom) => (isAtBottom ? "auto" : false)}
            atBottomStateChange={(b) => setShowJump(!b)}
            // A little tolerance so "at bottom" isn't lost to sub-pixel rounding.
            atBottomThreshold={48}
            increaseViewportBy={400}
            itemContent={(i, m) => {
              const prev = messages[i - 1];
              const showAuthor =
                !prev || prev.who !== m.who || prev.fromMe !== m.fromMe;
              const parent = m.replyTo ? (byId.get(m.replyTo) ?? null) : null;
              return (
                <div className="px-0 pb-0.5">
                  <MessageBubble
                    m={m}
                    parent={parent}
                    showAuthor={showAuthor}
                    reactions={m.id ? (reactionsByTarget.get(m.id) ?? []) : []}
                    selfReactionId={selfReactionId}
                    myName={myName}
                    onReply={setReplyTo}
                    onReact={toggleReaction}
                  />
                </div>
              );
            }}
            // Stable per-row identity so reactions/edits don't remount unrelated rows.
            computeItemKey={(i, m) => m.id ?? m.clientId ?? `pending-${i}`}
          />
        )}
        {showJump && (
          <button
            type="button"
            aria-label="Jump to latest messages"
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

      <TransferBar transferKey={active.id} label="file" />

      <Composer
        placeholder={
          isChannel ? `Message #${active.name}` : `Message ${active.name}`
        }
        mentionNames={mentionNames}
        replyTo={replyTo}
        onCancelReply={() => setReplyTo(null)}
        onAttach={async () => {
          try {
            const path = await openFileDialog({ multiple: false });
            if (typeof path === "string") await sendFile(path);
          } catch (e) {
            setError(`Couldn't open file: ${errorMessage(e)}`);
          }
        }}
        onSend={(t) => {
          send(t, replyTo?.id ?? null);
          setReplyTo(null);
        }}
      />
    </main>
  );
}
