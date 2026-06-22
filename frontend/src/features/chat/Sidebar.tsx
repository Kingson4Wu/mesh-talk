import { useMemo, useRef, useState } from "react";
import { LogOut, Moon, Network, Pencil, Pin, PinOff, Sun } from "lucide-react";
import { useTranslation } from "react-i18next";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { IdentityGlyph, PresenceDot } from "@/components/identity";
import { cn } from "@/lib/utils";
import { shortId } from "@/lib/format";
import { useTheme } from "@/lib/theme";
import { CreateChannelDialog } from "./CreateChannelDialog";
import { SearchDialog } from "./SearchDialog";
import { FilesTray } from "./FilesTray";
import { LinkDeviceDialog } from "./LinkDeviceDialog";
import { DiagnosticsDialog } from "./DiagnosticsDialog";
import { SettingsDialog } from "./SettingsDialog";
import { AboutDialog } from "./AboutDialog";
import { useAuth } from "@/store/auth";
import { convKey, useChat, type Conversation } from "@/store/chat";
import {
  presenceLabel,
  presenceStatus,
  usePresenceFor,
} from "@/store/presence";
import type { AccountInfo, ChannelInfo } from "@/lib/types";

function accountConv(a: AccountInfo, name: string): Conversation {
  return {
    kind: "account",
    id: a.account_id,
    name,
  };
}
function channelConv(c: ChannelInfo, name: string): Conversation {
  return { kind: "channel", id: c.channel_id, name };
}

/** A compact mesh/group sigil for channels (distinct from a person's IdentityGlyph). */
function ChannelGlyph({ size = 36 }: { size?: number }) {
  return (
    <div
      className="flex shrink-0 items-center justify-center rounded-[28%] border border-border bg-secondary text-muted-foreground"
      style={{ width: size, height: size }}
      aria-hidden
    >
      <Network style={{ width: size * 0.5, height: size * 0.5 }} />
    </div>
  );
}

