import { useChatRuntime } from "@/features/chat/useChatRuntime";
import { ChatErrorToast } from "@/features/chat/ChatErrorToast";
import { useChat } from "@/store/chat";
import { MobileConversationList } from "./MobileConversationList";
import { MobileConversationScreen } from "./MobileConversationScreen";

/** The phone shell: a single-pane app. Shows the conversation list, or the open conversation when
 * one is active (the store's `active` drives nav; the screen's back button clears it). Shares the
 * boot/presence/gateway-sync runtime with the desktop shell via useChatRuntime. */
export function MobileApp() {
  const { error, clearError } = useChatRuntime();
  const active = useChat((s) => s.active);

  return (
    <div
      data-testid="chat-shell"
      className="relative flex h-full flex-col overflow-hidden"
    >
      {active ? (
        <MobileConversationScreen active={active} />
      ) : (
        <MobileConversationList />
      )}
      <ChatErrorToast error={error} clearError={clearError} />
    </div>
  );
}
