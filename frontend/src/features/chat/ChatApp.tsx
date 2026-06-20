import { useEffect } from "react";
import { Sidebar } from "./Sidebar";
import { ConversationView } from "./ConversationView";
import { useChat } from "@/store/chat";

export function ChatApp() {
  const start = useChat((s) => s.start);
  const error = useChat((s) => s.error);
  const clearError = useChat((s) => s.clearError);

  useEffect(() => {
    const stop = start();
    return stop;
  }, [start]);

  return (
    <div className="relative flex h-full overflow-hidden">
      <Sidebar />
      <ConversationView />
      {error && (
        <div
          role="alert"
          className="absolute bottom-4 left-1/2 z-50 flex -translate-x-1/2 items-center gap-3 rounded-md bg-destructive px-4 py-2 text-sm text-destructive-foreground shadow-lg"
        >
          <span>{error}</span>
          <button
            type="button"
            onClick={clearError}
            aria-label="Dismiss error"
            className="font-bold opacity-80 hover:opacity-100"
          >
            ×
          </button>
        </div>
      )}
    </div>
  );
}
