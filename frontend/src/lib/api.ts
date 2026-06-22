import { invoke } from "@tauri-apps/api/core";
import {
  disable as autostartDisable,
  enable as autostartEnable,
  isEnabled as autostartIsEnabled,
} from "@tauri-apps/plugin-autostart";
import type {
  AccountInfo,
  AppSettings,
  ChannelInfo,
  ChannelMemberInfo,
  DiagNetworkInfo,
  DiagPeerInfo,
  EnvInfo,
  FavoriteInfo,
  HistoryItem,
  LoginResult,
  LogoutResult,
  PeerInfo,
  ReactionInfo,
  RegisterResult,
  SafetyNumber,
  SearchHitInfo,
  TrustInfo,
} from "./types";

// Tauri v2 maps camelCase JS arg keys to the Rust commands' snake_case params.

export const auth = {
  login: (username: string, password: string) =>
    invoke<LoginResult>("login", { username, password }),
  register: (username: string, password: string) =>
    invoke<RegisterResult>("register", { username, password }),
  logout: () => invoke<LogoutResult>("logout"),
  adoptLinkedAccount: () => invoke<void>("adopt_linked_account"),
};

export const chat = {
  myId: () => invoke<string>("my_id"),
  accountId: () => invoke<string>("account_id"),

  listPeers: () => invoke<PeerInfo[]>("list_peers"),
  listAccounts: () => invoke<AccountInfo[]>("list_accounts"),
  listChannels: () => invoke<ChannelInfo[]>("list_channels"),

  // Direct messages (device-addressed)
  sendDm: (recipient: string, text: string, replyTo: string | null = null) =>
    invoke<void>("send_dm", { recipient, text, replyTo }),
  history: (peer: string, limit: number) =>
    invoke<HistoryItem[]>("history", { peer, limit }),
  reactions: (peer: string) => invoke<ReactionInfo[]>("reactions", { peer }),
  reactDm: (
    recipient: string,
    target: string,
    emoji: string,
    remove: boolean,
  ) => invoke<void>("react_dm", { recipient, target, emoji, remove }),
  sendFileDm: (recipient: string, path: string) =>
    invoke<string>("send_file_dm", { recipient, path }),

  // Account-addressed (multi-device) messages
  sendToAccount: (
    account: string,
    text: string,
    replyTo: string | null = null,
  ) => invoke<void>("send_to_account", { account, text, replyTo }),
  accountHistory: (account: string, limit: number) =>
    invoke<HistoryItem[]>("account_history", { account, limit }),
  accountReactions: (account: string) =>
    invoke<ReactionInfo[]>("account_reactions", { account }),
  reactAccount: (
    account: string,
    target: string,
    emoji: string,
    remove: boolean,
  ) => invoke<void>("react_account", { account, target, emoji, remove }),
  sendFileToAccount: (account: string, path: string) =>
    invoke<string>("send_file_to_account", { account, path }),

  // Channels
  createChannel: (name: string, memberIds: string[]) =>
    invoke<string>("create_channel", { name, memberIds }),
  channelMembers: (channelId: string) =>
    invoke<ChannelMemberInfo[]>("channel_members", { channelId }),
  addChannelMember: (channelId: string, memberId: string) =>
    invoke<void>("add_channel_member", { channelId, memberId }),
  removeChannelMember: (channelId: string, memberId: string) =>
    invoke<void>("remove_channel_member", { channelId, memberId }),
  sendChannelMessage: (
    channelId: string,
    text: string,
    replyTo: string | null = null,
  ) => invoke<void>("send_channel_message", { channelId, text, replyTo }),
  channelHistory: (channelId: string, limit: number) =>
    invoke<HistoryItem[]>("channel_history", { channelId, limit }),
  channelReactions: (channelId: string) =>
    invoke<ReactionInfo[]>("channel_reactions", { channelId }),
  reactChannel: (
    channelId: string,
    target: string,
    emoji: string,
    remove: boolean,
  ) => invoke<void>("react_channel", { channelId, target, emoji, remove }),
  sendFileChannel: (channelId: string, path: string) =>
    invoke<string>("send_file_channel", { channelId, path }),

  // Contact trust / safety numbers
  getTrust: (accountId: string, currentFingerprint: string) =>
    invoke<TrustInfo>("get_trust", { accountId, currentFingerprint }),
  markVerified: (accountId: string, fingerprint: string) =>
    invoke<void>("mark_verified", { accountId, fingerprint }),
  safetyNumber: (fingerprint: string) =>
    invoke<SafetyNumber>("safety_number", { fingerprint }),

  // Files + search + device linking
  saveFile: (fileConv: string, dest: string) =>
    invoke<void>("save_file", { fileConv, dest }),
  saveFileToDir: (fileConv: string, dir: string) =>
    invoke<string>("save_file_to_dir", { fileConv, dir }),
  readFile: (fileConv: string) =>
    invoke<ArrayBuffer>("read_file", { fileConv }),
  /** Write pasted bytes to a temp file and return its path, to feed the file-send pipeline. */
  writeTempFile: (bytes: number[], ext: string) =>
    invoke<string>("write_temp_file", { bytes, ext }),
  search: (query: string) => invoke<SearchHitInfo[]>("search", { query }),
  startLinking: () => invoke<string>("start_linking"),
  stopLinking: () => invoke<void>("stop_linking"),
  linkDevice: (peer: string, code: string) =>
    invoke<string>("link_device", { peer, code }),
  rekeyAccount: () => invoke<string>("rekey_account"),
};

export const diag = {
  getPeers: () => invoke<DiagPeerInfo[]>("diag_get_peers"),
  networkInfo: () => invoke<DiagNetworkInfo>("diag_network_info"),
  /** Force an immediate re-announce + rescan (manual "announce now"). */
  rescan: () => invoke<void>("rescan_peers"),
};

/** Observability: logs + static environment facts for the Diagnostics dialog. */
export const obs = {
  envInfo: () => invoke<EnvInfo>("env_info"),
  logsDir: () => invoke<string>("get_logs_dir"),
  logFile: () => invoke<string>("get_log_file"),
  logTail: () => invoke<string>("read_log_tail"),
  saveLogTail: (dest: string) => invoke<void>("save_log_tail", { dest }),
};

export const favorites = {
  /** Every favorites entry the user has set (pin and/or alias), keyed by id. */
  get: () => invoke<FavoriteInfo[]>("get_favorites"),
  /** Pin or unpin a contact by id. */
  setFavorite: (id: string, pinned: boolean) =>
    invoke<void>("set_favorite", { id, pinned }),
  /** Set or clear (null/blank) a contact's custom alias. */
  setAlias: (id: string, alias: string | null) =>
    invoke<void>("set_alias", { id, alias }),
};

export const settings = {
  /** The two non-autostart toggles (minimize-to-tray, notifications). */
  get: () => invoke<AppSettings>("get_app_settings"),
  set: (value: AppSettings) =>
    invoke<void>("set_app_settings", { settings: value }),
  // Launch-at-login is owned by the autostart plugin (OS launch-agent is the
  // source of truth), so it's read/written through the plugin, not our state.
  autostartEnabled: () => autostartIsEnabled(),
  setAutostart: (on: boolean) => (on ? autostartEnable() : autostartDisable()),
};
