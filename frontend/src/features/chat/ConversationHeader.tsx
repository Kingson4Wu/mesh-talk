import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  IdentityCrest,
  PresenceDot,
  AvatarEditMenu,
} from "@/components/identity";
import { GroupAvatar } from "@/components/GroupAvatar";
import { chat as chatApi } from "@/lib/api";
import { useChat, type Conversation } from "@/store/chat";
import {
  presenceLabel,
  presenceStatus,
  usePresenceFor,
} from "@/store/presence";

/** DM header — the large IdentityCrest (glyph + name + presence + verified). */
function DmHeader({ id, name }: { id: string; name: string }) {
  const { t } = useTranslation();
  const peers = useChat((s) => s.peers);
  const presence = usePresenceFor(id);
  const status = presenceStatus(presence);

  // Verified state for the crest: a single trust read for the active account (not per-row, so no
  // fan-out), refreshed when the conversation or its presenting device changes.
  const fingerprint = peers.find((p) => p.account_id === id)?.user_id ?? "";
  const [verified, setVerified] = useState(false);
  useEffect(() => {
    if (!fingerprint) {
      setVerified(false);
      return;
    }
    let live = true;
    void chatApi
      .getTrust(id, fingerprint)
      .then((tr) => live && setVerified(tr.verified && !tr.fingerprint_changed))
      .catch(() => {});
    return () => {
      live = false;
    };
  }, [id, fingerprint]);

  return (
    <div className="flex min-w-0 flex-col gap-0.5">
      <IdentityCrest
        id={id}
        name={name}
        verified={verified}
        status={status}
        variant="compact"
      />
      <span className="pl-12 text-xs text-muted-foreground">
        {presenceLabel(presence, t)}
      </span>
    </div>
  );
}

/** Channel header — a mesh/group glyph + name + member count. */
function ChannelHeader({
  id,
  name,
  memberCount,
}: {
  id: string;
  name: string;
  memberCount: number;
}) {
  const { t } = useTranslation();
  const presence = usePresenceFor(id);
  const status = presenceStatus(presence);
  return (
    <div className="flex min-w-0 items-center gap-3">
      <div className="relative">
        <AvatarEditMenu id={id} ariaLabel={t("avatar.editGroup")} pack="clubs">
          <GroupAvatar channelId={id} size={36} title={name} />
        </AvatarEditMenu>
        <PresenceDot
          status={status}
          size="md"
          className="absolute -bottom-0.5 -right-0.5"
        />
      </div>
      <div className="min-w-0">
        <div className="truncate font-display font-semibold tracking-tight">
          {name}
        </div>
        <div className="truncate text-xs text-muted-foreground">
          {memberCount
            ? t("conversation.members", { count: memberCount })
            : t("conversation.channel")}
        </div>
      </div>
    </div>
  );
}

/** The conversation header (DM crest or channel glyph), shared by the desktop + mobile shells. */
export function ConversationHeader({
  active,
  name,
  memberCount,
}: {
  active: Conversation;
  name: string;
  memberCount: number;
}) {
  return active.kind === "channel" ? (
    <ChannelHeader id={active.id} name={name} memberCount={memberCount} />
  ) : (
    <DmHeader id={active.id} name={name} />
  );
}
