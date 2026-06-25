import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  LogOut,
  MoreHorizontal,
  Moon,
  Network,
  Wifi,
  WifiOff,
  Sun,
  X,
} from "lucide-react";
import { useTranslation } from "react-i18next";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from "@/components/ui/popover";
import {
  AvatarEditMenu,
  IdentityGlyph,
  PresenceDot,
} from "@/components/identity";
import { Logo } from "@/components/Logo";
import { cn } from "@/lib/utils";
import { shortId } from "@/lib/format";
import { useTheme } from "@/lib/theme";
import { THEME_CREST } from "@/lib/themeCrest";
import { CreateChannelDialog } from "./CreateChannelDialog";
import { SearchDialog } from "./SearchDialog";
import { FilesTray } from "./FilesTray";
import { LinkDeviceDialog } from "./LinkDeviceDialog";
import { DiagnosticsDialog } from "./DiagnosticsDialog";
import { OfflineConnectDialog } from "./OfflineConnectDialog";
import { SettingsDialog } from "./SettingsDialog";
import { AboutDialog } from "./AboutDialog";
import { useAuth } from "@/store/auth";
import { chat, diag } from "@/lib/api";
import { useChat } from "@/store/chat";
import { usePresence } from "@/store/presence";
import { ConversationRow } from "./ConversationRow";
import { useConversationList } from "./useConversationList";

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

// Sidebar width: user-resizable via the right-edge drag handle, persisted to localStorage,
// clamped to a sensible range, double-click to reset. Default mirrors the old `w-72`.
const WIDTH_KEY = "mesh-talk-sidebar-width";
const DEFAULT_WIDTH = 288; // w-72
const MIN_WIDTH = 230;
const MAX_WIDTH = 460;

const clampWidth = (w: number) =>
  Math.min(MAX_WIDTH, Math.max(MIN_WIDTH, Math.round(w)));

function readWidth(): number {
  if (typeof localStorage === "undefined") return DEFAULT_WIDTH;
  const v = Number(localStorage.getItem(WIDTH_KEY));
  return Number.isFinite(v) && v > 0 ? clampWidth(v) : DEFAULT_WIDTH;
}

function useSidebarWidth() {
  const [width, setWidth] = useState(readWidth);
  const dragging = useRef(false);

  const persist = useCallback((w: number) => {
    setWidth(w);
    if (typeof localStorage !== "undefined")
      localStorage.setItem(WIDTH_KEY, String(w));
  }, []);

  // Drag-to-resize: track pointer moves on the document so the cursor can leave the thin
  // handle without dropping the drag. Width = pointer x relative to the sidebar's left edge.
  const onPointerDown = useCallback(
    (e: React.PointerEvent) => {
      e.preventDefault();
      dragging.current = true;
      const aside = (e.currentTarget as HTMLElement).parentElement;
      const left = aside?.getBoundingClientRect().left ?? 0;
      const prevCursor = document.body.style.cursor;
      const prevSelect = document.body.style.userSelect;
      document.body.style.cursor = "col-resize";
      document.body.style.userSelect = "none";
      const onMove = (ev: PointerEvent) => {
        if (!dragging.current) return;
        persist(clampWidth(ev.clientX - left));
      };
      const onUp = () => {
        dragging.current = false;
        document.body.style.cursor = prevCursor;
        document.body.style.userSelect = prevSelect;
        window.removeEventListener("pointermove", onMove);
        window.removeEventListener("pointerup", onUp);
      };
      window.addEventListener("pointermove", onMove);
      window.addEventListener("pointerup", onUp);
    },
    [persist],
  );

  const reset = useCallback(() => persist(DEFAULT_WIDTH), [persist]);

  // Keyboard resize for accessibility (focus the handle, arrow to nudge).
  const onKeyDown = useCallback(
    (e: React.KeyboardEvent) => {
      if (e.key === "ArrowLeft") persist(clampWidth(width - 16));
      else if (e.key === "ArrowRight") persist(clampWidth(width + 16));
      else return;
      e.preventDefault();
    },
    [persist, width],
  );

  return { width, onPointerDown, reset, onKeyDown };
}

