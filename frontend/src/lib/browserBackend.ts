// The browser / installed-PWA backend.
//
// On desktop, `api.ts` talks to the Rust core over Tauri IPC. In a plain browser there is no
// Tauri, so commands route here. Identity + auth run against a persistent wasm node (pwaNode);
// read commands return safe empties so the React UI boots and renders; harmless writes no-op.
// Messaging actions (send_dm, channels, …) that need conversation addressing aren't wired yet
// and fail loudly. See docs/superpowers/specs/2026-06-23-mobile-pwa-design.md.

import { loadWasm } from "./wasm";
import {
  loadNode,
  persistNode,
  saveSession,
  loadSession,
  clearSession,
} from "./pwaNode";
import {
  connect as webrtcConnect,
  wsSignaling,
  wsSignalingWithRole,
  wsSignalingMesh,
  meshPeerSignaling,
  type MeshChannel,
} from "./webrtcClient";

type Node = Awaited<ReturnType<typeof loadNode>>;
let node: Node | null = null;
/** This device's display name, announced into the presence directory (so peers see a real name). */
let myName = "";

/**
 * Connect to a peer in `room` on the signaling relay and sync the node's log over the data
 * channel — so messages flow between this PWA and a peer (or a gateway node). `initiator` is the
 * offerer side. After a sync, the conversation's new messages are in the local log. Persisted on
 * completion. (The app calls this on an interval once a relay URL is configured / a QR is scanned.)
 */
export async function connectGateway(
  signalUrl: string,
  room: string,
  initiator: boolean,
): Promise<void> {
  if (!node) throw new Error("not signed in");
  const signaling = await wsSignaling(signalUrl, room);
  const dc = await webrtcConnect(signaling, initiator ? "offerer" : "answerer");
  await node.sync_over_dc(dc, initiator);
  await persistNode(node);
  signaling.close();
}

/**
 * Like `connectGateway`, but the relay assigns this peer's role, so two PWAs can connect to the
 * same room without pre-agreeing who offers — the basis for an automatic background sync.
 */
export async function connectGatewayAuto(
  signalUrl: string,
  room: string,
  timeoutMs = 12000,
): Promise<void> {
  if (!node) throw new Error("not signed in");
  const { signaling, initiator } = await wsSignalingWithRole(signalUrl, room);
  const active = node;
  const round = (async () => {
    const dc = await webrtcConnect(
      signaling,
      initiator ? "offerer" : "answerer",
    );
    await active.sync_over_dc(dc, initiator);
    await persistNode(active);
  })();
  // Bound it: if no peer is in the room, the handshake never completes — give up + retry later.
  const timer = new Promise<never>((_, rej) =>
    setTimeout(() => rej(new Error("gateway connect timed out")), timeoutMs),
  );
  try {
    await Promise.race([round, timer]);
  } finally {
    signaling.close();
  }
}

/**
 * Continuously sync with peers in `room`: repeatedly run a gateway sync, retrying after a peer-
 * less timeout, so messages flow without a manual trigger. Returns a stop function. `onSync`
 * fires after each successful round (e.g. to refresh the conversation view).
 */
export function startGatewaySync(
  signalUrl: string,
  room: string,
  onSync?: () => void,
): () => void {
  let stopped = false;
  void (async () => {
    while (!stopped) {
      try {
        await connectGatewayAuto(signalUrl, room);
        if (!stopped) onSync?.();
      } catch {
        /* no peer yet / timed out — retry after a short pause */
      }
      await new Promise((r) => setTimeout(r, 600));
    }
  })();
  return () => {
    stopped = true;
  };
}

const RELAY_URL_KEY = "gateway-relay-url";
const RELAY_ROOM_KEY = "gateway-room";

/** Read the configured gateway relay (localStorage). */
export function gatewayRelayUrl(): string | null {
  return typeof localStorage !== "undefined"
    ? localStorage.getItem(RELAY_URL_KEY)
    : null;
}

