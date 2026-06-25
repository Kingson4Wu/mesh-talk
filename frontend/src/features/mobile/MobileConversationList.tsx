import type { ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { IdentityGlyph } from "@/components/identity";
import { SearchDialog } from "@/features/chat/SearchDialog";
import { CreateChannelDialog } from "@/features/chat/CreateChannelDialog";
import { ConversationRow } from "@/features/chat/ConversationRow";
import { useConversationList } from "@/features/chat/useConversationList";
import { shortId } from "@/lib/format";
import { useChat } from "@/store/chat";
import { MobileSettings } from "./MobileSettings";

function SectionLabel({
  children,
  action,
}: {
  children: ReactNode;
  action?: ReactNode;
}) {
  return (
    <div className="flex items-center justify-between px-2.5 pb-1 pt-4 text-xs font-semibold uppercase tracking-wider text-muted-foreground">
      <span>{children}</span>
      {action}
    </div>
  );
}

/** The phone's conversation list (full-screen, single pane). Shares the row data + ConversationRow
 * with the desktop sidebar; tapping a row opens it (the store drives single-pane via `active`). */
export function MobileConversationList() {
  const { t } = useTranslation();
  const myAccountId = useChat((s) => s.myAccountId);
  const togglePinned = useChat((s) => s.togglePinned);
  const setAlias = useChat((s) => s.setAlias);
  const {
    pinnedAccounts,
    unpinnedAccounts,
    pinnedChannels,
    unpinnedChannels,
    hasPinned,
    accountCount,
  } = useConversationList();

  const rename = (id: string, current: string) => {
    const next = window.prompt(t("sidebar.rename"), current);
    if (next != null) void setAlias(id, next.trim());
  };

  return (
    <div
      data-testid="sidebar"
      className="flex h-full w-full flex-col bg-card/40"
    >
      <header className="flex items-center gap-2 border-b px-4 py-3">
        <IdentityGlyph seed={myAccountId} size={32} />
        <span className="flex-1 truncate font-display font-semibold tracking-tight">
          Mesh-Talk
        </span>
        <SearchDialog />
        <CreateChannelDialog />
        <MobileSettings />
      </header>
      <nav
        role="list"
        aria-label={t("conversation.list")}
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
                onRename={() => rename(r.id, r.a.names[0] || shortId(r.id))}
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
                onRename={() => rename(r.id, r.c.name)}
              />
            ))}
          </>
        )}

        <SectionLabel action={<CreateChannelDialog />}>
          {t("sidebar.directMessages")}
        </SectionLabel>
        {accountCount === 0 && (
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
            onRename={() => rename(r.id, r.a.names[0] || shortId(r.id))}
          />
        ))}

        {(pinnedChannels.length > 0 || unpinnedChannels.length > 0) && (
          <SectionLabel>{t("sidebar.channels")}</SectionLabel>
        )}
        {unpinnedChannels.map((r) => (
          <ConversationRow
            key={r.id}
            conv={r.conv}
            subtitle={r.subtitle}
            channel
            pinned={false}
            onTogglePin={() => void togglePinned(r.id, true)}
            onRename={() => rename(r.id, r.c.name)}
          />
        ))}
      </nav>
    </div>
  );
}
