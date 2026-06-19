import { useEffect, useMemo, useRef } from "react";
import { Hash, MessagesSquare } from "lucide-react";
import { Avatar } from "@/components/ui/avatar";
import { Composer } from "./Composer";
import { MessageBubble } from "./MessageBubble";
import { MembersDialog } from "./MembersDialog";
import { shortId } from "@/lib/format";
import { convKey, useChat, type ChatMessage } from "@/store/chat";

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

export function ConversationView() {
  const active = useChat((s) => s.active);
  const send = useChat((s) => s.send);
  const messages = useChat((s) =>
    active ? (s.messages[convKey(active)] ?? []) : [],
  );
  const loading = useChat((s) => s.loading);
  const members = useChat((s) => s.members);

  const scrollRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const el = scrollRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [messages.length, active]);

  const byId = useMemo(() => {
    const m = new Map<string, ChatMessage>();
    for (const msg of messages) if (msg.id) m.set(msg.id, msg);
    return m;
  }, [messages]);

  if (!active) {
    return (
      <main className="flex flex-1 flex-col">
        <EmptyState />
      </main>
    );
  }

  const isChannel = active.kind === "channel";

  return (
    <main className="flex flex-1 flex-col">
      {/* conversation header */}
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
        {isChannel && <MembersDialog />}
      </header>

      {/* messages */}
      <div ref={scrollRef} className="flex-1 space-y-1 overflow-y-auto py-4">
        {loading && messages.length === 0 && (
          <p className="py-8 text-center text-sm text-muted-foreground">Loading…</p>
        )}
        {!loading && messages.length === 0 && (
          <p className="py-8 text-center text-sm text-muted-foreground">
            No messages yet — say hello.
          </p>
        )}
        {messages.map((m, i) => {
          const prev = messages[i - 1];
          const showAuthor = !prev || prev.who !== m.who || prev.fromMe !== m.fromMe;
          const parent = m.replyTo ? (byId.get(m.replyTo) ?? null) : null;
          return (
            <MessageBubble
              key={m.id ?? `pending-${i}`}
              m={m}
              parent={parent}
              showAuthor={showAuthor}
            />
          );
        })}
      </div>

      <Composer
        placeholder={isChannel ? `Message #${active.name}` : `Message ${active.name}`}
        onSend={(t) => send(t, null)}
      />
    </main>
  );
}