/** Set (or clear) the gateway relay URL + optional room (persisted in localStorage). */
export function setGatewayRelay(url: string | null, room?: string): void {
  if (url) localStorage.setItem(RELAY_URL_KEY, url);
  else localStorage.removeItem(RELAY_URL_KEY);
  if (room) localStorage.setItem(RELAY_ROOM_KEY, room);
}

/**
 * A shareable invite encoding the current gateway config — `meshtalk://gateway?relay=…&room=…`.
 * Hand it to another device (copy/paste, or render as a QR) to point it at the same relay+room.
 * Null when no relay is configured.
 */
export function gatewayInviteLink(): string | null {
  const url = gatewayRelayUrl();
  if (!url) return null;
  const room =
    (typeof localStorage !== "undefined" &&
      localStorage.getItem(RELAY_ROOM_KEY)) ||
    "mesh-talk";
  return `meshtalk://gateway?relay=${encodeURIComponent(url)}&room=${encodeURIComponent(room)}`;
}

/**
 * If the page URL carries `?relay=…(&room=…)` — i.e. it was opened by scanning the desktop hub's
 * QR — adopt that gateway config. Returns true if a relay was found. Called on boot so a scanned
 * link auto-joins with no typing.
 */
export function applyUrlGatewayConfig(): boolean {
  if (typeof location === "undefined") return false;
  const params = new URLSearchParams(location.search);
  const relay = params.get("relay");
  if (!relay) return false;
  setGatewayRelay(relay, params.get("room") || undefined);
  return true;
}

/** If `text` is a gateway invite link, apply its relay + room and return true; else false. */
export function applyGatewayInvite(text: string): boolean {
  try {
    const u = new URL(text.trim());
    if (u.protocol !== "meshtalk:" || u.host !== "gateway") return false;
    const relay = u.searchParams.get("relay");
    if (!relay) return false;
    setGatewayRelay(relay, u.searchParams.get("room") || undefined);
    return true;
  } catch {
    return false;
  }
}

const hexToBytes = (h: string): Uint8Array => {
  const out = new Uint8Array(h.length / 2);
  for (let i = 0; i < out.length; i++)
    out[i] = parseInt(h.slice(i * 2, i * 2 + 2), 16);
  return out;
};

/**
 * Connect to a peer in `room` and sync the ENCRYPTED per-peer DM conversation (vs the plaintext
 * demo conversation of connectGateway). Records the peer in the roster and opens any new messages.
 * Returns the peer's fingerprint. Bounded by a timeout so a peerless room retries.
 */
export async function connectDmSync(
  signalUrl: string,
  room: string,
  timeoutMs = 15000,
): Promise<string> {
  if (!node) throw new Error("not signed in");
  const { signaling, initiator } = await wsSignalingWithRole(signalUrl, room);
  const active = node;
  let peerId = "";
  const round = (async () => {
    const dc = await webrtcConnect(
      signaling,
      initiator ? "offerer" : "answerer",
    );
    peerId = await active.sync_dm_over_dc(dc, initiator);
    await persistNode(active);
  })();
  const timer = new Promise<never>((_, rej) =>
    setTimeout(() => rej(new Error("dm sync timed out")), timeoutMs),
  );
  try {
    await Promise.race([round, timer]);
  } finally {
    signaling.close();
  }
  return peerId;
}

/** Peers discovered via gateway handshakes: `[{id, ed25519, x25519}]` (hex keys). */
export function dmRoster(): { id: string; ed25519: string; x25519: string }[] {
  return node ? JSON.parse(node.peer_roster()) : [];
}

/**
 * Connect to `room` and GOSSIP all conversations with the peer (vs connectDmSync, which syncs only
 * the conversation with that peer). This is how a node reaches everyone through a hub: the hub
 * gossips every conversation, so messages route transitively. Returns the peer's fingerprint.
 */
