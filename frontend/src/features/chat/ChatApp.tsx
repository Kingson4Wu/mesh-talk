import { Sidebar } from "./Sidebar";
import { ConversationView } from "./ConversationView";
import { ChatErrorToast } from "./ChatErrorToast";
import { useChatRuntime } from "./useChatRuntime";

/** The DESKTOP chat shell: the two-pane layout (conversation list + open conversation). Phone-width
 * viewports render `MobileApp` instead (chosen in App.tsx), so this has no viewport branches.
 * Boot/presence/gateway-sync live in the shared `useChatRuntime` hook. */
export function ChatApp() {
  const { error, clearError } = useChatRuntime();

  return (
    <div
      data-testid="chat-shell"
      className="relative flex h-full overflow-hidden"
    >
      <Sidebar />
      <ConversationView />
      <ChatErrorToast error={error} clearError={clearError} />
    </div>
  );
}
