import { create } from "zustand";
import { chat } from "@/lib/api";
import { subscribeNodeEvents } from "@/lib/events";
import type {
  AccountInfo,
  ChannelInfo,
  ChannelMemberInfo,
  ChannelMessageEvent,
  DmReceivedEvent,
  FileReceivedEvent,
  HistoryItem,
  PeerInfo,
  ReactionInfo,
} from "@/lib/types";

const HISTORY_LIMIT = 200;

export type ConvKind = "account" | "channel";

export interface Conversation {
  kind: ConvKind;
  id: string; // account_id (hex) | channel_id (hex)
  name: string;
}

export interface ChatMessage {
  id: string | null; // hex EventId; null while pending
  fromMe: boolean;
  who: string;
  text: string;
  wallClock: number;
  replyTo: string | null;
  pending?: boolean;
  file?: { name: string; size: number; fileConv: string } | null;
}

export interface IncomingFile {
  fromName: string;
  name: string;
  size: number;
  fileConv: string;
}

export const convKey = (c: Conversation) => `${c.kind}:${c.id}`;

function fromHistory(h: HistoryItem): ChatMessage {
  return {
    id: h.id,
    fromMe: h.from_me,
    who: h.who,
    text: h.text,
    wallClock: h.wall_clock,
    replyTo: h.reply_to,
  };
}

// --- per-conversation API dispatch ----------------------------------------

function historyFor(c: Conversation) {
  return c.kind === "account"
    ? chat.accountHistory(c.id, HISTORY_LIMIT)
    : chat.channelHistory(c.id, HISTORY_LIMIT);
}
function reactionsFor(c: Conversation) {
  return c.kind === "account"
    ? chat.accountReactions(c.id)
    : chat.channelReactions(c.id);
}
function sendTextFor(c: Conversation, text: string, replyTo: string | null) {
  return c.kind === "account"
    ? chat.sendToAccount(c.id, text, replyTo)
    : chat.sendChannelMessage(c.id, text, replyTo);
}
function reactFor(c: Conversation, target: string, emoji: string, remove: boolean) {
  return c.kind === "account"
    ? chat.reactAccount(c.id, target, emoji, remove)
    : chat.reactChannel(c.id, target, emoji, remove);
}
function sendFileFor(c: Conversation, path: string) {
  return c.kind === "account"
    ? chat.sendFileToAccount(c.id, path)
    : chat.sendFileChannel(c.id, path);
}

interface ChatState {
  ready: boolean;
  myId: string;
  myAccountId: string;
  peers: PeerInfo[];
  accounts: AccountInfo[];
  channels: ChannelInfo[];
  active: Conversation | null;
  messages: Record<string, ChatMessage[]>;
  reactions: Record<string, ReactionInfo[]>;
  unread: Record<string, number>;
  members: ChannelMemberInfo[];
  incomingFiles: IncomingFile[];
  loading: boolean;

  start: () => () => void;
  dismissFile: (fileConv: string) => void;
  refreshRoster: () => Promise<void>;
  open: (c: Conversation) => Promise<void>;
  reload: () => Promise<void>;
  send: (text: string, replyTo: string | null) => Promise<void>;
  sendFile: (path: string) => Promise<void>;
  toggleReaction: (target: string, emoji: string) => Promise<void>;
  createChannel: (name: string, memberIds: string[]) => Promise<void>;
  addMember: (memberId: string) => Promise<void>;
  removeMember: (memberId: string) => Promise<void>;
}

