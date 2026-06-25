import { useEffect } from "react";
import { chat as chatApi } from "@/lib/api";
import { isTauri } from "@/lib/backend";
import {
  applyUrlGatewayConfig,
  startConfiguredGatewaySync,
} from "@/lib/browserBackend";
import { useChat } from "@/store/chat";
import { usePresence } from "@/store/presence";

/**
 * The chat shell's runtime: the boot effects every shell (desktop two-pane AND the mobile
 * single-pane) needs, plus the render-facing error state. Lives here — not in a component — so
 * both shells share ONE copy and can't drift (no per-shell duplication of start/presence/sync).
 *
 * Returns the error banner state; the shells differ only in LAYOUT, not in this wiring.
 */
export function useChatRuntime() {
  const start = useChat((s) => s.start);
  const startPresence = usePresence((s) => s.start);
  const error = useChat((s) => s.error);
  const clearError = useChat((s) => s.clearError);
  // Total unread across all conversations → the OS app-icon badge (dock/taskbar count). A
  // primitive sum keeps the selector stable (re-renders only when the total changes).
  const totalUnread = useChat((s) =>
    Object.values(s.unread).reduce((a, b) => a + b, 0),
  );

  useEffect(() => {
    chatApi.setBadge(totalUnread).catch(() => {});
  }, [totalUnread]);

  useEffect(() => {
    const stop = start();
    return stop;
  }, [start]);

  // Presence polls on its own slow interval into an isolated store — kept apart from the chat
  // store so a presence tick never re-renders the virtualized message list.
  useEffect(() => {
    const stop = startPresence();
    return stop;
  }, [startPresence]);

  // Browser/PWA only (orthogonal to viewport): if a gateway relay is configured, continuously
  // sync with peers in the background and refresh the open conversation as messages arrive.
  // No-op on desktop (mesh peers come from LAN discovery) and when no relay is set. Runs on the
  // mobile shell too — that's how a phone meshes — so it MUST live in this shared hook.
  useEffect(() => {
    if (isTauri()) return;
    // A scanned QR opens the app at .../?relay=…&room=… — adopt that gateway before the loop.
    applyUrlGatewayConfig();
    const stop = startConfiguredGatewaySync(() => {
      void useChat.getState().refreshRoster();
      void useChat.getState().reload();
    });
    return stop;
  }, []);

  return { error, clearError };
}