export async function connectGossipSync(
  signalUrl: string,
  room: string,
  timeoutMs = 15000,
): Promise<string> {
  if (!node) throw new Error("not signed in");
  const { signaling, initiator } = await wsSignalingWithRole(signalUrl, room);
  const active = node;
  let peerId = "";
  const round = (async () => {
    const dc = await webrtcConnect(
      signaling,
      initiator ? "offerer" : "answerer",
    );
    peerId = await active.sync_all_over_dc(dc, initiator);
    await persistNode(active);
  })();
  const timer = new Promise<never>((_, rej) =>
    setTimeout(() => rej(new Error("gossip sync timed out")), timeoutMs),
  );
  try {
    await Promise.race([round, timer]);
  } finally {
    signaling.close();
  }
  return peerId;
}

/**
 * Join `room` as a MESH participant and gossip with every peer through it — the multi-peer
 * deployment with no 2-peer limit. The first node in the room is the hub: it holds one gossiping
 * connection per spoke. Later nodes are spokes that periodically gossip with the hub. Because the
 * hub gossips everyone's conversations + presence directory, all nodes reach all nodes
 * (A → hub → B). Returns a stop function; `onSync` fires after each successful gossip round.
 */
export function runMeshSync(
  relayUrls: string | string[],
  room: string,
  onSync?: () => void,
  roundTimeoutMs = 15000,
): () => void {
  if (!node) throw new Error("not signed in");
  const active = node;
  const seed = (Array.isArray(relayUrls) ? relayUrls : [relayUrls]).filter(
    Boolean,
  );
  if (seed.length === 0) return () => {};
  // Relay candidates, re-evaluated every reconnect so relays LEARNED via gossip (other online
  // hubs announce their endpoint into the mesh) join the failover set — zero-config failover.
  const relayCandidates = (): string[] => {
    const all = [...seed, ...meshKnownRelays(), ...knownRelays()];
    return [...new Set(all)];
  };
  let stopped = false;
  let current: MeshChannel | null = null;

  // One mesh session over a connected channel: act as hub or spoke until the channel closes.
  const session = (mesh: MeshChannel): Promise<void> =>
    new Promise<void>((done) => {
      let live = true;
      const finish = () => {
        if (live) {
          live = false;
          done();
        }
      };
      mesh.onClose(finish);
      const spokeRunning = new Map<number, boolean>();

      // One gossip round with `peer`: a fresh WebRTC connection over the addressed channel.
      const round = async (peer: number, asOfferer: boolean): Promise<void> => {
        const sig = meshPeerSignaling(mesh, peer);
        const work = (async () => {
          const dc = await webrtcConnect(
            sig,
            asOfferer ? "offerer" : "answerer",
          );
          await active.sync_all_over_dc(dc, asOfferer);
          await persistNode(active);
          if (!stopped) onSync?.();
        })();
        const timer = new Promise<never>((_, rej) =>
          setTimeout(
            () => rej(new Error("mesh round timed out")),
            roundTimeoutMs,
          ),
        );
        await Promise.race([work, timer]);
      };
      const answerSpoke = async (spoke: number) => {
        while (live && !stopped && spokeRunning.get(spoke)) {
          try {
            await round(spoke, false);
          } catch {
            /* spoke didn't offer this round */
          }
          await new Promise((r) => setTimeout(r, 300));
        }
      };
      const offerHub = async (hub: number) => {
        while (live && !stopped) {
          try {
            await round(hub, true);
          } catch {
            /* hub unreachable this round */
          }
          await new Promise((r) => setTimeout(r, 1500));
        }
      };

      if (mesh.peers.length === 0) {
        // First in the room → we are the hub: answer every spoke that joins.
        mesh.onPeer((ev) => {
          if (ev.type === "joined") {
            spokeRunning.set(ev.id, true);
            void answerSpoke(ev.id);
          } else {
            spokeRunning.set(ev.id, false);
          }
        });
      } else {
        // A hub already exists → we are a spoke: gossip with it (the oldest peer).
        void offerHub(mesh.peers[0]);
      }
    });

  // Reconnect loop: rotate through the relay candidates. If the current relay is unreachable or
  // drops (e.g. the desktop hosting it goes offline), move to the next known relay — so a phone
  // keeps meshing through any other online hub. The working relay is remembered for next time.
  void (async () => {
    let i = 0;
    while (!stopped) {
      const candidates = relayCandidates();
      const url = candidates[i % candidates.length];
      i++;
      let mesh: MeshChannel | null = null;
      try {
        mesh = await wsSignalingMesh(url, room);
      } catch {
        await new Promise((r) => setTimeout(r, 800));
        continue;
      }
      current = mesh;
      rememberRelay(url);
      // Announce the relay we're on into the gossiped relay directory, so other nodes (incl.
      // phones reached only through a hub) learn it and can fail over to it.
      try {
        active.announce_relay(url);
        await persistNode(active);
      } catch {
        /* best effort */
      }
      await session(mesh);
      try {
        mesh.close();
      } catch {
        /* already closed */
      }
      if (!stopped) await new Promise((r) => setTimeout(r, 400));
    }
  })();

  return () => {
    stopped = true;
    current?.close();
  };
}

