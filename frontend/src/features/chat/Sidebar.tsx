import { Hash, LogOut, MessagesSquare, Users } from "lucide-react";
import { Avatar } from "@/components/ui/avatar";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";
import { shortId } from "@/lib/format";
import { CreateChannelDialog } from "./CreateChannelDialog";
import { SearchDialog } from "./SearchDialog";
import { FilesTray } from "./FilesTray";
import { LinkDeviceDialog } from "./LinkDeviceDialog";
import { useAuth } from "@/store/auth";
import {
  convKey,
  useChat,
  type Conversation,
} from "@/store/chat";
import type { AccountInfo, ChannelInfo } from "@/lib/types";

function accountConv(a: AccountInfo): Conversation {
  return { kind: "account", id: a.account_id, name: a.names[0] || shortId(a.account_id) };
}
function channelConv(c: ChannelInfo): Conversation {
  return { kind: "channel", id: c.channel_id, name: c.name };
}

function Row({
  conv,
  subtitle,
  icon,
}: {
  conv: Conversation;
  subtitle: string;
  icon?: React.ReactNode;
}) {
  const active = useChat((s) => s.active);
  const unread = useChat((s) => s.unread[convKey(conv)] ?? 0);
  const open = useChat((s) => s.open);
  const isActive = active != null && convKey(active) === convKey(conv);

  return (
    <button
      onClick={() => open(conv)}
      className={cn(
        "flex w-full items-center gap-3 rounded-lg px-2.5 py-2 text-left transition-colors",
        isActive ? "bg-accent" : "hover:bg-accent/50",
      )}
    >
      {icon ?? <Avatar name={conv.name} id={conv.id} className="h-9 w-9" />}
      <div className="min-w-0 flex-1">
        <div className="truncate text-sm font-medium">{conv.name}</div>
        <div className="truncate text-xs text-muted-foreground">{subtitle}</div>
      </div>
      {unread > 0 && (
        <Badge className="bg-primary text-primary-foreground">{unread}</Badge>
      )}
    </button>
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
  const accounts = useChat((s) => s.accounts);
  const channels = useChat((s) => s.channels);
  const myId = useChat((s) => s.myId);
  const ready = useChat((s) => s.ready);
  const bootFailed = useChat((s) => s.bootFailed);
  const username = useAuth((s) => s.user?.username ?? "");
  const logout = useAuth((s) => s.logout);

  return (
    <aside className="flex w-72 shrink-0 flex-col border-r bg-card/40">
      {/* identity header */}
      <div className="border-b px-4 py-3">
        <div className="flex items-center gap-3">
          <div className="flex h-9 w-9 items-center justify-center rounded-xl bg-primary text-primary-foreground">
            <MessagesSquare className="h-5 w-5" />
          </div>
          <div className="min-w-0 flex-1">
            <div className="truncate text-sm font-semibold">{username}</div>
            <div className="truncate font-mono text-xs text-muted-foreground">
              {ready
                ? `you · ${shortId(myId)}`
                : bootFailed
                  ? "node unavailable"
                  : "starting…"}
            </div>
          </div>
        </div>
        <div className="mt-2 flex items-center gap-0.5">
          <SearchDialog />
          <FilesTray />
          <LinkDeviceDialog />
          <div className="flex-1" />
          <Button variant="ghost" size="icon" title="Sign out" onClick={() => logout()}>
            <LogOut className="h-4 w-4" />
          </Button>
        </div>
      </div>

      <nav className="flex-1 overflow-y-auto px-2 pb-4">
        <SectionLabel>Direct messages</SectionLabel>
        {accounts.length === 0 && (
          <p className="px-2.5 py-2 text-xs text-muted-foreground">
            No contacts discovered yet…
          </p>
        )}
        {accounts.map((a) => (
          <Row
            key={a.account_id}
            conv={accountConv(a)}
            subtitle={
              a.device_count > 1 ? `${a.device_count} devices` : shortId(a.account_id, 12)
            }
          />
        ))}

        <SectionLabel action={<CreateChannelDialog />}>Channels</SectionLabel>
        {channels.length === 0 && (
          <p className="px-2.5 py-2 text-xs text-muted-foreground">No channels yet.</p>
        )}
        {channels.map((c) => (
          <Row
            key={c.channel_id}
            conv={channelConv(c)}
            subtitle={`${c.member_count} member${c.member_count === 1 ? "" : "s"}`}
            icon={
              <div className="flex h-9 w-9 items-center justify-center rounded-full bg-muted text-muted-foreground">
                <Hash className="h-4 w-4" />
              </div>
            }
          />
        ))}
      </nav>

      <div className="flex items-center gap-2 border-t px-4 py-2 text-xs text-muted-foreground">
        <Users className="h-3.5 w-3.5" />
        {accounts.length} contact{accounts.length === 1 ? "" : "s"} on the LAN
      </div>
    </aside>
  );
}
