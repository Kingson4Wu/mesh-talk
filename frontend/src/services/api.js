import { invoke } from "@tauri-apps/api/core";

// Authentication API functions
const authAPI = {
  login: (username, password) => invoke("login", { username, password }),
  register: (username, password) => invoke("register", { username, password }),
  logout: () => invoke("logout"),
};

// Node API functions
const nodeAPI = {
  getNodeInfo: () => invoke("get_node_info"),
  connectToNode: (address) => invoke("connect_to_node", { address }),
  connectToUser: (userId) => invoke("connect_to_user", { userId }),
};

// Contact API functions
const contactsAPI = {
  getContacts: () => invoke("get_contacts"),
  updateContact: (contactId, data) =>
    invoke("update_contact", { id: contactId, ...data }),
  deleteContact: (contactId) => invoke("delete_contact", { contactId }),
  sendContactRequest: (
    targetPublicKey,
    alias,
    username,
    remoteIp,
    port,
    userId,
  ) => {
    console.log("[API] Sending contact request with parameters:", {
      targetPublicKey,
      alias,
      username,
      remoteIp,
      port,
      userId,
    });

    return invoke("send_contact_request", {
      targetPublicKey,
      alias,
      username,
      remote_ip: remoteIp,
      port,
      user_id: userId,
    });
  },
  handleContactRequest: (requestJson, approve) =>
    invoke("handle_contact_request", { requestJson, approve }),
};

// Message API functions
const messagesAPI = {
  getMessages: () => invoke("get_messages"),
  sendMessage: (content, target = {}) =>
    invoke("send_message", {
      content,
      targetUserId: target.userId ?? target.user_id ?? null,
      targetAddress: target.address ?? null,
    }),
  markMessageRead: (messageId) => invoke("mark_message_read", { messageId }),
};

const filesAPI = {
  sendFile: (path, target = {}) =>
    invoke("send_file", {
      path,
      targetUserId: target.userId ?? target.user_id ?? null,
      targetAddress: target.address ?? null,
    }),
  resumeTransfer: (transferId) =>
    invoke("resume_file_transfer", { transferId }),
  cancelTransfer: (transferId) =>
    invoke("cancel_file_transfer", { transferId }),
  listTransfers: () => invoke("list_file_transfers"),
  acceptIncoming: (transferId, savePath) =>
    invoke("accept_incoming_file_transfer", { transferId, savePath }),
  rejectIncoming: (transferId) =>
    invoke("reject_incoming_file_transfer", { transferId }),
};

// Network API functions
const networkAPI = {
  getDiscoveredNodes: () => invoke("get_discovered_nodes"),
  startNodeDiscovery: () => invoke("start_node_discovery"),
  stopNodeDiscovery: () => invoke("stop_node_discovery"),
  allowFirewall: (port) => invoke("allow_firewall_port", { port }),
};

// Redesign node API (the new serverless E2E DM stack)
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
  adoptLinkedAccount: () => invoke("redesign_adopt_linked_account"),
  sendFileToAccount: (account, path) => invoke("redesign_send_file_to_account", { account, path }),
  reactAccount: (account, target, emoji, remove) =>
    invoke("redesign_react_account", { account, target, emoji, remove }),
  accountReactions: (account) => invoke("redesign_account_reactions", { account }),
};

// Combined API service
export const API = {
  auth: authAPI,
  node: nodeAPI,
  contacts: contactsAPI,
  messages: messagesAPI,
  files: filesAPI,
  network: networkAPI,
  redesign: redesignAPI,
};