/** Announce a relay endpoint into the gossiped relay directory (so peers can fail over to it). */
export async function announceRelay(url: string): Promise<void> {
  if (!node) throw new Error("not signed in");
  node.announce_relay(url);
  await persistNode(node);
}

/** Relay endpoints learned via mesh gossip — other online hubs announce theirs, so a phone can
 * fail over to a relay it never directly used (zero-config failover). */
export function meshKnownRelays(): string[] {
  if (!node) return [];
  try {
    return JSON.parse(node.known_relay_urls());
  } catch {
    return [];
  }
}

const KNOWN_RELAYS_KEY = "mesh-known-relays";

/** Relay URLs this device has successfully meshed through (for failover when one goes offline). */
export function knownRelays(): string[] {
  if (typeof localStorage === "undefined") return [];
  try {
    return JSON.parse(localStorage.getItem(KNOWN_RELAYS_KEY) || "[]");
  } catch {
    return [];
  }
}

/** Remember a working relay URL (deduped) so a future session can fail over to it. */
function rememberRelay(url: string): void {
  if (typeof localStorage === "undefined") return;
  const set = knownRelays();
  if (!set.includes(url)) {
    set.push(url);
    localStorage.setItem(KNOWN_RELAYS_KEY, JSON.stringify(set));
  }
}

/** Conversation ids in the local event log (hex) — introspection for tests/diagnostics. */
export function nodeConversationIds(): string[] {
  return node ? JSON.parse(node.conversation_ids()) : [];
}

type MeshPeer = {
  id: string;
  ed25519: string;
  x25519: string;
  name?: string;
  last_seen_ms?: number;
};

/** The gossiped presence directory: each node's latest announcement (name + last-seen). */
export function meshDirectory(): MeshPeer[] {
  return node ? JSON.parse(node.directory()) : [];
}

/** All addressable peers: the mesh directory + directly-handshook peers, deduped by id. */
function meshPeers(): MeshPeer[] {
  const byId = new Map<string, MeshPeer>();
  // Roster first, then directory — the directory carries the name + last-seen, so it wins.
  for (const p of [...dmRoster(), ...meshDirectory()]) byId.set(p.id, p);
  return [...byId.values()];
}

/** A node counts as "online" if its last presence heartbeat is within this window. Stale entries
 * (from old sessions / nodes that went offline) fall outside it and show as offline / drop off. */
const ONLINE_WINDOW_MS = 90_000;
/** Only nodes seen within this window appear in the contact list at all (prunes long-gone ghosts
 * accumulated in the append-only directory). */
const SHOW_WINDOW_MS = 10 * 60_000;

/** Seal + queue an encrypted DM to a peer (by hex public keys); delivered on the next DM sync. */
export async function sendDmRatcheted(
  peerEd25519: string,
  peerX25519: string,
  text: string,
): Promise<void> {
  if (!node) throw new Error("not signed in");
  node.send_ratcheted_dm(hexToBytes(peerEd25519), hexToBytes(peerX25519), text);
  await persistNode(node);
}

