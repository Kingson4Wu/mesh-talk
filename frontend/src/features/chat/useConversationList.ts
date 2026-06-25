import { useMemo } from "react";
import { useTranslation } from "react-i18next";
import { shortId } from "@/lib/format";
import { useChat } from "@/store/chat";
import { accountConv, channelConv } from "./ConversationRow";

/**
 * The conversation list as rows (DM accounts + channels), partitioned pinned vs the rest, with
 * alias/name/subtitle resolved. Shared by the desktop Sidebar and the mobile list so both show the
 * SAME conversations from ONE source (no drift). Pin/rename actions stay in each shell (they own
 * their own rename UI); this hook is pure derived data.
 */
export function useConversationList() {
  const { t } = useTranslation();
  const accounts = useChat((s) => s.accounts);
  const channels = useChat((s) => s.channels);
  const favorites = useChat((s) => s.favorites);

  return useMemo(() => {
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
      accountCount: accounts.length,
      pinnedAccounts,
      unpinnedAccounts,
      pinnedChannels,
      unpinnedChannels,
      hasPinned: pinnedAccounts.length + pinnedChannels.length > 0,
    };
  }, [accounts, channels, favorites, t]);
}
