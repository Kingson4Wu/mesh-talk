import { useEffect, useMemo, useRef, useState } from "react";
import { open as openFileDialog } from "@tauri-apps/plugin-dialog";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { Loader2, MessagesSquare, Upload } from "lucide-react";
import { useTranslation } from "react-i18next";
import { useTheme } from "@/lib/theme";
import { THEME_CREST } from "@/lib/themeCrest";
import { Composer } from "./Composer";
import { MembersDialog } from "./MembersDialog";
import { VerifyContactDialog } from "./VerifyContactDialog";
import { ConversationHistoryDialog } from "./ConversationHistoryDialog";
import { TransferBar } from "./TransferBar";
import { MessageStream } from "./MessageStream";
import { ConversationHeader } from "./ConversationHeader";
import { chat as chatApi } from "@/lib/api";
import { isTauri } from "@/lib/backend";
import { errorMessage } from "@/lib/error";
import { convKey, displayName, useChat, type ChatMessage } from "@/store/chat";

function EmptyState() {
  const { t } = useTranslation();
  // On a brand theme, greet with that crest/emblem instead of the generic chat glyph.
  const crest = THEME_CREST[useTheme((s) => s.theme)];
  return (
    <div className="flex flex-1 flex-col items-center justify-center gap-4 text-center">
      <div className="relative">
        <div
          aria-hidden
          className="absolute inset-0 -z-10 rounded-2xl bg-signal/15 blur-2xl"
        />
        {crest ? (
          <img
            src={crest}
            alt=""
            data-testid="empty-crest"
            className="h-20 w-20 object-contain drop-shadow-lg"
          />
        ) : (
          <div className="flex h-16 w-16 items-center justify-center rounded-2xl border bg-card text-signal shadow-elevation">
            <MessagesSquare className="h-8 w-8" />
          </div>
        )}
      </div>
      <div>
        <p className="font-display text-lg font-semibold tracking-tight">
          {t("conversation.noneSelectedTitle")}
        </p>
        <p className="text-sm text-muted-foreground">
          {t("conversation.noneSelectedDesc")}
        </p>
      </div>
    </div>
  );
}

// Shown on first boot while the node's encrypted stores are still being unlocked in the
// background (the heavy argon2 KDF + store opens). The login invoke already returned, so this is a
// non-blocking "two-phase startup" state — not a frozen window.
function UnlockingState() {
  const { t } = useTranslation();
  return (
    <div className="flex flex-1 flex-col items-center justify-center gap-4 text-center">
      <div className="relative flex h-16 w-16 items-center justify-center">
        <span
          aria-hidden
          className="absolute inset-0 animate-ping rounded-full bg-signal/20"
        />
        <Loader2 className="h-7 w-7 animate-spin text-signal" />
      </div>
      <div>
        <p className="font-display text-lg font-semibold tracking-tight">
          {t("conversation.unlockingTitle")}
        </p>
        <p className="text-sm text-muted-foreground">
          {t("conversation.unlockingDesc")}
        </p>
      </div>
    </div>
  );
}

/** Desktop conversation pane: header + the shared MessageStream + TransferBar + Composer, with the
 * desktop-only Tauri drag-drop + file/screenshot wiring. The mobile shell has its own screen
 * (MobileConversationScreen) reusing MessageStream/ConversationHeader. */
