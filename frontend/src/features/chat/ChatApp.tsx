import { useEffect } from "react";
import { Sidebar } from "./Sidebar";
import { ConversationView } from "./ConversationView";
import { useChat } from "@/store/chat";

export function ChatApp() {
  const start = useChat((s) => s.start);

  useEffect(() => {
    const stop = start();
    return stop;
  }, [start]);

  return (
    <div className="flex h-full overflow-hidden">
      <Sidebar />
      <ConversationView />
    </div>
  );
}
