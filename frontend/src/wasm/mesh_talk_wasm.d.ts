/* tslint:disable */
/* eslint-disable */

export class WasmNode {
    free(): void;
    [Symbol.dispose](): void;
    /**
     * Announce a relay endpoint (ws URL) into the gossiped relay directory. Idempotent per URL.
     * Gossiped by sync_all, so other nodes — including phones reached only through a hub — learn
     * this relay and can use it for failover.
     */
    announce_relay(url: string): void;
    /**
     * Announce this node's identity into the well-known directory conversation (presence). The
     * payload is the public ed25519+x25519 keys (identities are public). Idempotent: skips if we
     * already announced. Gossiped by sync_all, so everyone learns everyone — the discovery that
     * lets a node address (DM) any other node in the mesh.
     */
    announce_self(name: string): void;
    /**
     * The conversation ids present in the local event log, as hex JSON (introspection/tests).
     */
    conversation_ids(): string;
    /**
     * The directory: every identity announced into the mesh, as `[{id, ed25519, x25519}]` (hex).
     * Built from the gossiped directory conversation — the roster a node can DM anyone from.
     */
    directory(): string;
    /**
     * TEST-ONLY: this node's own latest presence event ([7;32]), bincode-encoded, hex. Another
     * node can `ingest_event` it to simulate pulling this node's presence over a sync.
     */
    directory_event_hex(): string;
    /**
     * Serialize + password-seal the full DM state — ratchet sessions (secret keys), decrypted
     * plaintext (ratchet messages can't be re-opened), and the peer roster — for IndexedDB, so a
     * DM conversation survives a reload intact. Sealed with the same password as the keystore.
     */
    dm_state_snapshot(password: string): Uint8Array;
    /**
     * This node's Ed25519 public key — a peer needs it (with the X25519 key) to ratchet DMs.
     */
    ed25519_public(): Uint8Array;
    /**
     * This device's fingerprint (user id).
     */
    fingerprint(): string;
    /**
     * Build a node with a persistent identity from a sealed keystore blob + password (vs `new`,
     * which generates a fresh identity each time). The conversation log starts empty; restore it
     * separately. Lets the PWA keep the same identity across reloads.
     */
    static from_keystore(blob: Uint8Array, password: string): WasmNode;
    /**
     * The conversation's messages as `HistoryItem`-shaped JSON (what the app's UI renders).
     */
    history_json(): string;
    /**
     * Append a serialized event to the log (what the sync does); used for direct event transfer.
     */
    ingest_event(event_bytes: Uint8Array): void;
    /**
     * TEST-ONLY: the user-ids this node would treat as DM peers (handshook peers + the gossiped
     * directory) — the exact set `sync_all_over_dc` builds DM conversations for.
     */
    known_identity_ids(): string;
    /**
     * Every relay endpoint announced into the mesh (deduped), as a JSON array of ws URL strings.
     * The basis for zero-config failover: a phone tries these when its current relay drops.
     */
    known_relay_urls(): string;
    /**
     * The conversation's messages, in order, as a JSON array of strings.
     */
    messages(): string;
    constructor();
    /**
     * Open a sealed DM addressed to us from a sender with the given X25519 public key. Errors if
     * it wasn't sealed to us or the sender doesn't match (the AEAD + sender binding reject it).
     */
    open_dm(sender_x25519: Uint8Array, envelope: Uint8Array): string;
    /**
     * Open a forward-secret DM from a peer. Bootstraps as responder on first contact; resolves a
     * simultaneous-init race by the user-id tie-break. Decrypts on a copy and persists the
     * advanced session ONLY on success (ratchet_decrypt mutates before AEAD auth, so a forged
     * message must not be able to corrupt the stored session).
     */
    open_dm_ratcheted(peer_ed25519: Uint8Array, peer_x25519: Uint8Array, wire: Uint8Array): string;
    /**
     * The discovered peers (from gateway handshakes) as JSON: `[{id, ed25519, x25519}]` (hex keys).
     */
    peer_roster(): string;
    /**
     * Open any of a peer's DM events present in the log but not yet decrypted (post-sync step).
     */
    process_dm_events_with(peer_ed25519: Uint8Array, peer_x25519: Uint8Array): void;
    /**
     * The DM conversation with `peer` as `HistoryItem`-shaped JSON (decrypted plaintext per event).
     */
    ratcheted_dm_history(peer_ed25519: Uint8Array, peer_x25519: Uint8Array): string;
    /**
     * Receive a serialized DM event from `peer`: open the ratchet payload ONCE (keeping the
     * plaintext for history), then file the event in the per-peer conversation.
     */
    receive_ratcheted_dm(peer_ed25519: Uint8Array, peer_x25519: Uint8Array, event_bytes: Uint8Array): void;
    /**
     * Restore events from a `snapshot` blob into this node's log (idempotent — duplicate or
     * out-of-order events are skipped). Returns the event count afterwards.
     */
    restore(blob: Uint8Array): number;
    /**
     * Restore the DM state from a sealed snapshot (replaces current sessions/plaintext/roster).
     */
    restore_dm_state(blob: Uint8Array, password: string): void;
    /**
     * Seal `text` as a private DM to a recipient's X25519 public key, returning the sealed
     * envelope. Per-recipient encrypted + sender-authenticated — reuses the mesh's `dm`
     * sealed-box (the same primitive the desktop node uses), not new crypto.
     */
    seal_dm(recipient_x25519: Uint8Array, text: string): Uint8Array;
    /**
     * Seal `text` as a forward-secret DM to a peer using the Double Ratchet (the same wire format
     * the desktop node speaks), returning the wire blob to sync. Establishes the session as
     * initiator on first use; advances + keeps the per-peer session. Reuses the core `ratchet`
     * crypto — bidirectional + forward-secret, unlike the one-shot `seal_dm` sealed-box.
     */
    seal_dm_ratcheted(peer_ed25519: Uint8Array, peer_x25519: Uint8Array, text: string): Uint8Array;
    /**
     * TEST-ONLY: seal a DM the way the NATIVE node does — wrapping the text in a MessageBody
     * (`MTB1`) frame before the ratchet — so a test can prove the receiver strips the frame and
     * surfaces the bare text (the "messages arrive with an MTB1 prefix" bug).
     */
    seal_framed_dm_ratcheted(peer_ed25519: Uint8Array, peer_x25519: Uint8Array, text: string): Uint8Array;
    /**
     * Append a message to the conversation; returns the event count afterwards.
     */
    send_message(text: string): number;
    /**
     * Send a forward-secret DM to `peer`: seal it with the ratchet, append it as an event to the
     * per-peer conversation, and remember the plaintext (own messages can't be ratchet-reopened).
     * Returns the serialized event to deliver to the peer (the gateway sync carries these).
     */
    send_ratcheted_dm(peer_ed25519: Uint8Array, peer_x25519: Uint8Array, text: string): Uint8Array;
    /**
     * Serialize the conversation's event log to bytes, for persistence (e.g. IndexedDB).
     */
    snapshot(): Uint8Array;
    /**
     * Gossip ALL conversations with the peer on the other end of `dc` (vs sync_dm_over_dc, which
     * syncs only the conversation with that peer). The initiator runs a sync round for every
     * conversation it knows (the presence + relay directories, its event log, + a DM conversation
     * per known identity); the responder serves whatever is requested. This is what lets a node
     * reach everyone through a hub: the hub gossips every conversation with each phone, so A→hub→B
     * routes transitively. Returns the peer's fingerprint.
     */
    sync_all_over_dc(dc: RTCDataChannel, initiator: boolean): Promise<string>;
    /**
     * Introspection (tests): the conversation ids this node would sync right now, as a hex-string
     * JSON array. Lets a test assert that even a fresh phone (empty log, never announced) ALWAYS
     * pulls the presence + relay directories — the "phone is an equal node" guarantee.
     */
    sync_conv_set_hex(): string;
    /**
     * Sync the DM conversation with the peer on the other end of `dc` over the gateway: run the
     * Noise handshake, take the peer's AUTHENTICATED identity from it, record the peer in the
     * roster, sync the per-peer DM conversation, and open any newly-arrived messages from them.
     * Returns the peer's fingerprint. This is the encrypted-DM counterpart to `sync_over_dc`.
     */
    sync_dm_over_dc(dc: RTCDataChannel, initiator: boolean): Promise<string>;
    /**
     * Sync this node's log with a peer over a data channel: open the Noise channel (initiator =
     * offerer), then reconcile the conversation's event log. After this, messages sent on either
     * side are present on both. The initiator drives one round + closes; the responder serves.
     */
    sync_over_dc(dc: RTCDataChannel, initiator: boolean): Promise<void>;
    /**
     * This node's X25519 public key — a peer seals DMs to it (and authenticates DMs from us).
     */
    x25519_public(): Uint8Array;
}

