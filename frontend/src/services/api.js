import { invoke } from "@tauri-apps/api/core";

// Authentication API (login starts the redesign node on the backend).
const authAPI = {
  login: (username, password) => invoke("login", { username, password }),
  register: (username, password) => invoke("register", { username, password }),
  logout: () => invoke("logout"),
};

// Redesign node API (the serverless E2E messaging stack — the whole product).
const redesignAPI = {
  myId: () => invoke("redesign_my_id"),
  listPeers: () => invoke("redesign_list_peers"),
  sendDm: (recipient, text, replyTo = null) =>
    invoke("redesign_send_dm", { recipient, text, replyTo }),
  history: (peer, limit) => invoke("redesign_history", { peer, limit }),
  listChannels: () => invoke("redesign_list_channels"),
  createChannel: (name, memberIds) => invoke("redesign_create_channel", { name, memberIds }),
  sendChannelMessage: (channelId, text, replyTo = null) =>
    invoke("redesign_send_channel_message", { channelId, text, replyTo }),
  channelHistory: (channelId, limit) => invoke("redesign_channel_history", { channelId, limit }),
  sendFileDm: (recipient, path) => invoke("redesign_send_file_dm", { recipient, path }),
  sendFileChannel: (channelId, path) => invoke("redesign_send_file_channel", { channelId, path }),
  saveFile: (fileConv, dest) => invoke("redesign_save_file", { fileConv, dest }),
  reactDm: (recipient, target, emoji, remove) =>
    invoke("redesign_react_dm", { recipient, target, emoji, remove }),
  reactChannel: (channelId, target, emoji, remove) =>
    invoke("redesign_react_channel", { channelId, target, emoji, remove }),
  reactions: (peer) => invoke("redesign_reactions", { peer }),
  channelReactions: (channelId) => invoke("redesign_channel_reactions", { channelId }),
  search: (query) => invoke("redesign_search", { query }),
  addChannelMember: (channelId, memberId) => invoke("redesign_add_channel_member", { channelId, memberId }),
  removeChannelMember: (channelId, memberId) => invoke("redesign_remove_channel_member", { channelId, memberId }),
  channelMembers: (channelId) => invoke("redesign_channel_members", { channelId }),
  // Multi-device: account identity, account-addressed messaging, and device linking.
  accountId: () => invoke("redesign_account_id"),
  sendToAccount: (account, text, replyTo = null) =>
    invoke("redesign_send_to_account", { account, text, replyTo }),
  accountHistory: (account, limit) => invoke("redesign_account_history", { account, limit }),
  listAccounts: () => invoke("redesign_list_accounts"),
  startLinking: () => invoke("redesign_start_linking"),
  stopLinking: () => invoke("redesign_stop_linking"),
  linkDevice: (peer, code) => invoke("redesign_link_device", { peer, code }),
  rekeyAccount: () => invoke("redesign_rekey_account"),
  adoptLinkedAccount: () => invoke("redesign_adopt_linked_account"),
  sendFileToAccount: (account, path) => invoke("redesign_send_file_to_account", { account, path }),
  reactAccount: (account, target, emoji, remove) =>
    invoke("redesign_react_account", { account, target, emoji, remove }),
  accountReactions: (account) => invoke("redesign_account_reactions", { account }),
};

export const API = {
  auth: authAPI,
  redesign: redesignAPI,
};
