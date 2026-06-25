import { useEffect, useMemo, useState } from "react";
import { ChevronLeft } from "lucide-react";
import { useTranslation } from "react-i18next";
import { Composer } from "@/features/chat/Composer";
import { ConversationHeader } from "@/features/chat/ConversationHeader";
import { MessageStream } from "@/features/chat/MessageStream";
import { MembersDialog } from "@/features/chat/MembersDialog";
import { VerifyContactDialog } from "@/features/chat/VerifyContactDialog";
import { TransferBar } from "@/features/chat/TransferBar";
import {
  convKey,
  displayName,
  useChat,
  type ChatMessage,
  type Conversation,
} from "@/store/chat";

/** The phone conversation screen: a back button + the shared ConversationHeader + MessageStream +
 * Composer. Text/reply only — native file attach + screenshot are desktop-only (the browser file
 * fallback is a separate change). */
export function MobileConversationScreen({ active }: { active: Conversation }) {
  const { t } = useTranslation();
  const favorites = useChat((s) => s.favorites);
  const send = useChat((s) => s.send);
  const members = useChat((s) => s.members);
  const key = convKey(active);

  const [replyTo, setReplyTo] = useState<ChatMessage | null>(null);
  useEffect(() => {
    setReplyTo(null);
  }, [key]);

  const isChannel = active.kind === "channel";
  const headerName = displayName(favorites, active.id, active.name);
  const mentionNames = useMemo(
    () =>
      isChannel ? members.map((m) => m.name).filter(Boolean) : [active.name],
    [isChannel, members, active.name],
  );

  return (
    <main className="flex h-full min-w-0 flex-col">
      <header
        data-testid="conversation-header"
        className="flex items-center gap-2 border-b px-3 py-3"
      >
        <button
          type="button"
          onClick={() => useChat.setState({ active: null })}
          aria-label={t("conversation.back")}
          data-testid="conversation-back"
          className="-ml-1 shrink-0 rounded-md p-1.5 text-muted-foreground hover:bg-muted hover:text-foreground"
        >
          <ChevronLeft className="h-5 w-5" />
        </button>
        <ConversationHeader
          active={active}
          name={headerName}
          memberCount={members.length}
        />
        <div className="flex-1" />
        {isChannel ? (
          <MembersDialog />
        ) : (
          active.kind === "account" && (
            <VerifyContactDialog accountId={active.id} name={headerName} />
          )
        )}
      </header>

      <MessageStream
        active={active}
        headerName={headerName}
        onReply={setReplyTo}
      />

      <TransferBar transferKey={active.id} />

      <Composer
        placeholder={
          isChannel
            ? t("conversation.messageChannel", { name: headerName })
            : t("conversation.messageContact", { name: headerName })
        }
        mentionNames={mentionNames}
        replyTo={replyTo}
        onCancelReply={() => setReplyTo(null)}
        onSend={(text) => {
          send(text, replyTo?.id ?? null);
          setReplyTo(null);
        }}
      />
    </main>
  );
}
