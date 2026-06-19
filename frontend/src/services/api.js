import { invoke } from "@tauri-apps/api/core";

// Authentication API (login starts the node on the backend).
const authAPI = {
  login: (username, password) => invoke("login", { username, password }),
  register: (username, password) => invoke("register", { username, password }),
  logout: () => invoke("logout"),
};

// Node API (the serverless E2E messaging stack — the whole product).
const chatAPI = {
  myId: () => invoke("my_id"),
  listPeers: () => invoke("list_peers"),
  sendDm: (recipient, text, replyTo = null) =>
    invoke("send_dm", { recipient, text, replyTo }),
  history: (peer, limit) => invoke("history", { peer, limit }),
  listChannels: () => invoke("list_channels"),
  createChannel: (name, memberIds) => invoke("create_channel", { name, memberIds }),
  sendChannelMessage: (channelId, text, replyTo = null) =>
    invoke("send_channel_message", { channelId, text, replyTo }),
  channelHistory: (channelId, limit) => invoke("channel_history", { channelId, limit }),
  sendFileDm: (recipient, path) => invoke("send_file_dm", { recipient, path }),
  sendFileChannel: (channelId, path) => invoke("send_file_channel", { channelId, path }),
  saveFile: (fileConv, dest) => invoke("save_file", { fileConv, dest }),
  reactDm: (recipient, target, emoji, remove) =>
    invoke("react_dm", { recipient, target, emoji, remove }),
  reactChannel: (channelId, target, emoji, remove) =>
    invoke("react_channel", { channelId, target, emoji, remove }),
  reactions: (peer) => invoke("reactions", { peer }),
  channelReactions: (channelId) => invoke("channel_reactions", { channelId }),
  search: (query) => invoke("search", { query }),
  addChannelMember: (channelId, memberId) => invoke("add_channel_member", { channelId, memberId }),
  removeChannelMember: (channelId, memberId) => invoke("remove_channel_member", { channelId, memberId }),
  channelMembers: (channelId) => invoke("channel_members", { channelId }),
  // Multi-device: account identity, account-addressed messaging, and device linking.
  accountId: () => invoke("account_id"),
  sendToAccount: (account, text, replyTo = null) =>
    invoke("send_to_account", { account, text, replyTo }),
  accountHistory: (account, limit) => invoke("account_history", { account, limit }),
  listAccounts: () => invoke("list_accounts"),
  startLinking: () => invoke("start_linking"),
  stopLinking: () => invoke("stop_linking"),
  linkDevice: (peer, code) => invoke("link_device", { peer, code }),
  rekeyAccount: () => invoke("rekey_account"),
  adoptLinkedAccount: () => invoke("adopt_linked_account"),
  sendFileToAccount: (account, path) => invoke("send_file_to_account", { account, path }),
  reactAccount: (account, target, emoji, remove) =>
    invoke("react_account", { account, target, emoji, remove }),
  accountReactions: (account) => invoke("account_reactions", { account }),
};

export const API = {
  auth: authAPI,
  chat: chatAPI,
};