/** The decrypted DM history with a peer (by hex public keys), HistoryItem-shaped. */
export function dmHistoryWith(
  peerEd25519: string,
  peerX25519: string,
): unknown[] {
  if (!node) return [];
  return JSON.parse(
    node.ratcheted_dm_history(hexToBytes(peerEd25519), hexToBytes(peerX25519)),
  );
}

/**
 * Start the background DM sync loop IF a relay is configured; otherwise a no-op. Each round
 * discovers peers (roster) and syncs the encrypted per-peer conversations; `onSync` fires after a
 * round so the UI can refresh the roster + open conversation. Returns a stop function.
 */
export function startConfiguredGatewaySync(onSync?: () => void): () => void {
  const url = gatewayRelayUrl();
  if (!url) return () => {};
  const room =
    (typeof localStorage !== "undefined" &&
      localStorage.getItem(RELAY_ROOM_KEY)) ||
    "mesh-talk";
  // Join as a MESH participant (multi-peer): the desktop node (or the first phone) is the hub and
  // gossips everyone's conversations + presence directory, so every node reaches every node and
  // many phones share one relay — no 2-peer limit. (connectGossipSync remains for the legacy
  // pairwise path / tests.) Pass the configured relay as the seed; runMeshSync also folds in
  // relays learned via gossip (other online hubs announce theirs) + ones used before — so if the
  // host of the scanned relay goes offline, the phone fails over to another online relay it
  // discovered through the mesh, with no extra scanning.
  const stopMesh = runMeshSync(url, room, onSync);
  // Presence heartbeat: re-announce periodically so our last-seen stays fresh while we're online
  // (peers mark us offline once heartbeats stop). Gossiped on the next sync round.
  const heartbeat = setInterval(() => {
    if (!node) return;
    try {
      node.announce_self(myName);
      void persistNode(node);
    } catch {
      /* node tearing down */
    }
  }, 30_000);
  return () => {
    stopMesh();
    clearInterval(heartbeat);
  };
}

/** Commands that read state — return an empty/default value so the UI renders cleanly. */
const EMPTY_READS: Record<string, unknown> = {
  // The PWA has no group channels yet (real DMs come from the gossiped mesh directory); return an
  // empty channel list rather than the old local "Saved messages" demo conversation.
  list_channels: [],
  channel_history: [],
  channel_members: { members: [] },
  get_favorites: [],
  get_avatars: {},
  peer_avatars: {},
  history: [],
  account_history: [],
  reactions: [],
  account_reactions: [],
  channel_reactions: [],
  search: [],
  network_name: null,
  get_app_settings: {
    minimize_to_tray: false,
    notifications: false,
    download_dir: "",
    stay_signed_in: false,
    last_user: null,
  },
};

/** Writes that are safe to ignore in the browser (local personalization / OS-only). */
const IGNORED_WRITES = new Set([
  "set_badge",
  "set_avatar",
  "publish_avatar",
  "set_favorite",
  "set_alias",
  "set_app_settings",
  "rescan_peers",
  "send_channel_message",
]);