/**
 * Generate a fresh device identity, seal it under `password` (PBKDF2 + AES-256-GCM), and return
 * the encrypted blob to persist (e.g. in IndexedDB). Plaintext keys never leave wasm.
 */
export function create_identity_keystore(password: string): Uint8Array;

/**
 * Generate a fresh device identity and return its fingerprint (user id).
 *
 * Smoke test: drives dalek Ed25519/X25519 key generation — i.e. the wasm randomness path
 * (`getrandom/js`) — plus the fingerprint hashing, so a non-empty return from a browser/Node
 * wasm runtime proves the protocol crypto works there.
 */
export function generate_identity_fingerprint(): string;

export function init(): void;

/**
 * Run a full mesh `SecureChannel` (Noise XX) handshake in-wasm over an in-memory duplex, then
 * send `message` through it and return what the other end received.
 *
 * Proves the authenticated, encrypted channel — the same one the browser gateway runs over a
 * WebRTC data channel — actually *executes* in a JS/wasm runtime (the snow handshake, AES-GCM,
 * and the length-prefixed framing), not merely compiles. The two endpoints are driven
 * concurrently on the single wasm task via `join!`.
 */
export function noise_loopback(message: string): Promise<string>;

/**
 * Open a keystore blob with `password`, returning the device fingerprint (user id). Errors if
 * the password is wrong or the blob is corrupt/tampered (AEAD rejects it).
 */