/** A tidy popover collecting the secondary/utility actions (files, link-device,
 *  diagnostics, settings, about, theme toggle, sign-out). Keeps the top clean; the
 *  primary Search stays up top. Each action keeps its existing `data-testid`. */
function UtilityMenu() {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  const theme = useTheme((s) => s.theme);
  const toggleTheme = useTheme((s) => s.toggle);
  const logout = useAuth((s) => s.logout);

  return (
    <Popover open={open} onOpenChange={setOpen}>
      <PopoverTrigger asChild>
        <Button
          variant="ghost"
          size="icon"
          data-testid="sidebar-overflow"
          title={t("sidebar.moreActions")}
          aria-label={t("sidebar.moreActions")}
        >
          <MoreHorizontal className="h-4 w-4" />
        </Button>
      </PopoverTrigger>
      <PopoverContent
        align="start"
        side="top"
        className="w-auto p-1.5"
        data-testid="sidebar-overflow-menu"
      >
        {/* The dialog/popover trigger buttons keep their own testids + render their own
            icon and open their dialog; collected here as a quiet utility toolbar. */}
        <div className="grid gap-0.5">
          <div className="flex items-center gap-0.5">
            <LinkDeviceDialog />
            <DiagnosticsDialog />
            <SettingsDialog />
            <AboutDialog />
          </div>
          <div className="my-1 h-px bg-border" />
          <button
            type="button"
            data-testid="sidebar-theme-toggle"
            onClick={() => toggleTheme()}
            className="flex w-full items-center gap-2.5 rounded-md px-2 py-1.5 text-left text-sm transition-colors hover:bg-accent"
          >
            {theme === "light" ? (
              <Moon className="h-4 w-4 text-muted-foreground" />
            ) : (
              <Sun className="h-4 w-4 text-muted-foreground" />
            )}
            <span>
              {theme === "light"
                ? t("sidebar.darkMode")
                : t("sidebar.lightMode")}
            </span>
          </button>
          <button
            type="button"
            data-testid="sidebar-sign-out"
            onClick={() => logout()}
            className="flex w-full items-center gap-2.5 rounded-md px-2 py-1.5 text-left text-sm text-destructive transition-colors hover:bg-destructive/10"
          >
            <LogOut className="h-4 w-4" />
            <span>{t("sidebar.signOut")}</span>
          </button>
        </div>
      </PopoverContent>
    </Popover>
  );
}

