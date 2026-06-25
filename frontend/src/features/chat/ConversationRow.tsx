import { useTranslation } from "react-i18next";
import { Pencil, Pin, PinOff } from "lucide-react";
import { Badge } from "@/components/ui/badge";
import { GroupAvatar } from "@/components/GroupAvatar";
import { IdentityGlyph, PresenceDot } from "@/components/identity";
import { cn } from "@/lib/utils";
import { convKey, useChat, type Conversation } from "@/store/chat";
import {
  presenceLabel,
  presenceStatus,
  usePresenceFor,
} from "@/store/presence";
import type { AccountInfo, ChannelInfo } from "@/lib/types";

/** A conversation as the chat list renders it — shared by the desktop sidebar + the mobile list. */
export function accountConv(a: AccountInfo, name: string): Conversation {
  return { kind: "account", id: a.account_id, name };
}
export function channelConv(c: ChannelInfo, name: string): Conversation {
  return { kind: "channel", id: c.channel_id, name };
}

/**
 * One row in the conversation list (glyph + name + subtitle + presence + unread + rename/pin).
 * Shared by the desktop Sidebar and the mobile list so the `conversation-row-${id}` /
 * `conversation-pin-${id}` testid contract + behaviour live in ONE place. Hover-reveal controls
 * stay visible on touch via the `[@media(hover:hover)]` variants.
 */
export function ConversationRow({
  conv,
  subtitle,
  channel,
  pinned,
  onTogglePin,
  onRename,
}: {
  conv: Conversation;
  subtitle: string;
  channel?: boolean;
  pinned: boolean;
  onTogglePin: () => void;
  onRename: () => void;
}) {
  const { t } = useTranslation();
  const active = useChat((s) => s.active);
  const unread = useChat((s) => s.unread[convKey(conv)] ?? 0);
  const open = useChat((s) => s.open);
  const isActive = active != null && convKey(active) === convKey(conv);
  // Presence is read from the isolated store and keyed by id, so a presence tick only re-renders
  // the rows whose snapshot actually changed.
  const presence = usePresenceFor(conv.id);
  const status = presenceStatus(presence);

  return (
    <div
      role="listitem"
      className={cn(
        "group relative flex w-full items-center gap-3 rounded-lg px-2.5 py-2 text-left transition-colors duration-150 ease-out",
        isActive ? "bg-accent" : "hover:bg-accent/60",
      )}
    >
      {/* Teal active rail. */}
      {isActive && (
        <span
          aria-hidden
          className="absolute inset-y-1.5 left-0 w-0.5 rounded-full bg-signal"
        />
      )}
      <button
        onClick={() => open(conv)}
        data-conv-option
        data-testid={`conversation-row-${conv.id}`}
        aria-current={isActive ? "true" : undefined}
        aria-label={`${conv.name}${subtitle ? `, ${subtitle}` : ""}`}
        className="flex min-w-0 flex-1 items-center gap-3 rounded-md text-left outline-none focus-visible:ring-2 focus-visible:ring-ring"
      >
        <div className="relative">
          {channel ? (
            <GroupAvatar channelId={conv.id} size={36} title={conv.name} />
          ) : (
            <IdentityGlyph seed={conv.id} size={36} title={conv.name} />
          )}
          {/* Presence overlays the glyph — the signature living-LAN cue. */}
          {!channel && (
            <PresenceDot
              status={status}
              size="md"
              label={presenceLabel(presence, t)}
              className="absolute -bottom-0.5 -right-0.5"
            />
          )}
        </div>
        <div className="min-w-0 flex-1">
          <div className="truncate font-display text-sm font-medium tracking-tight">
            {conv.name}
          </div>
          <div className="truncate font-mono text-xs text-muted-foreground">
            {subtitle}
          </div>
        </div>
      </button>
      {unread > 0 && (
        <Badge className="bg-signal font-mono text-[11px] text-primary-foreground">
          {unread}
        </Badge>
      )}
      <button
        type="button"
        onClick={onRename}
        title={t("sidebar.rename")}
        aria-label={t("sidebar.rename")}
        className="rounded-md p-1 text-muted-foreground transition-colors hover:bg-accent hover:text-foreground [@media(hover:hover)]:hidden [@media(hover:hover)]:group-hover:block"
      >
        <Pencil className="h-3.5 w-3.5" />
      </button>
      <button
        type="button"
        onClick={onTogglePin}
        data-testid={`conversation-pin-${conv.id}`}
        title={pinned ? t("sidebar.unpin") : t("sidebar.pin")}
        aria-label={pinned ? t("sidebar.unpin") : t("sidebar.pin")}
        className={cn(
          "rounded-md p-1 transition-colors hover:bg-accent",
          pinned
            ? "text-signal"
            : "text-muted-foreground hover:text-foreground [@media(hover:hover)]:hidden [@media(hover:hover)]:group-hover:block",
        )}
      >
        {pinned ? (
          <PinOff className="h-3.5 w-3.5" />
        ) : (
          <Pin className="h-3.5 w-3.5" />
        )}
      </button>
    </div>
  );
}