export function open_identity_keystore(blob: Uint8Array, password: string): string;

/**
 * Run the mesh `SecureChannel` (Noise XX) over a browser `RTCDataChannel` and return the
 * authenticated peer's fingerprint. `initiator` is the offerer side. This is the browser
 * counterpart to the node gateway: in production the PWA runs this over the WebRTC data channel
 * to the node, then syncs the mesh event log through it.
 */
export function secure_handshake_over_dc(dc: RTCDataChannel, initiator: boolean): Promise<string>;

/**
 * Run the mesh event-log sync over a data channel and return how many events the local store
 * holds afterwards. The initiator drives `request_round` (and closes the channel when done so
 * the responder's serve loop stops); the responder serves rounds. With `seed`, the store starts
 * with one event. This is the in-wasm proof that the PWA can sync the mesh over WebRTC: seed one
 * side and both converge to the same event.
 */
export function sync_demo_over_dc(dc: RTCDataChannel, initiator: boolean, seed: boolean): Promise<number>;

export type InitInput = RequestInfo | URL | Response | BufferSource | WebAssembly.Module;

export interface InitOutput {
    readonly memory: WebAssembly.Memory;
    readonly generate_identity_fingerprint: () => [number, number];
    readonly noise_loopback: (a: number, b: number) => any;
    readonly secure_handshake_over_dc: (a: any, b: number) => any;
    readonly sync_demo_over_dc: (a: any, b: number, c: number) => any;
    readonly __wbg_wasmnode_free: (a: number, b: number) => void;
    readonly wasmnode_new: () => number;
    readonly wasmnode_from_keystore: (a: number, b: number, c: number, d: number) => [number, number, number];
    readonly wasmnode_fingerprint: (a: number) => [number, number];
    readonly wasmnode_x25519_public: (a: number) => [number, number];
    readonly wasmnode_ed25519_public: (a: number) => [number, number];
    readonly wasmnode_seal_dm: (a: number, b: number, c: number, d: number, e: number) => [number, number, number, number];
    readonly wasmnode_open_dm: (a: number, b: number, c: number, d: number, e: number) => [number, number, number, number];
    readonly wasmnode_seal_dm_ratcheted: (a: number, b: number, c: number, d: number, e: number, f: number, g: number) => [number, number, number, number];
    readonly wasmnode_seal_framed_dm_ratcheted: (a: number, b: number, c: number, d: number, e: number, f: number, g: number) => [number, number, number, number];
    readonly wasmnode_open_dm_ratcheted: (a: number, b: number, c: number, d: number, e: number, f: number, g: number) => [number, number, number, number];
    readonly wasmnode_send_ratcheted_dm: (a: number, b: number, c: number, d: number, e: number, f: number, g: number) => [number, number, number, number];
    readonly wasmnode_receive_ratcheted_dm: (a: number, b: number, c: number, d: number, e: number, f: number, g: number) => [number, number];
    readonly wasmnode_ratcheted_dm_history: (a: number, b: number, c: number, d: number, e: number) => [number, number, number, number];
    readonly wasmnode_sync_dm_over_dc: (a: number, b: any, c: number) => any;
    readonly wasmnode_sync_conv_set_hex: (a: number) => [number, number, number, number];
    readonly wasmnode_sync_all_over_dc: (a: number, b: any, c: number) => any;
    readonly wasmnode_conversation_ids: (a: number) => [number, number, number, number];
    readonly wasmnode_announce_self: (a: number, b: number, c: number) => [number, number];
    readonly wasmnode_directory_event_hex: (a: number) => [number, number, number, number];
    readonly wasmnode_known_identity_ids: (a: number) => [number, number, number, number];
    readonly wasmnode_directory: (a: number) => [number, number, number, number];
    readonly wasmnode_announce_relay: (a: number, b: number, c: number) => [number, number];
    readonly wasmnode_known_relay_urls: (a: number) => [number, number, number, number];
    readonly wasmnode_peer_roster: (a: number) => [number, number, number, number];
    readonly wasmnode_ingest_event: (a: number, b: number, c: number) => [number, number];
    readonly wasmnode_process_dm_events_with: (a: number, b: number, c: number, d: number, e: number) => [number, number];
    readonly wasmnode_dm_state_snapshot: (a: number, b: number, c: number) => [number, number, number, number];
    readonly wasmnode_restore_dm_state: (a: number, b: number, c: number, d: number, e: number) => [number, number];
    readonly wasmnode_send_message: (a: number, b: number, c: number) => [number, number, number];
    readonly wasmnode_messages: (a: number) => [number, number, number, number];
    readonly wasmnode_history_json: (a: number) => [number, number, number, number];
    readonly wasmnode_snapshot: (a: number) => [number, number, number, number];
    readonly wasmnode_restore: (a: number, b: number, c: number) => [number, number, number];
    readonly wasmnode_sync_over_dc: (a: number, b: any, c: number) => any;
    readonly create_identity_keystore: (a: number, b: number) => [number, number, number, number];
    readonly open_identity_keystore: (a: number, b: number, c: number, d: number) => [number, number, number, number];
    readonly init: () => void;
    readonly wasm_bindgen__convert__closures_____invoke__h1c47d85015abd8d5: (a: number, b: number, c: any) => [number, number];
    readonly wasm_bindgen__convert__closures_____invoke__h3cd6c576a5e62e5f: (a: number, b: number, c: any, d: any) => void;
    readonly wasm_bindgen__convert__closures_____invoke__ha64b36242e1c9513: (a: number, b: number, c: any) => void;
    readonly wasm_bindgen__convert__closures_____invoke__h25b09cbd1f5fd7c4: (a: number, b: number) => void;
    readonly __wbindgen_exn_store: (a: number) => void;
    readonly __externref_table_alloc: () => number;
    readonly __wbindgen_externrefs: WebAssembly.Table;
    readonly __wbindgen_free: (a: number, b: number, c: number) => void;
    readonly __wbindgen_malloc: (a: number, b: number) => number;
    readonly __wbindgen_realloc: (a: number, b: number, c: number, d: number) => number;
    readonly __wbindgen_destroy_closure: (a: number, b: number) => void;
    readonly __externref_table_dealloc: (a: number) => void;
    readonly __wbindgen_start: () => void;
}

export type SyncInitInput = BufferSource | WebAssembly.Module;

/**
 * Instantiates the given `module`, which can either be bytes or
 * a precompiled `WebAssembly.Module`.
 *
 * @param {{ module: SyncInitInput }} module - Passing `SyncInitInput` directly is deprecated.
 *
 * @returns {InitOutput}
 */
export function initSync(module: { module: SyncInitInput } | SyncInitInput): InitOutput;

/**
 * If `module_or_path` is {RequestInfo} or {URL}, makes a request and
 * for everything else, calls `WebAssembly.instantiate` directly.
 *
 * @param {{ module_or_path: InitInput | Promise<InitInput> }} module_or_path - Passing `InitInput` directly is deprecated.
 *
 * @returns {Promise<InitOutput>}
 */
export default function __wbg_init (module_or_path?: { module_or_path: InitInput | Promise<InitInput> } | InitInput | Promise<InitInput>): Promise<InitOutput>;