export function Sidebar() {
  const { t } = useTranslation();
  const accounts = useChat((s) => s.accounts);
  const channels = useChat((s) => s.channels);
  const peers = useChat((s) => s.peers);
  const favorites = useChat((s) => s.favorites);
  // The active theme's crest (a brand theme) replaces the app mark in the footer, so the
  // chosen club/national/Messi identity is always present without shouting.
  const themeCrest = THEME_CREST[useTheme((s) => s.theme)];
  const togglePinned = useChat((s) => s.togglePinned);
  const setAlias = useChat((s) => s.setAlias);
  const myId = useChat((s) => s.myId);
  const myAccountId = useChat((s) => s.myAccountId);
  const ready = useChat((s) => s.ready);
  // Online people on the LAN — distinct OTHER accounts that are presence-ONLINE (the same
  // signal the conversation-row dots use, so the count always matches the visible green dots).
  // Excludes post-office relays (infrastructure, not people) and our own other devices, and
  // ignores roster entries that are merely lingering (seen, but past the online window) so the
  // number doesn't drift from what's actually online.
  const presenceMap = usePresence((s) => s.map);
  const onlinePeople = useMemo(() => {
    const online = new Set<string>();
    for (const p of peers) {
      if (p.post_office) continue;
      const account = p.account_id ?? p.user_id;
      if (account === myAccountId) continue;
      if (presenceMap[account]?.online) online.add(account);
    }
    return online.size;
  }, [peers, presenceMap, myAccountId]);
  const bootFailed = useChat((s) => s.bootFailed);
  const retryBoot = useChat((s) => s.retryBoot);
  const username = useAuth((s) => s.user?.username ?? "");
  const { width, onPointerDown, reset, onKeyDown } = useSidebarWidth();

  // Inline rename dialog state (a contact id + a draft alias). Kept local/primitive.
  const [renameId, setRenameId] = useState<string | null>(null);
  const [renameDraft, setRenameDraft] = useState("");
  // The Wi-Fi network (SSID) we're on — Mesh-Talk is LAN-scoped, so show which network the
  // peers around us share. Polled (it can change when you switch networks); null when wired
  // or the OS withholds it.
  const [ssid, setSsid] = useState<string | null>(null);
  // Whether this device has no usable (non-loopback) network interface at all — the strongest
  // "you're stranded" signal, which sharpens the offline-connect prompt's wording.
  const [noNetwork, setNoNetwork] = useState(false);
  useEffect(() => {
    let alive = true;
    const refresh = () => {
      chat
        .networkName()
        .then((n) => alive && setSsid(n))
        .catch(() => {});
      diag
        .networkInfo()
        .then((i) => alive && setNoNetwork(i.interfaces.length === 0))
        .catch(() => {});
    };
    refresh();
    const id = setInterval(refresh, 60_000);
    return () => {
      alive = false;
      clearInterval(id);
    };
  }, []);

  // "Offline direct connect" prompt — surfaces only when stranded: the node is up but, after a
  // grace period (discovery's startup burst gets a fair chance), still nobody is online around
  // us. Dismissible per session; also always reachable from the diagnostics Troubleshoot tab.
  const [offlineOpen, setOfflineOpen] = useState(false);
  const [strandedDismissed, setStrandedDismissed] = useState(false);
  const [graceElapsed, setGraceElapsed] = useState(false);
  useEffect(() => {
    if (!ready) return;
    const id = setTimeout(() => setGraceElapsed(true), 20_000);
    return () => clearTimeout(id);
  }, [ready]);
  const stranded =
    ready && graceElapsed && onlinePeople === 0 && !strandedDismissed;

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

  // The conversation rows (pinned vs the rest) come from the shared hook, so the desktop sidebar
  // and the mobile list show the SAME conversations. Pin/rename UI stays here.
  const {
    pinnedAccounts,
    unpinnedAccounts,
    pinnedChannels,
    unpinnedChannels,
    hasPinned,
  } = useConversationList();

  return (
    <aside
      data-testid="sidebar"
      style={{ width }}
      className={cn("relative flex shrink-0 flex-col border-r bg-card/40")}
    >
      {/* Identity header — own glyph + name (display) + own short mono id, plus the one
          primary action (Search). `data-tauri-drag-region` makes the strip a window-drag
          handle (interactive controls inside opt out via their own pointer handling);
          `data-titlebar-inset` clears the macOS traffic-lights (no-op elsewhere). */}
      <div
        className="border-b px-4 py-3"
        data-testid="self-identity"
        data-tauri-drag-region
        data-titlebar-inset="left"
      >
        <div className="flex items-center gap-3">
          <div className="relative shrink-0">
            <AvatarEditMenu
              id={myAccountId || myId || username}
              ariaLabel={t("avatar.editOwn")}
              pack="players"
            >
              <IdentityGlyph
                seed={myAccountId || myId || username}
                size={38}
                title={username}
              />
            </AvatarEditMenu>
            <PresenceDot
              status={ready ? "online" : "offline"}
              size="md"
              label={ready ? t("presence.online") : t("common.starting")}
              className="pointer-events-none absolute -bottom-0.5 -right-0.5"
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
        {/* Primary actions up top: Search + Received files. Other utilities live in the
            bottom-left overflow menu. */}
        <div className="mt-2.5 flex items-center gap-0.5">
          <SearchDialog />
          <FilesTray />
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
              <ConversationRow
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
              <ConversationRow
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
          <ConversationRow
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
          <ConversationRow
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

      {/* Stranded prompt: nobody's online and the grace window has passed — offer the
          offline direct-connect guide. Quiet, dismissible, and only here when it's earned. */}
      {stranded && (
        <div
          data-testid="stranded-prompt"
          className="flex items-center gap-2 border-t border-signal/20 bg-signal/[0.06] px-2.5 py-1.5 text-xs"
        >
          <WifiOff className="h-3.5 w-3.5 shrink-0 text-signal" />
          <button
            type="button"
            data-testid="stranded-open"
            onClick={() => setOfflineOpen(true)}
            className="flex-1 truncate text-left font-medium text-foreground/90 hover:text-foreground hover:underline"
          >
            {noNetwork
              ? t("sidebar.strandedNoNetwork")
              : t("sidebar.strandedAlone")}
          </button>
          <button
            type="button"
            data-testid="stranded-dismiss"
            onClick={() => setStrandedDismissed(true)}
            title={t("common.dismiss")}
            className="shrink-0 rounded p-0.5 text-muted-foreground hover:bg-muted hover:text-foreground"
          >
            <X className="h-3.5 w-3.5" />
          </button>
        </div>
      )}
      <OfflineConnectDialog
        open={offlineOpen}
        onOpenChange={setOfflineOpen}
        noNetwork={noNetwork}
      />

      {/* Bottom-left footer: the utility overflow menu (settings/diagnostics/etc.) sits
          first so the top stays clean, then the LAN status + brand mark. */}
      <div className="flex items-center gap-1.5 border-t px-2 py-2 text-xs text-muted-foreground">
        <UtilityMenu />
        {/* Active theme's crest/logo — ADDED in the bottom-left corner (the app's own mark
            stays at the far right; this is an addition, not a replacement). */}
        {themeCrest && (
          <img
            src={themeCrest}
            alt=""
            data-testid="theme-crest"
            title={t("settings.theme")}
            className="h-[18px] w-[18px] shrink-0 object-contain"
          />
        )}
        {ssid ? (
          <Wifi className="h-3.5 w-3.5 shrink-0 text-signal" />
        ) : (
          <Network className="h-3.5 w-3.5 shrink-0 text-signal" />
        )}
        <span
          className="flex-1 truncate"
          title={ssid ?? t("sidebar.localNetwork")}
        >
          {ssid ?? t("sidebar.localNetwork")}
        </span>
        {/* Live LAN headcount — restored from the old footer text; a breathing signal dot +
            the number of people currently discovered around us. Always visible (the SSID can
            grow long and truncate), with the full phrase in the tooltip. */}
        <span
          data-testid="lan-online-count"
          className="flex shrink-0 items-center gap-1 tabular-nums"
          title={t("sidebar.peopleOnLan", { count: onlinePeople })}
        >
          <PresenceDot
            status={onlinePeople > 0 ? "online" : "offline"}
            size="sm"
          />
          {onlinePeople}
        </span>
        {/* App's own brand mark — always present (kept distinct from the theme crest above). */}
        <Logo
          size={16}
          className="ml-1.5 shrink-0 opacity-80"
          title="Mesh-Talk"
        />
      </div>

      {/* Right-edge resize handle: drag to resize (persisted), double-click to reset. The hit
          area is a few px wide; a hairline highlights in the signal accent on hover. */}
      <div
        role="separator"
        aria-orientation="vertical"
        aria-label={t("sidebar.resize")}
        tabIndex={0}
        data-testid="sidebar-resize-handle"
        onPointerDown={onPointerDown}
        onDoubleClick={reset}
        onKeyDown={onKeyDown}
        className="group absolute inset-y-0 right-0 z-20 w-1.5 translate-x-1/2 cursor-col-resize outline-none"
      >
        <span
          aria-hidden
          className="absolute inset-y-0 left-1/2 w-px -translate-x-1/2 bg-transparent transition-colors group-hover:bg-signal group-focus-visible:bg-signal"
        />
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
