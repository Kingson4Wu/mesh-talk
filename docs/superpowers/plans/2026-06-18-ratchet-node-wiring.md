# Ratchet Node DM-Path Wiring Implementation Plan (Ratchet Plan 4)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development. SECURITY-SENSITIVE. Steps use checkbox syntax.

**Goal:** Route the redesign DM `Message` path through the ratchet end-to-end: seal with `DmRatchet`, decrypt-on-receive into `ReceivedLog` (then the wire key is single-use/gone), and serve DM history from `SentLog` + `ReceivedLog`. Forward secrecy, proven by a loopback rig.

**Architecture:** `Node` gains `dm_ratchet: Mutex<DmRatchet>` + `received: Mutex<ReceivedLog>`, opened from paths DERIVED from `log_path`'s dir (so `Node::open`'s signature is unchanged — no caller migration). `send_dm_reply` seals the `MessageBody` via `DmRatchet::encrypt`; `emit_new_messages` decrypts via `DmRatchet::decrypt`, stores plaintext in `ReceivedLog`, and only marks emitted on success (so a not-yet-decryptable message is retried); `history` merges `SentLog` + `ReceivedLog`. The redesign is UNRELEASED → NO legacy `dm::seal` coexistence for messages. File-manifest + reaction events keep `dm::seal` (not the message path).

**Tech Stack:** Rust; `crate::node::{dm_ratchet::DmRatchet, ratchet_sessions::RatchetSessions, received_log::ReceivedLog}`, the existing Node DM path. No new deps.

---

## Background the implementer needs

Current Node DM path (`src-tauri/src/node/node.rs`):
- `Node::open(identity, roster, incoming, channel_incoming, file_incoming, log_path, sent_path, password)` — opens `PersistentEventLog`, `SentLog`, seeds `emitted` from `log.all_event_ids()`, seeds channels + files.
- `send_dm_reply(recipient, text, reply_to)`: wraps `MessageBody::new(text, reply_to).encode()` → `build_dm_event(identity, &peer.public, conv, seq, parents, lamport, wall_clock, &wrapped)` (which `dm::seal`s) → append; `sentlog.record(conv, seq, wall_clock, wrapped)`; deliver+replicate; `emit_new_messages`.
- `emit_new_messages(conv)`: collect non-self un-emitted `Message` events, mark emitted, then `open_dm_event(identity, roster, event)` → `(from, name, text, reply_to)` → send `ReceivedDm`.
- `history(conv, limit)`: received via `open_dm_event` (event.id) + sent via `sentlog` (seq→id map for ids); both decode `MessageBody` for reply_to.
- `open_dm_event` (`node/conversation.rs`): `dm::open` + `MessageBody::decode`.

Verified service API (Plan 3):
- `DmRatchet::new(RatchetSessions)`, `encrypt(&mut self, me: &DeviceIdentity, peer: &PublicIdentity, pt: &[u8]) -> Result<Vec<u8>, LogError>` (wire), `decrypt(&mut self, me, peer, wire) -> Result<Vec<u8>, LogError>` (plaintext = the wrapped MessageBody).
- `RatchetSessions::open(path, password)`, `ReceivedLog::open(path, password)`, `received.record(conversation, from, wall_clock, plaintext)`, `received.entries(&conv) -> Vec<ReceivedEntry>`.

**CPU/test discipline:** `nice -n 10`, one filter/run. `cargo test --lib node::node` + `node::conversation`; build/clippy/fmt; `cargo test --test persistent_history`. Confirm branch line.

---

### Task 1: add `event_id` to received entries + thread the stores into `Node` (no behavior switch)

**Files:** Modify `src-tauri/src/node/received_log.rs`, `node/node.rs`.

- [ ] **Step 1: `ReceivedEntry` gains `event_id`**

Add `pub event_id: crate::eventlog::event::EventId,` to `ReceivedEntry` (first field). Update `record` to take `event_id: EventId` and store it. Update the received_log tests' `record(...)` calls to pass an `EventId::new([..;32])`.

