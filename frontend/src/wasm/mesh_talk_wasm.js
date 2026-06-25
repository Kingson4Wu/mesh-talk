/* @ts-self-types="./mesh_talk_wasm.d.ts" */

export class WasmNode {
    static __wrap(ptr) {
        const obj = Object.create(WasmNode.prototype);
        obj.__wbg_ptr = ptr;
        WasmNodeFinalization.register(obj, obj.__wbg_ptr, obj);
        return obj;
    }
    __destroy_into_raw() {
        const ptr = this.__wbg_ptr;
        this.__wbg_ptr = 0;
        WasmNodeFinalization.unregister(this);
        return ptr;
    }
    free() {
        const ptr = this.__destroy_into_raw();
        wasm.__wbg_wasmnode_free(ptr, 0);
    }
    /**
     * Announce a relay endpoint (ws URL) into the gossiped relay directory. Idempotent per URL.
     * Gossiped by sync_all, so other nodes — including phones reached only through a hub — learn
     * this relay and can use it for failover.
     * @param {string} url
     */
    announce_relay(url) {
        const ptr0 = passStringToWasm0(url, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len0 = WASM_VECTOR_LEN;
        const ret = wasm.wasmnode_announce_relay(this.__wbg_ptr, ptr0, len0);
        if (ret[1]) {
            throw takeFromExternrefTable0(ret[0]);
        }
    }
    /**
     * Announce this node's identity into the well-known directory conversation (presence). The
     * payload is the public ed25519+x25519 keys (identities are public). Idempotent: skips if we
     * already announced. Gossiped by sync_all, so everyone learns everyone — the discovery that
     * lets a node address (DM) any other node in the mesh.
     * @param {string} name
     */
    announce_self(name) {
        const ptr0 = passStringToWasm0(name, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len0 = WASM_VECTOR_LEN;
        const ret = wasm.wasmnode_announce_self(this.__wbg_ptr, ptr0, len0);
        if (ret[1]) {
            throw takeFromExternrefTable0(ret[0]);
        }
    }
    /**
     * The conversation ids present in the local event log, as hex JSON (introspection/tests).
     * @returns {string}
     */
    conversation_ids() {
        let deferred2_0;
        let deferred2_1;
        try {
            const ret = wasm.wasmnode_conversation_ids(this.__wbg_ptr);
            var ptr1 = ret[0];
            var len1 = ret[1];
            if (ret[3]) {
                ptr1 = 0; len1 = 0;
                throw takeFromExternrefTable0(ret[2]);
            }
            deferred2_0 = ptr1;
            deferred2_1 = len1;
            return getStringFromWasm0(ptr1, len1);
        } finally {
            wasm.__wbindgen_free(deferred2_0, deferred2_1, 1);
        }
    }
    /**
     * The directory: every identity announced into the mesh, as `[{id, ed25519, x25519}]` (hex).
     * Built from the gossiped directory conversation — the roster a node can DM anyone from.
     * @returns {string}
     */
    directory() {
        let deferred2_0;
        let deferred2_1;
        try {
            const ret = wasm.wasmnode_directory(this.__wbg_ptr);
            var ptr1 = ret[0];
            var len1 = ret[1];
            if (ret[3]) {
                ptr1 = 0; len1 = 0;
                throw takeFromExternrefTable0(ret[2]);
            }
            deferred2_0 = ptr1;
            deferred2_1 = len1;
            return getStringFromWasm0(ptr1, len1);
        } finally {
            wasm.__wbindgen_free(deferred2_0, deferred2_1, 1);
        }
    }
    /**
     * TEST-ONLY: this node's own latest presence event ([7;32]), bincode-encoded, hex. Another
     * node can `ingest_event` it to simulate pulling this node's presence over a sync.
     * @returns {string}
     */
    directory_event_hex() {
        let deferred2_0;
        let deferred2_1;
        try {
            const ret = wasm.wasmnode_directory_event_hex(this.__wbg_ptr);
            var ptr1 = ret[0];
            var len1 = ret[1];
            if (ret[3]) {
                ptr1 = 0; len1 = 0;
                throw takeFromExternrefTable0(ret[2]);
            }
            deferred2_0 = ptr1;
            deferred2_1 = len1;
            return getStringFromWasm0(ptr1, len1);
        } finally {
            wasm.__wbindgen_free(deferred2_0, deferred2_1, 1);
        }
    }
    /**
     * Serialize + password-seal the full DM state — ratchet sessions (secret keys), decrypted
     * plaintext (ratchet messages can't be re-opened), and the peer roster — for IndexedDB, so a
     * DM conversation survives a reload intact. Sealed with the same password as the keystore.
     * @param {string} password
     * @returns {Uint8Array}
     */
    dm_state_snapshot(password) {
        const ptr0 = passStringToWasm0(password, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len0 = WASM_VECTOR_LEN;
        const ret = wasm.wasmnode_dm_state_snapshot(this.__wbg_ptr, ptr0, len0);
        if (ret[3]) {
            throw takeFromExternrefTable0(ret[2]);
        }
        var v2 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
        wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
        return v2;
    }
    /**
     * This node's Ed25519 public key — a peer needs it (with the X25519 key) to ratchet DMs.
     * @returns {Uint8Array}
     */
    ed25519_public() {
        const ret = wasm.wasmnode_ed25519_public(this.__wbg_ptr);
        var v1 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
        wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
        return v1;
    }
    /**
     * This device's fingerprint (user id).
     * @returns {string}
     */
    fingerprint() {
        let deferred1_0;
        let deferred1_1;
        try {
            const ret = wasm.wasmnode_fingerprint(this.__wbg_ptr);
            deferred1_0 = ret[0];
            deferred1_1 = ret[1];
            return getStringFromWasm0(ret[0], ret[1]);
        } finally {
            wasm.__wbindgen_free(deferred1_0, deferred1_1, 1);
        }
    }
    /**
     * Build a node with a persistent identity from a sealed keystore blob + password (vs `new`,
     * which generates a fresh identity each time). The conversation log starts empty; restore it
     * separately. Lets the PWA keep the same identity across reloads.
     * @param {Uint8Array} blob
     * @param {string} password
     * @returns {WasmNode}
     */
    static from_keystore(blob, password) {
        const ptr0 = passArray8ToWasm0(blob, wasm.__wbindgen_malloc);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(password, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len1 = WASM_VECTOR_LEN;
        const ret = wasm.wasmnode_from_keystore(ptr0, len0, ptr1, len1);
        if (ret[2]) {
            throw takeFromExternrefTable0(ret[1]);
        }
        return WasmNode.__wrap(ret[0]);
    }
    /**
     * The conversation's messages as `HistoryItem`-shaped JSON (what the app's UI renders).
     * @returns {string}
     */
    history_json() {
        let deferred2_0;
        let deferred2_1;
        try {
            const ret = wasm.wasmnode_history_json(this.__wbg_ptr);
            var ptr1 = ret[0];
            var len1 = ret[1];
            if (ret[3]) {
                ptr1 = 0; len1 = 0;
                throw takeFromExternrefTable0(ret[2]);
            }
            deferred2_0 = ptr1;
            deferred2_1 = len1;
            return getStringFromWasm0(ptr1, len1);
        } finally {
            wasm.__wbindgen_free(deferred2_0, deferred2_1, 1);
        }
    }
    /**
     * Append a serialized event to the log (what the sync does); used for direct event transfer.
     * @param {Uint8Array} event_bytes
     */
    ingest_event(event_bytes) {
        const ptr0 = passArray8ToWasm0(event_bytes, wasm.__wbindgen_malloc);
        const len0 = WASM_VECTOR_LEN;
        const ret = wasm.wasmnode_ingest_event(this.__wbg_ptr, ptr0, len0);
        if (ret[1]) {
            throw takeFromExternrefTable0(ret[0]);
        }
    }
    /**
     * TEST-ONLY: the user-ids this node would treat as DM peers (handshook peers + the gossiped
     * directory) — the exact set `sync_all_over_dc` builds DM conversations for.
     * @returns {string}
     */
    known_identity_ids() {
        let deferred2_0;
        let deferred2_1;
        try {
            const ret = wasm.wasmnode_known_identity_ids(this.__wbg_ptr);
            var ptr1 = ret[0];
            var len1 = ret[1];
            if (ret[3]) {
                ptr1 = 0; len1 = 0;
                throw takeFromExternrefTable0(ret[2]);
            }
            deferred2_0 = ptr1;
            deferred2_1 = len1;
            return getStringFromWasm0(ptr1, len1);
        } finally {
            wasm.__wbindgen_free(deferred2_0, deferred2_1, 1);
        }
    }
    /**
     * Every relay endpoint announced into the mesh (deduped), as a JSON array of ws URL strings.
     * The basis for zero-config failover: a phone tries these when its current relay drops.
     * @returns {string}
     */
    known_relay_urls() {
        let deferred2_0;
        let deferred2_1;
        try {
            const ret = wasm.wasmnode_known_relay_urls(this.__wbg_ptr);
            var ptr1 = ret[0];
            var len1 = ret[1];
            if (ret[3]) {
                ptr1 = 0; len1 = 0;
                throw takeFromExternrefTable0(ret[2]);
            }
            deferred2_0 = ptr1;
            deferred2_1 = len1;
            return getStringFromWasm0(ptr1, len1);
        } finally {
            wasm.__wbindgen_free(deferred2_0, deferred2_1, 1);
        }
    }
    /**
     * The conversation's messages, in order, as a JSON array of strings.
     * @returns {string}
     */
    messages() {
        let deferred2_0;
        let deferred2_1;
        try {
            const ret = wasm.wasmnode_messages(this.__wbg_ptr);
            var ptr1 = ret[0];
            var len1 = ret[1];
            if (ret[3]) {
                ptr1 = 0; len1 = 0;
                throw takeFromExternrefTable0(ret[2]);
            }
            deferred2_0 = ptr1;
            deferred2_1 = len1;
            return getStringFromWasm0(ptr1, len1);
        } finally {
            wasm.__wbindgen_free(deferred2_0, deferred2_1, 1);
        }
    }
    constructor() {
        const ret = wasm.wasmnode_new();
        this.__wbg_ptr = ret;
        WasmNodeFinalization.register(this, this.__wbg_ptr, this);
        return this;
    }
    /**
     * Open a sealed DM addressed to us from a sender with the given X25519 public key. Errors if
     * it wasn't sealed to us or the sender doesn't match (the AEAD + sender binding reject it).
     * @param {Uint8Array} sender_x25519
     * @param {Uint8Array} envelope
     * @returns {string}
     */
    open_dm(sender_x25519, envelope) {
        let deferred4_0;
        let deferred4_1;
        try {
            const ptr0 = passArray8ToWasm0(sender_x25519, wasm.__wbindgen_malloc);
            const len0 = WASM_VECTOR_LEN;
            const ptr1 = passArray8ToWasm0(envelope, wasm.__wbindgen_malloc);
            const len1 = WASM_VECTOR_LEN;
            const ret = wasm.wasmnode_open_dm(this.__wbg_ptr, ptr0, len0, ptr1, len1);
            var ptr3 = ret[0];
            var len3 = ret[1];
            if (ret[3]) {
                ptr3 = 0; len3 = 0;
                throw takeFromExternrefTable0(ret[2]);
            }
            deferred4_0 = ptr3;
            deferred4_1 = len3;
            return getStringFromWasm0(ptr3, len3);
        } finally {
            wasm.__wbindgen_free(deferred4_0, deferred4_1, 1);
        }
    }
    /**
     * Open a forward-secret DM from a peer. Bootstraps as responder on first contact; resolves a
     * simultaneous-init race by the user-id tie-break. Decrypts on a copy and persists the
     * advanced session ONLY on success (ratchet_decrypt mutates before AEAD auth, so a forged
     * message must not be able to corrupt the stored session).
     * @param {Uint8Array} peer_ed25519
     * @param {Uint8Array} peer_x25519
     * @param {Uint8Array} wire
     * @returns {string}
     */
    open_dm_ratcheted(peer_ed25519, peer_x25519, wire) {
        let deferred5_0;
        let deferred5_1;
        try {
            const ptr0 = passArray8ToWasm0(peer_ed25519, wasm.__wbindgen_malloc);
            const len0 = WASM_VECTOR_LEN;
            const ptr1 = passArray8ToWasm0(peer_x25519, wasm.__wbindgen_malloc);
            const len1 = WASM_VECTOR_LEN;
            const ptr2 = passArray8ToWasm0(wire, wasm.__wbindgen_malloc);
            const len2 = WASM_VECTOR_LEN;
            const ret = wasm.wasmnode_open_dm_ratcheted(this.__wbg_ptr, ptr0, len0, ptr1, len1, ptr2, len2);
            var ptr4 = ret[0];
            var len4 = ret[1];
            if (ret[3]) {
                ptr4 = 0; len4 = 0;
                throw takeFromExternrefTable0(ret[2]);
            }
            deferred5_0 = ptr4;
            deferred5_1 = len4;
            return getStringFromWasm0(ptr4, len4);
        } finally {
            wasm.__wbindgen_free(deferred5_0, deferred5_1, 1);
        }
    }
    /**
     * The discovered peers (from gateway handshakes) as JSON: `[{id, ed25519, x25519}]` (hex keys).
     * @returns {string}
     */
    peer_roster() {
        let deferred2_0;
        let deferred2_1;
        try {
            const ret = wasm.wasmnode_peer_roster(this.__wbg_ptr);
            var ptr1 = ret[0];
            var len1 = ret[1];
            if (ret[3]) {
                ptr1 = 0; len1 = 0;
                throw takeFromExternrefTable0(ret[2]);
            }
            deferred2_0 = ptr1;
            deferred2_1 = len1;
            return getStringFromWasm0(ptr1, len1);
        } finally {
            wasm.__wbindgen_free(deferred2_0, deferred2_1, 1);
        }
    }
    /**
     * Open any of a peer's DM events present in the log but not yet decrypted (post-sync step).
     * @param {Uint8Array} peer_ed25519
     * @param {Uint8Array} peer_x25519
     */
    process_dm_events_with(peer_ed25519, peer_x25519) {
        const ptr0 = passArray8ToWasm0(peer_ed25519, wasm.__wbindgen_malloc);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passArray8ToWasm0(peer_x25519, wasm.__wbindgen_malloc);
        const len1 = WASM_VECTOR_LEN;
        const ret = wasm.wasmnode_process_dm_events_with(this.__wbg_ptr, ptr0, len0, ptr1, len1);
        if (ret[1]) {
            throw takeFromExternrefTable0(ret[0]);
        }
    }
    /**
     * The DM conversation with `peer` as `HistoryItem`-shaped JSON (decrypted plaintext per event).
     * @param {Uint8Array} peer_ed25519
     * @param {Uint8Array} peer_x25519
     * @returns {string}
     */
    ratcheted_dm_history(peer_ed25519, peer_x25519) {
        let deferred4_0;
        let deferred4_1;
        try {
            const ptr0 = passArray8ToWasm0(peer_ed25519, wasm.__wbindgen_malloc);
            const len0 = WASM_VECTOR_LEN;
            const ptr1 = passArray8ToWasm0(peer_x25519, wasm.__wbindgen_malloc);
            const len1 = WASM_VECTOR_LEN;
            const ret = wasm.wasmnode_ratcheted_dm_history(this.__wbg_ptr, ptr0, len0, ptr1, len1);
            var ptr3 = ret[0];
            var len3 = ret[1];
            if (ret[3]) {
                ptr3 = 0; len3 = 0;
                throw takeFromExternrefTable0(ret[2]);
            }
            deferred4_0 = ptr3;
            deferred4_1 = len3;
            return getStringFromWasm0(ptr3, len3);
        } finally {
            wasm.__wbindgen_free(deferred4_0, deferred4_1, 1);
        }
    }
    /**
     * Receive a serialized DM event from `peer`: open the ratchet payload ONCE (keeping the
     * plaintext for history), then file the event in the per-peer conversation.
     * @param {Uint8Array} peer_ed25519
     * @param {Uint8Array} peer_x25519
     * @param {Uint8Array} event_bytes
     */
    receive_ratcheted_dm(peer_ed25519, peer_x25519, event_bytes) {
        const ptr0 = passArray8ToWasm0(peer_ed25519, wasm.__wbindgen_malloc);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passArray8ToWasm0(peer_x25519, wasm.__wbindgen_malloc);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passArray8ToWasm0(event_bytes, wasm.__wbindgen_malloc);
        const len2 = WASM_VECTOR_LEN;
        const ret = wasm.wasmnode_receive_ratcheted_dm(this.__wbg_ptr, ptr0, len0, ptr1, len1, ptr2, len2);
        if (ret[1]) {
            throw takeFromExternrefTable0(ret[0]);
        }
    }
    /**
     * Restore events from a `snapshot` blob into this node's log (idempotent — duplicate or
     * out-of-order events are skipped). Returns the event count afterwards.
     * @param {Uint8Array} blob
     * @returns {number}
     */
    restore(blob) {
        const ptr0 = passArray8ToWasm0(blob, wasm.__wbindgen_malloc);
        const len0 = WASM_VECTOR_LEN;
        const ret = wasm.wasmnode_restore(this.__wbg_ptr, ptr0, len0);
        if (ret[2]) {
            throw takeFromExternrefTable0(ret[1]);
        }
        return ret[0] >>> 0;
    }
    /**
     * Restore the DM state from a sealed snapshot (replaces current sessions/plaintext/roster).
     * @param {Uint8Array} blob
     * @param {string} password
     */
    restore_dm_state(blob, password) {
        const ptr0 = passArray8ToWasm0(blob, wasm.__wbindgen_malloc);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(password, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len1 = WASM_VECTOR_LEN;
        const ret = wasm.wasmnode_restore_dm_state(this.__wbg_ptr, ptr0, len0, ptr1, len1);
        if (ret[1]) {
            throw takeFromExternrefTable0(ret[0]);
        }
    }
    /**
     * Seal `text` as a private DM to a recipient's X25519 public key, returning the sealed
     * envelope. Per-recipient encrypted + sender-authenticated — reuses the mesh's `dm`
     * sealed-box (the same primitive the desktop node uses), not new crypto.
     * @param {Uint8Array} recipient_x25519
     * @param {string} text
     * @returns {Uint8Array}
     */
    seal_dm(recipient_x25519, text) {
        const ptr0 = passArray8ToWasm0(recipient_x25519, wasm.__wbindgen_malloc);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(text, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len1 = WASM_VECTOR_LEN;
        const ret = wasm.wasmnode_seal_dm(this.__wbg_ptr, ptr0, len0, ptr1, len1);
        if (ret[3]) {
            throw takeFromExternrefTable0(ret[2]);
        }
        var v3 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
        wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
        return v3;
    }
    /**
     * Seal `text` as a forward-secret DM to a peer using the Double Ratchet (the same wire format
     * the desktop node speaks), returning the wire blob to sync. Establishes the session as
     * initiator on first use; advances + keeps the per-peer session. Reuses the core `ratchet`
     * crypto — bidirectional + forward-secret, unlike the one-shot `seal_dm` sealed-box.
     * @param {Uint8Array} peer_ed25519
     * @param {Uint8Array} peer_x25519
     * @param {string} text
     * @returns {Uint8Array}
     */
    seal_dm_ratcheted(peer_ed25519, peer_x25519, text) {
        const ptr0 = passArray8ToWasm0(peer_ed25519, wasm.__wbindgen_malloc);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passArray8ToWasm0(peer_x25519, wasm.__wbindgen_malloc);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(text, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len2 = WASM_VECTOR_LEN;
        const ret = wasm.wasmnode_seal_dm_ratcheted(this.__wbg_ptr, ptr0, len0, ptr1, len1, ptr2, len2);
        if (ret[3]) {
            throw takeFromExternrefTable0(ret[2]);
        }
        var v4 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
        wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
        return v4;
    }
    /**
     * TEST-ONLY: seal a DM the way the NATIVE node does — wrapping the text in a MessageBody
     * (`MTB1`) frame before the ratchet — so a test can prove the receiver strips the frame and
     * surfaces the bare text (the "messages arrive with an MTB1 prefix" bug).
     * @param {Uint8Array} peer_ed25519
     * @param {Uint8Array} peer_x25519
     * @param {string} text
     * @returns {Uint8Array}
     */
    seal_framed_dm_ratcheted(peer_ed25519, peer_x25519, text) {
        const ptr0 = passArray8ToWasm0(peer_ed25519, wasm.__wbindgen_malloc);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passArray8ToWasm0(peer_x25519, wasm.__wbindgen_malloc);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(text, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len2 = WASM_VECTOR_LEN;
        const ret = wasm.wasmnode_seal_framed_dm_ratcheted(this.__wbg_ptr, ptr0, len0, ptr1, len1, ptr2, len2);
        if (ret[3]) {
            throw takeFromExternrefTable0(ret[2]);
        }
        var v4 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
        wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
        return v4;
    }
    /**
     * Append a message to the conversation; returns the event count afterwards.
     * @param {string} text
     * @returns {number}
     */
    send_message(text) {
        const ptr0 = passStringToWasm0(text, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len0 = WASM_VECTOR_LEN;
        const ret = wasm.wasmnode_send_message(this.__wbg_ptr, ptr0, len0);
        if (ret[2]) {
            throw takeFromExternrefTable0(ret[1]);
        }
        return ret[0] >>> 0;
    }
    /**
     * Send a forward-secret DM to `peer`: seal it with the ratchet, append it as an event to the
     * per-peer conversation, and remember the plaintext (own messages can't be ratchet-reopened).
     * Returns the serialized event to deliver to the peer (the gateway sync carries these).
     * @param {Uint8Array} peer_ed25519
     * @param {Uint8Array} peer_x25519
     * @param {string} text
     * @returns {Uint8Array}
     */
    send_ratcheted_dm(peer_ed25519, peer_x25519, text) {
        const ptr0 = passArray8ToWasm0(peer_ed25519, wasm.__wbindgen_malloc);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passArray8ToWasm0(peer_x25519, wasm.__wbindgen_malloc);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(text, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len2 = WASM_VECTOR_LEN;
        const ret = wasm.wasmnode_send_ratcheted_dm(this.__wbg_ptr, ptr0, len0, ptr1, len1, ptr2, len2);
        if (ret[3]) {
            throw takeFromExternrefTable0(ret[2]);
        }
        var v4 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
        wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
        return v4;
    }
    /**
     * Serialize the conversation's event log to bytes, for persistence (e.g. IndexedDB).
     * @returns {Uint8Array}
     */
    snapshot() {
        const ret = wasm.wasmnode_snapshot(this.__wbg_ptr);
        if (ret[3]) {
            throw takeFromExternrefTable0(ret[2]);
        }
        var v1 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
        wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
        return v1;
    }
    /**
     * Gossip ALL conversations with the peer on the other end of `dc` (vs sync_dm_over_dc, which
     * syncs only the conversation with that peer). The initiator runs a sync round for every
     * conversation it knows (the presence + relay directories, its event log, + a DM conversation
     * per known identity); the responder serves whatever is requested. This is what lets a node
     * reach everyone through a hub: the hub gossips every conversation with each phone, so A→hub→B
     * routes transitively. Returns the peer's fingerprint.
     * @param {RTCDataChannel} dc
     * @param {boolean} initiator
     * @returns {Promise<string>}
     */
    sync_all_over_dc(dc, initiator) {
        const ret = wasm.wasmnode_sync_all_over_dc(this.__wbg_ptr, dc, initiator);
        return ret;
    }
    /**
     * Introspection (tests): the conversation ids this node would sync right now, as a hex-string
     * JSON array. Lets a test assert that even a fresh phone (empty log, never announced) ALWAYS
     * pulls the presence + relay directories — the "phone is an equal node" guarantee.
     * @returns {string}
     */
    sync_conv_set_hex() {
        let deferred2_0;
        let deferred2_1;
        try {
            const ret = wasm.wasmnode_sync_conv_set_hex(this.__wbg_ptr);
            var ptr1 = ret[0];
            var len1 = ret[1];
            if (ret[3]) {
                ptr1 = 0; len1 = 0;
                throw takeFromExternrefTable0(ret[2]);
            }
            deferred2_0 = ptr1;
            deferred2_1 = len1;
            return getStringFromWasm0(ptr1, len1);
        } finally {
            wasm.__wbindgen_free(deferred2_0, deferred2_1, 1);
        }
    }
    /**
     * Sync the DM conversation with the peer on the other end of `dc` over the gateway: run the
     * Noise handshake, take the peer's AUTHENTICATED identity from it, record the peer in the
     * roster, sync the per-peer DM conversation, and open any newly-arrived messages from them.
     * Returns the peer's fingerprint. This is the encrypted-DM counterpart to `sync_over_dc`.
     * @param {RTCDataChannel} dc
     * @param {boolean} initiator
     * @returns {Promise<string>}
     */
    sync_dm_over_dc(dc, initiator) {
        const ret = wasm.wasmnode_sync_dm_over_dc(this.__wbg_ptr, dc, initiator);
        return ret;
    }
    /**
     * Sync this node's log with a peer over a data channel: open the Noise channel (initiator =
     * offerer), then reconcile the conversation's event log. After this, messages sent on either
     * side are present on both. The initiator drives one round + closes; the responder serves.
     * @param {RTCDataChannel} dc
     * @param {boolean} initiator
     * @returns {Promise<void>}
     */
    sync_over_dc(dc, initiator) {
        const ret = wasm.wasmnode_sync_over_dc(this.__wbg_ptr, dc, initiator);
        return ret;
    }
    /**
     * This node's X25519 public key — a peer seals DMs to it (and authenticates DMs from us).
     * @returns {Uint8Array}
     */
    x25519_public() {
        const ret = wasm.wasmnode_x25519_public(this.__wbg_ptr);
        var v1 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
        wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
        return v1;
    }
}
if (Symbol.dispose) WasmNode.prototype[Symbol.dispose] = WasmNode.prototype.free;

/**
 * Generate a fresh device identity, seal it under `password` (PBKDF2 + AES-256-GCM), and return
 * the encrypted blob to persist (e.g. in IndexedDB). Plaintext keys never leave wasm.
 * @param {string} password
 * @returns {Uint8Array}
 */
export function create_identity_keystore(password) {
    const ptr0 = passStringToWasm0(password, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ret = wasm.create_identity_keystore(ptr0, len0);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v2 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v2;
}

/**
 * Generate a fresh device identity and return its fingerprint (user id).
 *
 * Smoke test: drives dalek Ed25519/X25519 key generation — i.e. the wasm randomness path
 * (`getrandom/js`) — plus the fingerprint hashing, so a non-empty return from a browser/Node
 * wasm runtime proves the protocol crypto works there.
 * @returns {string}
 */
export function generate_identity_fingerprint() {
    let deferred1_0;
    let deferred1_1;
    try {
        const ret = wasm.generate_identity_fingerprint();
        deferred1_0 = ret[0];
        deferred1_1 = ret[1];
        return getStringFromWasm0(ret[0], ret[1]);
    } finally {
        wasm.__wbindgen_free(deferred1_0, deferred1_1, 1);
    }
}

export function init() {
    wasm.init();
}

/**
 * Run a full mesh `SecureChannel` (Noise XX) handshake in-wasm over an in-memory duplex, then
 * send `message` through it and return what the other end received.
 *
 * Proves the authenticated, encrypted channel — the same one the browser gateway runs over a
 * WebRTC data channel — actually *executes* in a JS/wasm runtime (the snow handshake, AES-GCM,
 * and the length-prefixed framing), not merely compiles. The two endpoints are driven
 * concurrently on the single wasm task via `join!`.
 * @param {string} message
 * @returns {Promise<string>}
 */
export function noise_loopback(message) {
    const ptr0 = passStringToWasm0(message, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ret = wasm.noise_loopback(ptr0, len0);
    return ret;
}

/**
 * Open a keystore blob with `password`, returning the device fingerprint (user id). Errors if
 * the password is wrong or the blob is corrupt/tampered (AEAD rejects it).
 * @param {Uint8Array} blob
 * @param {string} password
 * @returns {string}
 */
export function open_identity_keystore(blob, password) {
    let deferred4_0;
    let deferred4_1;
    try {
        const ptr0 = passArray8ToWasm0(blob, wasm.__wbindgen_malloc);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(password, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len1 = WASM_VECTOR_LEN;
        const ret = wasm.open_identity_keystore(ptr0, len0, ptr1, len1);
        var ptr3 = ret[0];
        var len3 = ret[1];
        if (ret[3]) {
            ptr3 = 0; len3 = 0;
            throw takeFromExternrefTable0(ret[2]);
        }
        deferred4_0 = ptr3;
        deferred4_1 = len3;
        return getStringFromWasm0(ptr3, len3);
    } finally {
        wasm.__wbindgen_free(deferred4_0, deferred4_1, 1);
    }
}

/**
 * Run the mesh `SecureChannel` (Noise XX) over a browser `RTCDataChannel` and return the
 * authenticated peer's fingerprint. `initiator` is the offerer side. This is the browser
 * counterpart to the node gateway: in production the PWA runs this over the WebRTC data channel
 * to the node, then syncs the mesh event log through it.
 * @param {RTCDataChannel} dc
 * @param {boolean} initiator
 * @returns {Promise<string>}
 */
export function secure_handshake_over_dc(dc, initiator) {
    const ret = wasm.secure_handshake_over_dc(dc, initiator);
    return ret;
}

/**
 * Run the mesh event-log sync over a data channel and return how many events the local store
 * holds afterwards. The initiator drives `request_round` (and closes the channel when done so
 * the responder's serve loop stops); the responder serves rounds. With `seed`, the store starts
 * with one event. This is the in-wasm proof that the PWA can sync the mesh over WebRTC: seed one
 * side and both converge to the same event.
 * @param {RTCDataChannel} dc
 * @param {boolean} initiator
 * @param {boolean} seed
 * @returns {Promise<number>}
 */
export function sync_demo_over_dc(dc, initiator, seed) {
    const ret = wasm.sync_demo_over_dc(dc, initiator, seed);
    return ret;
}
function __wbg_get_imports() {
    const import0 = {
        __proto__: null,
        __wbg_Error_fdd633d4bb5dd76a: function(arg0, arg1) {
            const ret = Error(getStringFromWasm0(arg0, arg1));
            return ret;
        },
        __wbg___wbindgen_is_function_acc5528be2b923f2: function(arg0) {
            const ret = typeof(arg0) === 'function';
            return ret;
        },
        __wbg___wbindgen_is_object_0beba4a1980d3eea: function(arg0) {
            const val = arg0;
            const ret = typeof(val) === 'object' && val !== null;
            return ret;
        },
        __wbg___wbindgen_is_string_1fca8072260dd261: function(arg0) {
            const ret = typeof(arg0) === 'string';
            return ret;
        },
        __wbg___wbindgen_is_undefined_721f8decd50c87a3: function(arg0) {
            const ret = arg0 === undefined;
            return ret;
        },
        __wbg___wbindgen_throw_ea4887a5f8f9a9db: function(arg0, arg1) {
            throw new Error(getStringFromWasm0(arg0, arg1));
        },
        __wbg__wbg_cb_unref_33c39e13d73b25f6: function(arg0) {
            arg0._wbg_cb_unref();
        },
        __wbg_call_5575218572ead796: function() { return handleError(function (arg0, arg1, arg2) {
            const ret = arg0.call(arg1, arg2);
            return ret;
        }, arguments); },
        __wbg_close_2ba5cc8d6aac7a37: function(arg0) {
            arg0.close();
        },
        __wbg_crypto_38df2bab126b63dc: function(arg0) {
            const ret = arg0.crypto;
            return ret;
        },
        __wbg_data_4a7f1308dbd33a21: function(arg0) {
            const ret = arg0.data;
            return ret;
        },
        __wbg_error_a6fa202b58aa1cd3: function(arg0, arg1) {
            let deferred0_0;
            let deferred0_1;
            try {
                deferred0_0 = arg0;
                deferred0_1 = arg1;
                console.error(getStringFromWasm0(arg0, arg1));
            } finally {
                wasm.__wbindgen_free(deferred0_0, deferred0_1, 1);
            }
        },
        __wbg_getRandomValues_c44a50d8cfdaebeb: function() { return handleError(function (arg0, arg1) {
            arg0.getRandomValues(arg1);
        }, arguments); },
        __wbg_length_589238bdcf171f0e: function(arg0) {
            const ret = arg0.length;
            return ret;
        },
        __wbg_msCrypto_bd5a034af96bcba6: function(arg0) {
            const ret = arg0.msCrypto;
            return ret;
        },
        __wbg_new_227d7c05414eb861: function() {
            const ret = new Error();
            return ret;
        },
        __wbg_new_81880fb5002cb255: function(arg0) {
            const ret = new Uint8Array(arg0);
            return ret;
        },
        __wbg_new_typed_00a409eb4ec4f2d9: function(arg0, arg1) {
            try {
                var state0 = {a: arg0, b: arg1};
                var cb0 = (arg0, arg1) => {
                    const a = state0.a;
                    state0.a = 0;
                    try {
                        return wasm_bindgen__convert__closures_____invoke__h3cd6c576a5e62e5f(a, state0.b, arg0, arg1);
                    } finally {
                        state0.a = a;
                    }
                };
                const ret = new Promise(cb0);
                return ret;
            } finally {
                state0.a = 0;
            }
        },
        __wbg_new_with_length_9b650f44b5c44a4e: function(arg0) {
            const ret = new Uint8Array(arg0 >>> 0);
            return ret;
        },
        __wbg_node_84ea875411254db1: function(arg0) {
            const ret = arg0.node;
            return ret;
        },
        __wbg_now_d2e0afbad4edbe82: function() {
            const ret = Date.now();
            return ret;
        },
        __wbg_process_44c7a14e11e9f69e: function(arg0) {
            const ret = arg0.process;
            return ret;
        },
        __wbg_prototypesetcall_d721637c7ca66eb8: function(arg0, arg1, arg2) {
            Uint8Array.prototype.set.call(getArrayU8FromWasm0(arg0, arg1), arg2);
        },
        __wbg_queueMicrotask_1c9b3800e321a967: function(arg0) {
            const ret = arg0.queueMicrotask;
            return ret;
        },
        __wbg_queueMicrotask_311744e534a929a3: function(arg0) {
            queueMicrotask(arg0);
        },
        __wbg_randomFillSync_6c25eac9869eb53c: function() { return handleError(function (arg0, arg1) {
            arg0.randomFillSync(arg1);
        }, arguments); },
        __wbg_require_b4edbdcf3e2a1ef0: function() { return handleError(function () {
            const ret = module.require;
            return ret;
        }, arguments); },
        __wbg_resolve_d82363d90af6928a: function(arg0) {
            const ret = Promise.resolve(arg0);
            return ret;
        },
        __wbg_send_aa0dff2a7e4bd540: function() { return handleError(function (arg0, arg1, arg2) {
            arg0.send(getArrayU8FromWasm0(arg1, arg2));
        }, arguments); },
        __wbg_set_binaryType_e0b5f80e28f0849e: function(arg0, arg1) {
            arg0.binaryType = __wbindgen_enum_RtcDataChannelType[arg1];
        },
        __wbg_set_onclose_37f22a724b1e26e7: function(arg0, arg1) {
            arg0.onclose = arg1;
        },
        __wbg_set_onmessage_64ff25e6dd70b00f: function(arg0, arg1) {
            arg0.onmessage = arg1;
        },
        __wbg_stack_3b0d974bbf31e44f: function(arg0, arg1) {
            const ret = arg1.stack;
            const ptr1 = passStringToWasm0(ret, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            const len1 = WASM_VECTOR_LEN;
            getDataViewMemory0().setInt32(arg0 + 4 * 1, len1, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, ptr1, true);
        },
        __wbg_static_accessor_GLOBAL_THIS_2fee5048bcca5938: function() {
            const ret = typeof globalThis === 'undefined' ? null : globalThis;
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        },
        __wbg_static_accessor_GLOBAL_ce44e66a4935da8c: function() {
            const ret = typeof global === 'undefined' ? null : global;
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        },
        __wbg_static_accessor_SELF_44f6e0cb5e67cdad: function() {
            const ret = typeof self === 'undefined' ? null : self;
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        },
        __wbg_static_accessor_WINDOW_168f178805d978fe: function() {
            const ret = typeof window === 'undefined' ? null : window;
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        },
        __wbg_subarray_b0e8ac4ed313fea8: function(arg0, arg1, arg2) {
            const ret = arg0.subarray(arg1 >>> 0, arg2 >>> 0);
            return ret;
        },
        __wbg_then_591b6b3a75ee817a: function(arg0, arg1) {
            const ret = arg0.then(arg1);
            return ret;
        },
        __wbg_versions_276b2795b1c6a219: function(arg0) {
            const ret = arg0.versions;
            return ret;
        },
        __wbindgen_cast_0000000000000001: function(arg0, arg1) {
            // Cast intrinsic for `Closure(Closure { owned: true, function: Function { arguments: [Externref], shim_idx: 234, ret: Result(Unit), inner_ret: Some(Result(Unit)) }, mutable: true }) -> Externref`.
            const ret = makeMutClosure(arg0, arg1, wasm_bindgen__convert__closures_____invoke__h1c47d85015abd8d5);
            return ret;
        },
        __wbindgen_cast_0000000000000002: function(arg0, arg1) {
            // Cast intrinsic for `Closure(Closure { owned: true, function: Function { arguments: [NamedExternref("MessageEvent")], shim_idx: 71, ret: Unit, inner_ret: Some(Unit) }, mutable: true }) -> Externref`.
            const ret = makeMutClosure(arg0, arg1, wasm_bindgen__convert__closures_____invoke__ha64b36242e1c9513);
            return ret;
        },
        __wbindgen_cast_0000000000000003: function(arg0, arg1) {
            // Cast intrinsic for `Closure(Closure { owned: true, function: Function { arguments: [], shim_idx: 69, ret: Unit, inner_ret: Some(Unit) }, mutable: true }) -> Externref`.
            const ret = makeMutClosure(arg0, arg1, wasm_bindgen__convert__closures_____invoke__h25b09cbd1f5fd7c4);
            return ret;
        },
        __wbindgen_cast_0000000000000004: function(arg0) {
            // Cast intrinsic for `F64 -> Externref`.
            const ret = arg0;
            return ret;
        },
        __wbindgen_cast_0000000000000005: function(arg0, arg1) {
            // Cast intrinsic for `Ref(Slice(U8)) -> NamedExternref("Uint8Array")`.
            const ret = getArrayU8FromWasm0(arg0, arg1);
            return ret;
        },
        __wbindgen_cast_0000000000000006: function(arg0, arg1) {
            // Cast intrinsic for `Ref(String) -> Externref`.
            const ret = getStringFromWasm0(arg0, arg1);
            return ret;
        },
        __wbindgen_init_externref_table: function() {
            const table = wasm.__wbindgen_externrefs;
            const offset = table.grow(4);
            table.set(0, undefined);
            table.set(offset + 0, undefined);
            table.set(offset + 1, null);
            table.set(offset + 2, true);
            table.set(offset + 3, false);
        },
    };
    return {
        __proto__: null,
        "./mesh_talk_wasm_bg.js": import0,
    };
}

function wasm_bindgen__convert__closures_____invoke__h25b09cbd1f5fd7c4(arg0, arg1) {
    wasm.wasm_bindgen__convert__closures_____invoke__h25b09cbd1f5fd7c4(arg0, arg1);
}

function wasm_bindgen__convert__closures_____invoke__ha64b36242e1c9513(arg0, arg1, arg2) {
    wasm.wasm_bindgen__convert__closures_____invoke__ha64b36242e1c9513(arg0, arg1, arg2);
}

function wasm_bindgen__convert__closures_____invoke__h1c47d85015abd8d5(arg0, arg1, arg2) {
    const ret = wasm.wasm_bindgen__convert__closures_____invoke__h1c47d85015abd8d5(arg0, arg1, arg2);
    if (ret[1]) {
        throw takeFromExternrefTable0(ret[0]);
    }
}

function wasm_bindgen__convert__closures_____invoke__h3cd6c576a5e62e5f(arg0, arg1, arg2, arg3) {
    wasm.wasm_bindgen__convert__closures_____invoke__h3cd6c576a5e62e5f(arg0, arg1, arg2, arg3);
}


const __wbindgen_enum_RtcDataChannelType = ["arraybuffer", "blob"];
const WasmNodeFinalization = (typeof FinalizationRegistry === 'undefined')
    ? { register: () => {}, unregister: () => {} }
    : new FinalizationRegistry(ptr => wasm.__wbg_wasmnode_free(ptr, 1));

function addToExternrefTable0(obj) {
    const idx = wasm.__externref_table_alloc();
    wasm.__wbindgen_externrefs.set(idx, obj);
    return idx;
}

const CLOSURE_DTORS = (typeof FinalizationRegistry === 'undefined')
    ? { register: () => {}, unregister: () => {} }
    : new FinalizationRegistry(state => wasm.__wbindgen_destroy_closure(state.a, state.b));

function getArrayU8FromWasm0(ptr, len) {
    ptr = ptr >>> 0;
    return getUint8ArrayMemory0().subarray(ptr / 1, ptr / 1 + len);
}

let cachedDataViewMemory0 = null;
function getDataViewMemory0() {
    if (cachedDataViewMemory0 === null || cachedDataViewMemory0.buffer.detached === true || (cachedDataViewMemory0.buffer.detached === undefined && cachedDataViewMemory0.buffer !== wasm.memory.buffer)) {
        cachedDataViewMemory0 = new DataView(wasm.memory.buffer);
    }
    return cachedDataViewMemory0;
}

function getStringFromWasm0(ptr, len) {
    return decodeText(ptr >>> 0, len);
}

let cachedUint8ArrayMemory0 = null;
function getUint8ArrayMemory0() {
    if (cachedUint8ArrayMemory0 === null || cachedUint8ArrayMemory0.byteLength === 0) {
        cachedUint8ArrayMemory0 = new Uint8Array(wasm.memory.buffer);
    }
    return cachedUint8ArrayMemory0;
}

function handleError(f, args) {
    try {
        return f.apply(this, args);
    } catch (e) {
        const idx = addToExternrefTable0(e);
        wasm.__wbindgen_exn_store(idx);
    }
}

function isLikeNone(x) {
    return x === undefined || x === null;
}

function makeMutClosure(arg0, arg1, f) {
    const state = { a: arg0, b: arg1, cnt: 1 };
    const real = (...args) => {

        // First up with a closure we increment the internal reference
        // count. This ensures that the Rust closure environment won't
        // be deallocated while we're invoking it.
        state.cnt++;
        const a = state.a;
        state.a = 0;
        try {
            return f(a, state.b, ...args);
        } finally {
            state.a = a;
            real._wbg_cb_unref();
        }
    };
    real._wbg_cb_unref = () => {
        if (--state.cnt === 0) {
            wasm.__wbindgen_destroy_closure(state.a, state.b);
            state.a = 0;
            CLOSURE_DTORS.unregister(state);
        }
    };
    CLOSURE_DTORS.register(real, state, state);
    return real;
}

function passArray8ToWasm0(arg, malloc) {
    const ptr = malloc(arg.length * 1, 1) >>> 0;
    getUint8ArrayMemory0().set(arg, ptr / 1);
    WASM_VECTOR_LEN = arg.length;
    return ptr;
}

function passStringToWasm0(arg, malloc, realloc) {
    if (realloc === undefined) {
        const buf = cachedTextEncoder.encode(arg);
        const ptr = malloc(buf.length, 1) >>> 0;
        getUint8ArrayMemory0().subarray(ptr, ptr + buf.length).set(buf);
        WASM_VECTOR_LEN = buf.length;
        return ptr;
    }

    let len = arg.length;
    let ptr = malloc(len, 1) >>> 0;

    const mem = getUint8ArrayMemory0();

    let offset = 0;

    for (; offset < len; offset++) {
        const code = arg.charCodeAt(offset);
        if (code > 0x7F) break;
        mem[ptr + offset] = code;
    }
    if (offset !== len) {
        if (offset !== 0) {
            arg = arg.slice(offset);
        }
        ptr = realloc(ptr, len, len = offset + arg.length * 3, 1) >>> 0;
        const view = getUint8ArrayMemory0().subarray(ptr + offset, ptr + len);
        const ret = cachedTextEncoder.encodeInto(arg, view);

        offset += ret.written;
        ptr = realloc(ptr, len, offset, 1) >>> 0;
    }

    WASM_VECTOR_LEN = offset;
    return ptr;
}

function takeFromExternrefTable0(idx) {
    const value = wasm.__wbindgen_externrefs.get(idx);
    wasm.__externref_table_dealloc(idx);
    return value;
}

let cachedTextDecoder = new TextDecoder('utf-8', { ignoreBOM: true, fatal: true });
cachedTextDecoder.decode();
const MAX_SAFARI_DECODE_BYTES = 2146435072;
let numBytesDecoded = 0;
function decodeText(ptr, len) {
    numBytesDecoded += len;
    if (numBytesDecoded >= MAX_SAFARI_DECODE_BYTES) {
        cachedTextDecoder = new TextDecoder('utf-8', { ignoreBOM: true, fatal: true });
        cachedTextDecoder.decode();
        numBytesDecoded = len;
    }
    return cachedTextDecoder.decode(getUint8ArrayMemory0().subarray(ptr, ptr + len));
}

const cachedTextEncoder = new TextEncoder();

if (!('encodeInto' in cachedTextEncoder)) {
    cachedTextEncoder.encodeInto = function (arg, view) {
        const buf = cachedTextEncoder.encode(arg);
        view.set(buf);
        return {
            read: arg.length,
            written: buf.length
        };
    };
}

let WASM_VECTOR_LEN = 0;

let wasmModule, wasmInstance, wasm;
function __wbg_finalize_init(instance, module) {
    wasmInstance = instance;
    wasm = instance.exports;
    wasmModule = module;
    cachedDataViewMemory0 = null;
    cachedUint8ArrayMemory0 = null;
    wasm.__wbindgen_start();
    return wasm;
}

async function __wbg_load(module, imports) {
    if (typeof Response === 'function' && module instanceof Response) {
        if (typeof WebAssembly.instantiateStreaming === 'function') {
            try {
                return await WebAssembly.instantiateStreaming(module, imports);
            } catch (e) {
                const validResponse = module.ok && expectedResponseType(module.type);

                if (validResponse && module.headers.get('Content-Type') !== 'application/wasm') {
                    console.warn("`WebAssembly.instantiateStreaming` failed because your server does not serve Wasm with `application/wasm` MIME type. Falling back to `WebAssembly.instantiate` which is slower. Original error:\n", e);

                } else { throw e; }
            }
        }

        const bytes = await module.arrayBuffer();
        return await WebAssembly.instantiate(bytes, imports);
    } else {
        const instance = await WebAssembly.instantiate(module, imports);

        if (instance instanceof WebAssembly.Instance) {
            return { instance, module };
        } else {
            return instance;
        }
    }

    function expectedResponseType(type) {
        switch (type) {
            case 'basic': case 'cors': case 'default': return true;
        }
        return false;
    }
}

function initSync(module) {
    if (wasm !== undefined) return wasm;


    if (module !== undefined) {
        if (Object.getPrototypeOf(module) === Object.prototype) {
            ({module} = module)
        } else {
            console.warn('using deprecated parameters for `initSync()`; pass a single object instead')
        }
    }

    const imports = __wbg_get_imports();
    if (!(module instanceof WebAssembly.Module)) {
        module = new WebAssembly.Module(module);
    }
    const instance = new WebAssembly.Instance(module, imports);
    return __wbg_finalize_init(instance, module);
}

async function __wbg_init(module_or_path) {
    if (wasm !== undefined) return wasm;


    if (module_or_path !== undefined) {
        if (Object.getPrototypeOf(module_or_path) === Object.prototype) {
            ({module_or_path} = module_or_path)
        } else {
            console.warn('using deprecated parameters for the initialization function; pass a single object instead')
        }
    }

    if (module_or_path === undefined) {
        module_or_path = new URL('mesh_talk_wasm_bg.wasm', import.meta.url);
    }
    const imports = __wbg_get_imports();

    if (typeof module_or_path === 'string' || (typeof Request === 'function' && module_or_path instanceof Request) || (typeof URL === 'function' && module_or_path instanceof URL)) {
        module_or_path = fetch(module_or_path);
    }

    const { instance, module } = await __wbg_load(await module_or_path, imports);

    return __wbg_finalize_init(instance, module);
}

export { initSync, __wbg_init as default };