function Row({
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
  // Presence is read from the isolated store and keyed by id, so a presence tick only
  // re-renders the rows whose snapshot actually changed.
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
            <ChannelGlyph />
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
        className="hidden rounded-md p-1 text-muted-foreground transition-colors hover:bg-accent hover:text-foreground group-hover:block"
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
            : "hidden text-muted-foreground hover:text-foreground group-hover:block",
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

function SectionLabel({
  children,
  action,
}: {
  children: React.ReactNode;
  action?: React.ReactNode;
}) {
  return (
    <div className="flex items-center justify-between px-2.5 pb-1 pt-4 text-xs font-semibold uppercase tracking-wider text-muted-foreground">
      <span>{children}</span>
      {action}
    </div>
  );
}

export function Sidebar() {
  const { t } = useTranslation();
  const accounts = useChat((s) => s.accounts);
  const channels = useChat((s) => s.channels);
  const favorites = useChat((s) => s.favorites);
  const togglePinned = useChat((s) => s.togglePinned);
  const setAlias = useChat((s) => s.setAlias);
  const myId = useChat((s) => s.myId);
  const myAccountId = useChat((s) => s.myAccountId);
  const ready = useChat((s) => s.ready);
  const bootFailed = useChat((s) => s.bootFailed);
  const retryBoot = useChat((s) => s.retryBoot);
  const username = useAuth((s) => s.user?.username ?? "");
  const logout = useAuth((s) => s.logout);
  const theme = useTheme((s) => s.theme);
  const toggleTheme = useTheme((s) => s.toggle);

  // Inline rename dialog state (a contact id + a draft alias). Kept local/primitive.
  const [renameId, setRenameId] = useState<string | null>(null);
  const [renameDraft, setRenameDraft] = useState("");

  // Roving keyboard navigation across the conversation rows. Arrow Up/Down moves focus
  // between the option buttons within the list (Enter/Space already open via the native
  // button). Scoped to the nav so it never traps the composer or other controls.
  const navRef = useRef<HTMLElement>(null);
  const onNavKeyDown = (e: React.KeyboardEvent) => {
    if (e.key !== "ArrowDown" && e.key !== "ArrowUp") return;
    const nav = navRef.current;
    if (!nav) return;
    const options = Array.from(
      nav.querySelectorAll<HTMLButtonElement>("[data-conv-option]"),
    );
    if (options.length === 0) return;
    const idx = options.indexOf(document.activeElement as HTMLButtonElement);
    e.preventDefault();
    const next =
      e.key === "ArrowDown"
        ? idx < 0
          ? 0
          : Math.min(idx + 1, options.length - 1)
        : idx <= 0
          ? 0
          : idx - 1;
    options[next]?.focus();
  };

  const startRename = (id: string, current: string) => {
    setRenameId(id);
    setRenameDraft(favorites[id]?.custom_alias ?? current);
  };
  const commitRename = () => {
    if (renameId) void setAlias(renameId, renameDraft);
    setRenameId(null);
  };

  // Resolve the displayed name (alias overrides the announced name) and split into
  // pinned vs the rest. Sort is stable on the source order within each group. Memoized so
  // the map + four partition passes don't re-run on every render (the sidebar re-renders
  // on the 4s roster refresh and on every favorites change).
  const {
    pinnedAccounts,
    unpinnedAccounts,
    pinnedChannels,
    unpinnedChannels,
    hasPinned,
  } = useMemo(() => {
    const accountRows = accounts.map((a) => {
      const id = a.account_id;
      const announced = a.names[0] || shortId(id);
      const name = favorites[id]?.custom_alias || announced;
      return {
        a,
        id,
        conv: accountConv(a, name),
        pinned: favorites[id]?.pinned ?? false,
        subtitle:
          a.device_count > 1
            ? t("sidebar.devices", { count: a.device_count })
            : shortId(id, 12),
      };
    });
    const channelRows = channels.map((c) => {
      const id = c.channel_id;
      const name = favorites[id]?.custom_alias || c.name;
      return {
        c,
        id,
        conv: channelConv(c, name),
        pinned: favorites[id]?.pinned ?? false,
        subtitle: t("sidebar.memberCount", { count: c.member_count }),
      };
    });

    const pinnedAccounts = accountRows.filter((r) => r.pinned);
    const unpinnedAccounts = accountRows.filter((r) => !r.pinned);
    const pinnedChannels = channelRows.filter((r) => r.pinned);
    const unpinnedChannels = channelRows.filter((r) => !r.pinned);
    return {
      pinnedAccounts,
      unpinnedAccounts,
      pinnedChannels,
      unpinnedChannels,
      hasPinned: pinnedAccounts.length + pinnedChannels.length > 0,
    };
  }, [accounts, channels, favorites, t]);

  return (
    <aside
      data-testid="sidebar"
      className="flex w-72 shrink-0 flex-col border-r bg-card/40"
    >
      {/* Identity header — own glyph + name (display) + own short mono id, then a tidy
          action cluster. The signature is the identity; the controls stay quiet. */}
      <div className="border-b px-4 py-3" data-testid="self-identity">
        <div className="flex items-center gap-3">
          <div className="relative shrink-0">
            <IdentityGlyph
              seed={myAccountId || myId || username}
              size={38}
              title={username}
            />
            <PresenceDot
              status={ready ? "online" : "offline"}
              size="md"
              label={ready ? t("presence.online") : t("common.starting")}
              className="absolute -bottom-0.5 -right-0.5"
            />
          </div>
          <div className="min-w-0 flex-1">
            <div className="truncate font-display text-sm font-semibold tracking-tight">
              {username}
            </div>
            <div className="truncate font-mono text-xs text-muted-foreground">
              {ready ? (
                t("sidebar.you", { id: shortId(myId) })
              ) : bootFailed ? (
                <button
                  type="button"
                  onClick={() => retryBoot()}
                  className="text-destructive underline-offset-2 hover:underline"
                >
                  {t("sidebar.nodeUnavailable")}
                </button>
              ) : (
                t("common.starting")
              )}
            </div>
          </div>
        </div>
        <div className="mt-2.5 flex items-center gap-0.5">
          <SearchDialog />
          <FilesTray />
          <LinkDeviceDialog />
          <DiagnosticsDialog />
          <SettingsDialog />
          <AboutDialog />
          <div className="flex-1" />
          <Button
            variant="ghost"
            size="icon"
            data-testid="sidebar-theme-toggle"
            title={
              theme === "light" ? t("sidebar.darkMode") : t("sidebar.lightMode")
            }
            aria-label={t("sidebar.toggleTheme")}
            onClick={() => toggleTheme()}
          >
            {theme === "light" ? (
              <Moon className="h-4 w-4" />
            ) : (
              <Sun className="h-4 w-4" />
            )}
          </Button>
          <Button
            variant="ghost"
            size="icon"
            data-testid="sidebar-sign-out"
            title={t("sidebar.signOut")}
            onClick={() => logout()}
          >
            <LogOut className="h-4 w-4" />
          </Button>
        </div>
      </div>

      <nav
        ref={navRef}
        role="list"
        aria-label={t("conversation.list")}
        onKeyDown={onNavKeyDown}
        className="flex-1 overflow-y-auto px-2 pb-4"
      >
        {hasPinned && (
          <>
            <SectionLabel>{t("sidebar.pinned")}</SectionLabel>
            {pinnedAccounts.map((r) => (
              <Row
                key={r.id}
                conv={r.conv}
                subtitle={r.subtitle}
                pinned
                onTogglePin={() => void togglePinned(r.id, false)}
                onRename={() =>
                  startRename(r.id, r.a.names[0] || shortId(r.id))
                }
              />
            ))}
            {pinnedChannels.map((r) => (
              <Row
                key={r.id}
                conv={r.conv}
                subtitle={r.subtitle}
                channel
                pinned
                onTogglePin={() => void togglePinned(r.id, false)}
                onRename={() => startRename(r.id, r.c.name)}
              />
            ))}
          </>
        )}

        <SectionLabel>{t("sidebar.directMessages")}</SectionLabel>
        {accounts.length === 0 && (
          <p className="px-2.5 py-2 text-xs text-muted-foreground">
            {t("sidebar.noContacts")}
          </p>
        )}
        {unpinnedAccounts.map((r) => (
          <Row
            key={r.id}
            conv={r.conv}
            subtitle={r.subtitle}
            pinned={false}
            onTogglePin={() => void togglePinned(r.id, true)}
            onRename={() => startRename(r.id, r.a.names[0] || shortId(r.id))}
          />
        ))}

        <SectionLabel action={<CreateChannelDialog />}>
          {t("sidebar.channels")}
        </SectionLabel>
        {channels.length === 0 && (
          <p className="px-2.5 py-2 text-xs text-muted-foreground">
            {t("sidebar.noChannels")}
          </p>
        )}
        {unpinnedChannels.map((r) => (
          <Row
            key={r.id}
            conv={r.conv}
            subtitle={r.subtitle}
            channel
            pinned={false}
            onTogglePin={() => void togglePinned(r.id, true)}
            onRename={() => startRename(r.id, r.c.name)}
          />
        ))}
      </nav>

      <div className="flex items-center gap-2 border-t px-4 py-2 text-xs text-muted-foreground">
        <Network className="h-3.5 w-3.5 text-signal" />
        {t("sidebar.contactsOnLan", { count: accounts.length })}
      </div>

      <Dialog
        open={renameId !== null}
        onOpenChange={(o) => !o && setRenameId(null)}
      >
        <DialogContent className="max-w-sm">
          <DialogHeader>
            <DialogTitle>{t("sidebar.renameTitle")}</DialogTitle>
          </DialogHeader>
          <Input
            autoFocus
            value={renameDraft}
            onChange={(e) => setRenameDraft(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && commitRename()}
            placeholder={t("sidebar.aliasPlaceholder")}
          />
          <div className="flex justify-end gap-2">
            <Button variant="ghost" onClick={() => setRenameId(null)}>
              {t("common.cancel")}
            </Button>
            <Button onClick={commitRename}>{t("common.save")}</Button>
          </div>
        </DialogContent>
      </Dialog>
    </aside>
  );
}