- [ ] **Step 2: `Node` fields + open the stores (derived paths, NO signature change)**

Add imports: `use crate::node::dm_ratchet::DmRatchet; use crate::node::ratchet_sessions::RatchetSessions; use crate::node::received_log::ReceivedLog;`.
Add fields:
```rust
    dm_ratchet: Mutex<DmRatchet>,
    received: Mutex<ReceivedLog>,
```
In `Node::open`, after opening `sentlog`, derive the new stores from `log_path`'s directory (so the signature is unchanged):
```rust
        let dir = log_path.parent().unwrap_or_else(|| std::path::Path::new("."));
        let sessions = RatchetSessions::open(&dir.join("ratchet.sessions"), password)?;
        let dm_ratchet = DmRatchet::new(sessions);
        let received = ReceivedLog::open(&dir.join("received.log"), password)?;
```
Add to the struct literal: `dm_ratchet: Mutex::new(dm_ratchet), received: Mutex::new(received),`.

- [ ] **Step 3: build, test, commit (plumbing only — no behavior change yet)**
```bash
cd src-tauri && nice -n 10 cargo test --lib node::received_log node::node -- --test-threads=2
cd src-tauri && nice -n 10 cargo build --lib --bins && nice -n 10 cargo clippy --lib --bins -- -D warnings 2>&1 | tail -20
cd src-tauri && nice -n 10 cargo fmt
cd /Users/kingsonwu/programming/rust-src/mesh-talk
git add src-tauri/src/node/received_log.rs src-tauri/src/node/node.rs
git commit -m "feat(node): thread DmRatchet + ReceivedLog into Node (derived paths); event_id on received entries"
git status | head -1
```
(All existing tests still pass — nothing uses the new stores yet.)

---

### Task 2: switch the DM message path to the ratchet + serve history from stores

**Files:** Modify `src-tauri/src/node/node.rs`. (Maybe remove now-unused `open_dm_event` usage; keep the fn if other code uses it, else `#[allow(dead_code)]` or delete.)

- [ ] **Step 1: `send_dm_reply` seals via the ratchet**

Replace the seal+append in `send_dm_reply`: compute the wire OUTSIDE the log lock, then append a `Message` event whose ciphertext is the wire.
```rust
        let wrapped = MessageBody::new(text.to_vec(), reply_to).encode();
        let wire = {
            let mut r = self.dm_ratchet.lock().expect("dm_ratchet mutex not poisoned");
            r.encrypt(&self.identity, &peer.public, &wrapped).map_err(NodeError::Log)?
        };
        let wall_clock = now_millis();
        let seq;
        {
            let mut log = self.log.lock().expect("log mutex not poisoned");
            let (parents, lamport) = log.prepare(&conv);
            seq = log.version_vector(&conv).get(&self_author).copied().unwrap_or(0) + 1;
            let event = Event::new(&self.identity, conv, seq, parents, lamport, wall_clock, EventKind::Message, wire);
            log.append(event).map_err(NodeError::Log)?;
        }
        // sidecar stores the WRAPPED plaintext (for our own history + reply_to).
        let _ = self.sentlog.lock().expect("sentlog mutex not poisoned").record(conv, seq, wall_clock, &wrapped);
```
(Keep the existing `conv`/`self_author`/`peer` setup + the deliver/replicate/emit tail. `build_dm_event` is no longer used by this path — leave the fn for now; clippy may flag it unused → add `#[allow(dead_code)]` on `build_dm_event` or remove it if nothing else references it. Note: `open_dm_event` is also being retired below.)

- [ ] **Step 2: `emit_new_messages` decrypts via the ratchet, stores plaintext, marks emitted ON SUCCESS**

