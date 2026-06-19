import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type {
  ChannelMessageEvent,
  DmReceivedEvent,
  FileReceivedEvent,
} from "./types";

/** Subscribe to the node's inbound events. Returns a single unlisten function. */
export function subscribeNodeEvents(handlers: {
  onDm?: (e: DmReceivedEvent) => void;
  onChannelMessage?: (e: ChannelMessageEvent) => void;
  onFile?: (e: FileReceivedEvent) => void;
}): () => void {
  const unlisteners: Promise<UnlistenFn>[] = [];
  if (handlers.onDm) {
    unlisteners.push(
      listen<DmReceivedEvent>("dm-received", (e) => handlers.onDm!(e.payload)),
    );
  }
  if (handlers.onChannelMessage) {
    unlisteners.push(
      listen<ChannelMessageEvent>("channel-message", (e) =>
        handlers.onChannelMessage!(e.payload),
      ),
    );
  }
  if (handlers.onFile) {
    unlisteners.push(
      listen<FileReceivedEvent>("file-received", (e) =>
        handlers.onFile!(e.payload),
      ),
    );
  }
  return () => {
    for (const u of unlisteners) u.then((fn) => fn()).catch(() => {});
  };
}