export function ConversationView() {
  const { t } = useTranslation();
  const active = useChat((s) => s.active);
  const favorites = useChat((s) => s.favorites);
  const send = useChat((s) => s.send);
  const sendFile = useChat((s) => s.sendFile);
  const setError = useChat((s) => s.setError);
  const members = useChat((s) => s.members);
  const ready = useChat((s) => s.ready);
  const key = active ? convKey(active) : "";

  // Native (Tauri) drag-and-drop: dropping file(s) onto the conversation sends them there. The
  // webview drag-drop event is global, so we subscribe once and read the latest active
  // conversation / sendFile through refs (avoids re-subscribing on every change).
  const [dragOver, setDragOver] = useState(false);
  const activeRef = useRef(active);
  const sendFileRef = useRef(sendFile);
  const setErrorRef = useRef(setError);
  activeRef.current = active;
  sendFileRef.current = sendFile;
  setErrorRef.current = setError;
  useEffect(() => {
    if (!isTauri()) return; // drag-drop is a Tauri webview feature; no-op in the browser PWA
    const unlisten = getCurrentWebview().onDragDropEvent((event) => {
      const p = event.payload;
      if (p.type === "over") {
        setDragOver(activeRef.current != null);
      } else if (p.type === "leave") {
        setDragOver(false);
      } else if (p.type === "drop") {
        setDragOver(false);
        if (!activeRef.current) return;
        void (async () => {
          for (const path of p.paths) {
            try {
              await sendFileRef.current(path, false);
            } catch (e) {
              setErrorRef.current(
                t("composer.couldntOpenFile", { error: errorMessage(e) }),
              );
            }
          }
        })();
      }
    });
    return () => {
      void unlisten.then((fn) => fn());
    };
  }, [t]);

  const [replyTo, setReplyTo] = useState<ChatMessage | null>(null);
  useEffect(() => {
    setReplyTo(null);
  }, [key]);

  const mentionNames = useMemo(
    () =>
      active?.kind === "channel"
        ? members.map((m) => m.name).filter(Boolean)
        : active
          ? [active.name]
          : [],
    [active, members],
  );

  // Route picked/pasted media bytes through the normal file-send pipeline: write to a temp file,
  // then send the path. `name` (the picker has one) is kept so the real filename + extension
  // survive to the chat list and the inline preview.
  const sendImageBytes = async (
    bytes: Uint8Array,
    ext: string,
    name?: string,
  ) => {
    try {
      const path = await chatApi.writeTempFile(Array.from(bytes), ext, name);
      await sendFile(path, true);
    } catch (e) {
      setError(t("composer.couldntOpenFile", { error: errorMessage(e) }));
    }
  };

  if (!active) {
    return (
      <main className="flex min-w-0 flex-1 flex-col">
        {ready ? <EmptyState /> : <UnlockingState />}
      </main>
    );
  }

  const isChannel = active.kind === "channel";
  // Alias (if set) overrides the conversation's announced name in the header + composer.
  const headerName = displayName(favorites, active.id, active.name);

  return (
    // min-w-0 lets this flex child shrink below its content's intrinsic width, so a wide
    // message/image wraps within the pane instead of pushing it past the window edge.
    <main className="relative flex min-w-0 flex-1 flex-col">
      {dragOver && (
        <div className="pointer-events-none absolute inset-0 z-30 flex flex-col items-center justify-center gap-2 bg-signal/10 backdrop-blur-sm">
          <div className="flex flex-col items-center gap-2 rounded-2xl border-2 border-dashed border-signal bg-background/90 px-8 py-6 shadow-elevation-lg">
            <Upload className="h-8 w-8 text-signal" />
            <p className="text-sm font-medium">
              {t("conversation.dropToSend", { name: headerName })}
            </p>
          </div>
        </div>
      )}
      <header
        data-testid="conversation-header"
        data-tauri-drag-region
        data-titlebar-inset
        className="flex items-center gap-3 border-b px-5 py-3"
      >
        <ConversationHeader
          active={active}
          name={headerName}
          memberCount={members.length}
        />
        <div className="flex-1" />
        <ConversationHistoryDialog
          conversation={{ ...active, name: headerName }}
        />
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
        onAttach={async () => {
          try {
            const path = await openFileDialog({ multiple: false });
            if (typeof path === "string") await sendFile(path, false);
          } catch (e) {
            setError(t("composer.couldntOpenFile", { error: errorMessage(e) }));
          }
        }}
        onSend={(text) => {
          send(text, replyTo?.id ?? null);
          setReplyTo(null);
        }}
        onPasteImage={sendImageBytes}
        onScreenshot={async (hideWindow) => {
          try {
            const bytes = await chatApi.captureScreen(hideWindow);
            if (bytes.length > 0) await sendImageBytes(bytes, "png");
          } catch (e) {
            const msg = errorMessage(e);
            setError(
              msg.includes("screen-recording-permission")
                ? t("screenshot.permission")
                : t("screenshot.failed", { error: msg }),
            );
          }
        }}
      />
    </main>
  );
}