/** Route one api command. Mirrors the Tauri command names used in `api.ts`. */
export async function browserInvoke<T>(
  cmd: string,
  args?: Record<string, unknown>,
): Promise<T> {
  const r = <V>(v: V) => v as unknown as T;

  switch (cmd) {
    // --- auth + identity (the persistent wasm node) ---
    case "login":
    case "register": {
      const password = String(args?.password ?? "");
      node = await loadNode(password);
      myName = String(args?.username ?? "");
      // Presence: announce our name into the gossiped directory (cheap, in-memory append). NOT
      // persisted here on purpose — re-sealing the DM state runs a 600k-PBKDF2 and would freeze the
      // login UI; the background sync loop persists it shortly after, and re-announces (heartbeat).
      node.announce_self(myName);
      // "Stay signed in": remember the session so the next open auto-unlocks the SAME identity
      // instead of re-prompting (and re-minting). Same-origin only — browser storage can't cross
      // to another PC's page.
      await saveSession(myName, password);
      const user = { id: node.fingerprint(), username: myName };
      return r({ success: true, user });
    }
    case "auto_login": {
      // Resume the saved session if there is one → open the SAME persistent keystore identity, no
      // re-login. (Returns the UserInfo directly, or null to fall through to the login screen.)
      const s = await loadSession();
      if (!s) return r(null);
      try {
        node = await loadNode(s.password);
        myName = s.username;
        node.announce_self(myName);
        return r({ id: node.fingerprint(), username: myName });
      } catch {
        await clearSession(); // stale/bad session → forget it, show the login screen
        return r(null);
      }
    }
    case "logout":
      node = null;
      await clearSession();
      return r({ success: true });
    case "clear_saved_session":
      await clearSession();
      return r(undefined);
    case "adopt_linked_account":
      return r(undefined);
    case "my_id":
    case "account_id":
      if (!node) throw new Error("not signed in");
      return r(node.fingerprint());

    // --- encrypted peer DMs (account-addressed; peers from the gossiped mesh directory) ---
    case "list_peers": {
      const me = node?.fingerprint();
      // Every node in the mesh (directory) + any directly-handshook peers, minus self.
      return r(
        meshPeers()
          .filter((p) => p.id !== me)
          .map((p) => ({
            user_id: p.id,
            name: p.id.slice(0, 8),
            addr: "",
            post_office: false,
            account_id: p.id,
          })),
      );
    }
    case "list_accounts": {
      // Each mesh node is its own account; surface recently-seen nodes as DM accounts. Long-gone
      // entries (stale heartbeats in the append-only directory) are filtered out so the list
      // matches who's actually around — not every identity ever announced.
      const me = node?.fingerprint();
      const now = Date.now();
      return r(
        meshPeers()
          .filter((p) => p.id !== me)
          .filter(
            (p) =>
              p.last_seen_ms === undefined ||
              now - p.last_seen_ms < SHOW_WINDOW_MS,
          )
          .map((p) => ({
            account_id: p.id,
            names: [p.name || p.id.slice(0, 8)],
            device_count: 1,
          })),
      );
    }
    case "get_presence": {
      // Online iff the node's last presence heartbeat is recent — so a node that went offline
      // (no fresh heartbeat) correctly shows offline, instead of lingering "online" forever.
      const me = node?.fingerprint();
      const now = Date.now();
      const map: Record<string, { online: boolean; last_seen_secs: number }> =
        {};
      for (const p of meshPeers()) {
        if (p.id === me) continue;
        const age =
          p.last_seen_ms === undefined ? Infinity : now - p.last_seen_ms;
        map[p.id] = {
          online: age < ONLINE_WINDOW_MS,
          last_seen_secs: age === Infinity ? 0 : Math.round(age / 1000),
        };
      }
      return r(map);
    }
    case "account_history": {
      const peer = meshPeers().find((p) => p.id === String(args?.account));
      return r(peer ? dmHistoryWith(peer.ed25519, peer.x25519) : []);
    }
    case "send_to_account": {
      const peer = meshPeers().find((p) => p.id === String(args?.account));
      if (peer)
        await sendDmRatcheted(
          peer.ed25519,
          peer.x25519,
          String(args?.text ?? ""),
        );
      return r(undefined);
    }

    // Phase 1 smoke (kept for the boot self-check / tests).
    case "wasm_identity_fingerprint":
      return r((await loadWasm()).generate_identity_fingerprint());

    default:
      if (cmd in EMPTY_READS) return r(EMPTY_READS[cmd]);
      if (IGNORED_WRITES.has(cmd)) return r(undefined);
      throw new Error(
        `"${cmd}" is not available in the browser yet — the PWA messaging surface is still being built out (mobile-PWA plan). Use the desktop app for this action for now.`,
      );
  }
}