export const useChat = create<ChatState>((set, get) => ({
  ready: false,
  myId: "",
  myAccountId: "",
  peers: [],
  accounts: [],
  channels: [],
  active: null,
  messages: {},
  reactions: {},
  unread: {},
  members: [],
  incomingFiles: [],
  loading: false,

  dismissFile: (fileConv) =>
    set((s) => ({
      incomingFiles: s.incomingFiles.filter((f) => f.fileConv !== fileConv),
    })),

  start: () => {
    // Fresh slate per login (the store survives logout/login of a different account).
    set({
      ready: false,
      myId: "",
      myAccountId: "",
      peers: [],
      accounts: [],
      channels: [],
      active: null,
      messages: {},
      reactions: {},
      unread: {},
      members: [],
      incomingFiles: [],
    });
    // Poll my_id until the node finishes opening (post-login KDF unlock takes a moment).
    let cancelled = false;
    const boot = async () => {
      for (let i = 0; i < 60 && !cancelled; i++) {
        try {
          const id = await chat.myId();
          const acct = await chat.accountId();
          set({ myId: id, myAccountId: acct, ready: true });
          await get().refreshRoster();
          return;
        } catch {
          await new Promise((r) => setTimeout(r, 500));
        }
      }
    };
    void boot();

    const roster = setInterval(() => {
      if (get().ready) void get().refreshRoster();
    }, 4000);

    const unlisten = subscribeNodeEvents({
      onDm: (e) => get_handleDm(set, get, e),
      onChannelMessage: (e) => get_handleChannel(set, get, e),
      onFile: (e) => get_handleFile(set, get, e),
    });

    return () => {
      cancelled = true;
      clearInterval(roster);
      unlisten();
    };
  },

  refreshRoster: async () => {
    try {
      const [peers, accounts, channels] = await Promise.all([
        chat.listPeers(),
        chat.listAccounts(),
        chat.listChannels(),
      ]);
      set({ peers, accounts, channels });
    } catch {
      // node may still be starting; ignore
    }
  },

  open: async (c) => {
    const key = convKey(c);
    set((s) => ({ active: c, unread: { ...s.unread, [key]: 0 }, members: [] }));
    await get().reload();
    if (c.kind === "channel") {
      try {
        set({ members: await chat.channelMembers(c.id) });
      } catch {
        /* ignore */
      }
    }
  },

  reload: async () => {
    const c = get().active;
    if (!c) return;
    const key = convKey(c);
    set({ loading: true });
    try {
      const [items, reacts] = await Promise.all([historyFor(c), reactionsFor(c)]);
      set((s) => ({
        messages: { ...s.messages, [key]: items.map(fromHistory) },
        reactions: { ...s.reactions, [key]: reacts },
        loading: false,
      }));
    } catch {
      set({ loading: false });
    }
  },

  send: async (text, replyTo) => {
    const c = get().active;
    if (!c || !text.trim()) return;
    const key = convKey(c);
    const optimistic: ChatMessage = {
      id: null,
      fromMe: true,
      who: get().myId,
      text,
      wallClock: Date.now(),
      replyTo,
      pending: true,
    };
    set((s) => ({
      messages: { ...s.messages, [key]: [...(s.messages[key] ?? []), optimistic] },
    }));
    try {
      await sendTextFor(c, text, replyTo);
    } finally {
      // Re-sync from the log so the message gets its real id (needed for reactions).
      if (get().active && convKey(get().active!) === key) await get().reload();
    }
  },

  sendFile: async (path) => {
    const c = get().active;
    if (!c) return;
    await sendFileFor(c, path);
    if (get().active && convKey(get().active!) === convKey(c)) await get().reload();
  },

  toggleReaction: async (target, emoji) => {
    const c = get().active;
    if (!c) return;
    const key = convKey(c);
    // Reaction `who` is keyed by account id for account conversations, device user-id for
    // channels — so "did I already react?" must compare against the matching id, or an
    // account-conversation reaction can never be detected as ours (never toggles off).
    const selfId = c.kind === "account" ? get().myAccountId : get().myId;
    const mine = (get().reactions[key] ?? []).find(
      (r) => r.target === target && r.emoji === emoji && r.who.includes(selfId),
    );
    try {
      await reactFor(c, target, emoji, Boolean(mine));
      await get().reload();
    } catch {
      /* ignore */
    }
  },

  createChannel: async (name, memberIds) => {
    const id = await chat.createChannel(name, memberIds);
    await get().refreshRoster();
    await get().open({ kind: "channel", id, name });
  },

  addMember: async (memberId) => {
    const c = get().active;
    if (!c || c.kind !== "channel") return;
    await chat.addChannelMember(c.id, memberId);
    set({ members: await chat.channelMembers(c.id) });
    void get().refreshRoster();
  },

  removeMember: async (memberId) => {
    const c = get().active;
    if (!c || c.kind !== "channel") return;
    await chat.removeChannelMember(c.id, memberId);
    set({ members: await chat.channelMembers(c.id) });
    void get().refreshRoster();
  },
}));

// --- incoming event handlers (module fns to keep the store object lean) -----

type Set = (
  partial: Partial<ChatState> | ((s: ChatState) => Partial<ChatState>),
) => void;
type Get = () => ChatState;

function bump(set: Set, key: string, active: boolean) {
  if (!active) {
    set((s) => ({ unread: { ...s.unread, [key]: (s.unread[key] ?? 0) + 1 } }));
  }
}

function get_handleDm(set: Set, get: Get, e: DmReceivedEvent) {
  // Route to the sender's account conversation (multi-device aware).
  const peer = get().peers.find((p) => p.user_id === e.from);
  const accountId = peer?.account_id;
  if (!accountId) {
    void get().refreshRoster();
    return;
  }
  const conv: Conversation = {
    kind: "account",
    id: accountId,
    name: e.from_name || peer?.name || "",
  };
  const key = convKey(conv);
  const isActive = get().active != null && convKey(get().active!) === key;
  if (isActive) void get().reload();
  bump(set, key, isActive);
}

function get_handleChannel(set: Set, get: Get, e: ChannelMessageEvent) {
  const conv: Conversation = {
    kind: "channel",
    id: e.channel_id,
    name: e.channel_name,
  };
  const key = convKey(conv);
  const isActive = get().active != null && convKey(get().active!) === key;
  if (isActive) void get().reload();
  bump(set, key, isActive);
  void get().refreshRoster();
}

function get_handleFile(set: Set, get: Get, e: FileReceivedEvent) {
  // History carries no file metadata, so surface received files in a dedicated tray
  // (Save via a native dialog). De-dupe by file_conv.
  const peer = get().peers.find((p) => p.user_id === e.from);
  const fromName = peer?.name || e.from;
  set((s) =>
    s.incomingFiles.some((f) => f.fileConv === e.file_conv)
      ? {}
      : {
          incomingFiles: [
            { fromName, name: e.name, size: e.size, fileConv: e.file_conv },
            ...s.incomingFiles,
          ],
        },
  );
}
