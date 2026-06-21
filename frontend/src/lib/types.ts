// TypeScript mirrors of the Rust IPC structs (serde serializes field names as-is,
// so these are snake_case). Keep in sync with src-tauri/src/chat_commands.rs + events.rs.

export interface UserInfo {
  id: string;
  username: string;
}

export interface LoginResult {
  success: boolean;
  token?: string;
  user?: UserInfo;
}

export interface RegisterResult {
  success: boolean;
  user?: UserInfo;
}

export interface LogoutResult {
  success: boolean;
}

export interface PeerInfo {
  user_id: string;
  name: string;
  addr: string;
  post_office: boolean;
  account_id: string | null;
}

export interface HistoryItem {
  id: string | null; // null when there is no stable event id yet
  from_me: boolean;
  who: string;
  text: string;
  wall_clock: number;
  reply_to: string | null;
}

export interface ReactionInfo {
  target: string;
  emoji: string;
  who: string[];
}

export interface ChannelMemberInfo {
  user_id: string;
  name: string;
}

export interface AccountInfo {
  account_id: string;
  device_count: number;
  names: string[];
}

export interface ChannelInfo {
  channel_id: string;
  name: string;
  member_count: number;
}

export interface SearchHitInfo {
  is_channel: boolean;
  target: string;
  label: string;
  from_me: boolean;
  who: string;
  text: string;
  wall_clock: number;
}

// --- Diagnostics / discovery (src-tauri/src/chat_commands.rs) ---

export interface DiagPeerInfo {
  user_id: string;
  name: string;
  ip: string;
  tcp_port: number;
  post_office: boolean;
  account_id: string | null;
  last_seen_secs: number;
}

export interface DiagNetworkInfo {
  own_user_id: string;
  own_name: string;
  account_id: string;
  listen_tcp_port: number;
  discovery_port: number;
  multicast_group: string;
  interfaces: string[];
}

// --- Background-presence settings (src-tauri/src/settings.rs) ---

export interface AppSettings {
  /** Closing the window hides it to the tray instead of quitting. */
  minimize_to_tray: boolean;
  /** Native notification on incoming messages when the window isn't focused. */
  notifications: boolean;
}

// --- Tauri events (src-tauri/src/events.rs) ---

export interface DmReceivedEvent {
  from: string;
  from_name: string;
  text: string;
  reply_to: string | null;
}

export interface ChannelMessageEvent {
  channel_id: string;
  channel_name: string;
  from: string;
  text: string;
  reply_to: string | null;
}

export interface FileReceivedEvent {
  conv: string;
  from: string;
  name: string;
  size: number;
  file_conv: string;
}

export interface FileProgressEvent {
  file_conv: string; // hex per-file conv, OR the send target label until the id resolves
  direction: "send" | "save";
  done: number;
  total: number;
}