Rewrite `emit_new_messages` so a message is decrypted via the ratchet, recorded in `ReceivedLog`, and only marked emitted when it actually decrypts (so a transiently-undecryptable one is retried):
```rust
    fn emit_new_messages(&self, conv: ConversationId) {
        let self_author = Author::from_ed25519(self.identity.public().ed25519_pub);
        let candidates: Vec<Event> = {
            let log = self.log.lock().expect("log mutex not poisoned");
            let emitted = self.emitted.lock().expect("emitted mutex not poisoned");
            log.events(&conv)
                .into_iter()
                .filter(|e| e.kind == EventKind::Message && e.author != self_author && !emitted.contains(&e.id))
                .cloned()
                .collect()
        };
        for event in candidates {
            // Resolve the author's public identity from the roster.
            let author_uid = event.author.user_id();
            let peer_public = {
                let roster = self.roster.lock().expect("roster mutex not poisoned");
                match roster.get(&author_uid) {
                    Some(p) => p.public.clone(),
                    None => continue, // unknown author yet; retry on a later sync (NOT marked emitted)
                }
            };
            let wrapped = {
                let mut r = self.dm_ratchet.lock().expect("dm_ratchet mutex not poisoned");
                match r.decrypt(&self.identity, &peer_public, &event.ciphertext) {
                    Ok(pt) => pt,
                    Err(_) => continue, // not yet decryptable / not a ratchet DM; leave un-emitted
                }
            };
            let body = MessageBody::decode(&wrapped);
            // Persist the received plaintext (the wire key is single-use/gone) + mark emitted.
            let _ = self.received.lock().expect("received mutex not poisoned").record(
                conv, author_uid.clone(), event.wall_clock, &wrapped, event.id, // adjust arg order to ReceivedLog::record
            );
            self.emitted.lock().expect("emitted mutex not poisoned").insert(event.id);
            let from_name = peer_public_display_name(&self.roster, &author_uid).unwrap_or(author_uid.clone());
            let _ = self.incoming.send(ReceivedDm {
                from: author_uid,
                from_name,
                text: body.text,
                reply_to: body.reply_to,
            });
        }
    }
```
Adjust to the real `ReceivedLog::record` signature `(conversation, from, wall_clock, plaintext, event_id)` from Task 1 (order them correctly). For `from_name`: read the roster peer's `name` (you already hold/relock the roster — inline it: capture `name` alongside `peer_public` in the roster block, return `(PublicIdentity, String)`). Drop the `peer_public_display_name` helper sketch — just capture the name in the roster lookup block.

- [ ] **Step 3: `history` + `dm_history` serve from `SentLog` + `ReceivedLog`**

Rewrite `history(conv, limit)` to merge sent (from `SentLog`, decode `MessageBody` for text+reply_to, id via seq→id map of own authored Message events) + received (from `ReceivedLog`: `event_id`, decode `MessageBody`):
```rust
    pub fn history(&self, conversation: ConversationId, limit: usize) -> Vec<HistoryEntry> {
        let self_author = Author::from_ed25519(self.identity.public().ed25519_pub);
        let mut my_msg_ids: std::collections::HashMap<u64, EventId> = std::collections::HashMap::new();
        {
            let log = self.log.lock().expect("log mutex not poisoned");
            for e in log.events(&conversation) {
                if e.kind == EventKind::Message && e.author == self_author {
                    my_msg_ids.insert(e.seq, e.id);
                }
            }
        }
        let mut entries: Vec<HistoryEntry> = Vec::new();
        for sent in self.sentlog.lock().expect("sentlog mutex").entries(&conversation) {
            let body = MessageBody::decode(&sent.plaintext);
            entries.push(HistoryEntry {
                id: my_msg_ids.get(&sent.seq).copied().unwrap_or(EventId::new([0u8; 32])),
                from_me: true, who: "you".to_string(), text: body.text,
                wall_clock: sent.wall_clock, reply_to: body.reply_to,
            });
        }
        for rcv in self.received.lock().expect("received mutex").entries(&conversation) {
            let body = MessageBody::decode(&rcv.plaintext);
            entries.push(HistoryEntry {
                id: rcv.event_id, from_me: false, who: rcv.from, text: body.text,
                wall_clock: rcv.wall_clock, reply_to: body.reply_to,
            });
        }
        entries.sort_by_key(|e| e.wall_clock);
        if entries.len() > limit { entries.drain(0..entries.len() - limit); }
        entries
    }
```
(`dm_history` already wraps `history` via the derived conv id — unchanged. `open_dm_event` is now unused by the node; if nothing references it, delete it + its tests from `conversation.rs`, or keep with `#[allow(dead_code)]`. Prefer deleting to avoid dead code — but check `grep -rn open_dm_event`.)

- [ ] **Step 4: forward-secrecy loopback rig**

Add to `node/node.rs` tests: two nodes over loopback TCP; Alice sends 2 DMs, Bob receives both (via the ratchet); assert (a) Bob's `dm_history` shows them (from `ReceivedLog`), (b) the SAME wire event re-fed to `emit_new_messages`/decrypt a second time does NOT re-surface (key consumed — proves single-use), (c) out-of-order: Alice sends m0,m1,m2; Bob's drain/serve receives them and all appear in history regardless of arrival order. Reuse the existing DM loopback rig scaffolding (`seed_roster`, the mpsc sinks, `run_accept_loop`).

- [ ] **Step 5: fix DM-test ripples + run the regression net**

The DM message path is now ratchet-based. Run ALL of these; fix any literal/asserts that assumed the dm-box wire (the offline-PO rig, persistent_history, the reactions rig (uses `react_dm`→`dm::seal`, NOT the message path → should be unaffected), the file rig (manifests use `dm::seal` → unaffected), search test, threads reply rig):
```bash
cd src-tauri && nice -n 10 cargo test --lib node::node -- --test-threads=2
cd src-tauri && nice -n 10 cargo test --lib node::conversation -- --test-threads=2
cd src-tauri && nice -n 10 cargo test --test persistent_history -- --test-threads=2
cd src-tauri && nice -n 10 cargo build --lib --bins && nice -n 10 cargo clippy --lib --bins -- -D warnings 2>&1 | tail -20
cd src-tauri && nice -n 10 cargo fmt
```
All DM rigs must pass via the ratchet. If `persistent_history` asserted re-decryptable wire history, update it to expect history from the stores (received plaintext persists; the wire key does not). Commit:
```bash
cd /Users/kingsonwu/programming/rust-src/mesh-talk
git add -A
git commit -m "feat(node): DM messages use the Double Ratchet end-to-end; history from sent+received stores"
git status | head -1
```

---

## Notes for the reviewer (crypto-aware)

- **Delivered:** redesign DM messages now have forward secrecy — each is ratchet-sealed, decrypted once on receipt into `ReceivedLog`, and the wire key is single-use. History serves from `SentLog` + `ReceivedLog` (the wire is no longer locally decryptable — that IS the forward-secrecy property). Reactions + file manifests still use `dm::seal` (separate, non-message events).
- **Reviewer checks:** `emit_new_messages` marks emitted ONLY after a successful decrypt (no lost messages; idempotent because `DmRatchet::decrypt` loads-per-call and persists only on success); no `MutexGuard` across an `.await` (the ratchet/log/received/sentlog locks are sync, released before deliver/replicate awaits); the FS rig proves a consumed key can't re-open; out-of-order delivery still surfaces all messages; `persistent_history` reflects store-served history; nothing on the message path still calls `dm::seal`/`open_dm_event`.
- **Deferred:** group/channel ratchet (sender keys); multi-device; a periodic prune of `ReceivedLog`/old sessions; pre-key-based async session setup (current bootstrap is deterministic from identity keys, which is sound but means the very first message's FS depends on identity keys until the first ratchet — standard).
