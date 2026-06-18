# Channels in the App + UI Implementation Plan (Plan 4b)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Surface live channels in the desktop app: forward received channel messages to a Tauri event, add IPC commands to create/list/send/read channels, and add a channels section to the `/redesign` Vue route.

**Architecture:** `RedesignRuntime::start` gains an `on_channel` callback (mirroring `on_dm`) wired to the node's `channel_incoming` stream; `commands::login` passes a closure that emits a `redesign-channel-message` Tauri event. New IPC commands in `redesign_commands.rs` delegate to the node's `create_channel`/`send_channel_message`/`channel_history` (+ a new `Node::list_channels`), converting between the binary `ConversationId` and a hex string for the frontend. The Vue view gains a channels list + create + per-channel message pane.

**Tech Stack:** Rust + Tauri v2 (`Emitter`); Vue 3 `<script setup>` + Pinia + `@tauri-apps/api` (`invoke`, `listen`); `hex 0.4` (already a dependency).

---

## Background the implementer needs

This is **Plan 4b**, the final channels slice. Plan 4a (live node channels, commits `6dfe1e1`/`20a7148`/`37e5507`/`9255252`) shipped on the `Node`:
- `Node::create_channel(&self, name: &str, members: Vec<PublicIdentity>) -> Result<ConversationId, NodeError>`
- `Node::send_channel_message(&self, channel: ConversationId, text: &[u8]) -> Result<(), NodeError>`
- `Node::channel_history(&self, channel: ConversationId, limit: usize) -> Vec<HistoryEntry>` (decrypts own + others via the group key)
- received channel messages stream on `channel_incoming: mpsc::UnboundedSender<ReceivedChannelMessage>` (constructor arg already wired; `RedesignRuntime` currently drops the receiver as `_channel_rx`)
- `ReceivedChannelMessage { channel_id: ConversationId, channel_name: String, from: String, text: Vec<u8> }`
- `ChannelBook::channel_ids() -> Vec<ConversationId>`; `ChannelBook::state(&id) -> Option<&ChannelState>`; `ChannelState::{name()->&str, members()->&[PublicIdentity], epoch()->u64}`

`ConversationId` is a `[u8;32]` newtype: `ConversationId::new([u8;32])`, `as_bytes() -> &[u8;32]`. Hex round-trip: `hex::encode(id.as_bytes())` / `hex::decode(s)` → `<[u8;32]>::try_from(v)`.

**Existing patterns to mirror (read them first):**
- `src-tauri/src/events.rs`: `EVENT_REDESIGN_DM_RECEIVED`, `RedesignDmReceivedEvent { from, from_name, text }`, and `emit_redesign_dm_received<R: Runtime>(app: &AppHandle<R>, ...)` — copy this shape for channels.
- `src-tauri/src/redesign_commands.rs`: `RedesignState`, `PeerInfo`, `HistoryItem`, the `NOT_STARTED` guard, and the snapshot-handle-then-release-lock pattern in `redesign_send_dm`.
- `src-tauri/src/node/runtime.rs`: `start(...)` builds `(incoming_tx, incoming_rx)` + an `on_dm` forwarder task; `handle() -> Arc<Node>`; `peer_public(uid) -> Option<PublicIdentity>`.
- `src-tauri/src/commands.rs` `login`: spawns `RedesignRuntime::start(...)` with an `on_dm` closure that calls `emit_redesign_dm_received`, stores the runtime in `RedesignState`.
- `src-tauri/src/lib.rs`: `.manage(RedesignState::empty())` + `tauri::generate_handler![... redesign_my_id, redesign_list_peers, redesign_send_dm, redesign_history]`.
- `frontend/src/services/api.js`: `redesignAPI { myId, listPeers, sendDm, history }`.
- `frontend/src/views/redesign/RedesignChatView.vue`: the existing DM chat view (peer list poll, history load, optimistic send, `listen('redesign-dm-received')`).

**CPU/test discipline (MANDATORY):** never a bare `cargo build`/`test`. Use:
```bash
cd src-tauri && nice -n 10 cargo build --lib --bins
cd src-tauri && nice -n 10 cargo test --lib node::node node::runtime -- --test-threads=2   # one filter per run
cd src-tauri && nice -n 10 cargo clippy --lib --bins -- -D warnings 2>&1 | tail -20
cd src-tauri && nice -n 10 cargo fmt
cd frontend && npm run build   # or the project's lint/typecheck
```
Confirm `git status | head -1` == `On branch feat/redesign-phase0` after each commit.

---

### Task 1: `Node::list_channels` + runtime `on_channel` forwarder + runtime channel accessors

**Files:** Modify `src-tauri/src/node/node.rs`, `src-tauri/src/node/runtime.rs`.

- [ ] **Step 1: `Node::list_channels` (node.rs)**

Add a small public DTO + method. Put the struct near `HistoryEntry`:
```rust
/// A channel summary for listing in the UI.
#[derive(Debug, Clone)]
pub struct ChannelSummary {
    pub id: ConversationId,
    pub name: String,
    pub member_count: usize,
}
```
And on `impl Node`:
```rust
    /// Summaries of all channels this node is a member of.
    pub fn list_channels(&self) -> Vec<ChannelSummary> {
        let book = self.channels.lock().expect("channels mutex not poisoned");
        book.channel_ids()
            .into_iter()
            .filter_map(|id| {
                book.state(&id).map(|s| ChannelSummary {
                    id,
                    name: s.name().to_string(),
                    member_count: s.members().len(),
                })
            })
            .collect()
    }
```
Export `ChannelSummary` wherever `HistoryEntry` is re-exported (check `node/mod.rs` — add it to the same `pub use`).

- [ ] **Step 2: A unit test for `list_channels` (node.rs `mod tests`)**

Append (reuses the loopback rig's setup style — a single node that creates a channel with itself only needs no peer):
```rust
    #[tokio::test]
    async fn list_channels_reports_created_channels() {
        let me = DeviceIdentity::generate();
        let dir = tempfile::tempdir().unwrap();
        let roster = Arc::new(Mutex::new(Roster::default()));
        let (dm_tx, _dm_rx) = mpsc::unbounded_channel();
        let (ch_tx, _ch_rx) = mpsc::unbounded_channel();
        let node = Node::open(me, roster, dm_tx, ch_tx,
            &dir.path().join("m.log"), &dir.path().join("m-sent.log"), "pw").unwrap();

        assert!(node.list_channels().is_empty());
        let id = node.create_channel("general", vec![]).await.unwrap();
        let channels = node.list_channels();
        assert_eq!(channels.len(), 1);
        assert_eq!(channels[0].id, id);
        assert_eq!(channels[0].name, "general");
        assert_eq!(channels[0].member_count, 1); // just the creator
    }
```
(With no peers and no PO, `create_channel`'s `distribute_channel` is a no-op and returns; the channel state is still built locally via `process_channel`.)

- [ ] **Step 3: runtime `on_channel` forwarder (runtime.rs)**

`start` gains an `on_channel` param after `on_dm`:
```rust
        on_dm: impl Fn(ReceivedDm) + Send + 'static,
        on_channel: impl Fn(crate::node::channel::ReceivedChannelMessage) + Send + 'static,
```
Replace the dropped receiver with a real channel + forwarder. Change:
```rust
        let (channel_tx, _channel_rx) =
            mpsc::unbounded_channel::<crate::node::channel::ReceivedChannelMessage>();
```
to:
```rust
        let (channel_tx, mut channel_rx) =
            mpsc::unbounded_channel::<crate::node::channel::ReceivedChannelMessage>();
```
and after the `on_dm` forwarder task, add:
```rust
        tasks.push(tokio::spawn(async move {
            while let Some(msg) = channel_rx.recv().await {
                on_channel(msg);
            }
        }));
```
Update the two `RedesignRuntime::start(...)` calls in `runtime.rs`'s own `#[cfg(test)] mod tests` to pass an extra `|_ch| {}` closure after the `|_dm| {}` one.

- [ ] **Step 4: runtime channel accessors (runtime.rs)**

Add pass-throughs (the commands could use `handle()` directly, but explicit methods keep the runtime the single node-facing surface):
```rust
    /// Create a channel; returns its conversation id.
    pub async fn create_channel(
        &self,
        name: &str,
        members: Vec<PublicIdentity>,
    ) -> Result<crate::eventlog::event::ConversationId, NodeError> {
        self.node.create_channel(name, members).await
    }

    /// Channels this node is a member of.
    pub fn list_channels(&self) -> Vec<crate::node::ChannelSummary> {
        self.node.list_channels()
    }

    /// History of a channel (all senders).
    pub fn channel_history(
        &self,
        channel: crate::eventlog::event::ConversationId,
        limit: usize,
    ) -> Vec<HistoryEntry> {
        self.node.channel_history(channel, limit)
    }
```
(`send_channel_message` is reached via `handle()` in the command, mirroring `redesign_send_dm`, so the state lock is released before the await. No runtime wrapper needed for it — but if you add one for symmetry, the command must still snapshot `handle()` to avoid holding the `RedesignState` lock across the send.)

- [ ] **Step 5: build, test, clippy, fmt, commit**
```bash
cd src-tauri && nice -n 10 cargo test --lib node::node -- --test-threads=2
cd src-tauri && nice -n 10 cargo test --lib node::runtime -- --test-threads=2
cd src-tauri && nice -n 10 cargo build --lib --bins && nice -n 10 cargo clippy --lib --bins -- -D warnings 2>&1 | tail -20
cd src-tauri && nice -n 10 cargo fmt
cd /Users/kingsonwu/programming/rust-src/mesh-talk
git add src-tauri/src/node/node.rs src-tauri/src/node/runtime.rs src-tauri/src/node/mod.rs
git commit -m "feat(runtime): Node::list_channels + on_channel forwarder + channel accessors"
git status | head -1
```
NOTE: this task changes `RedesignRuntime::start`'s signature, so the app won't build until Task 2 updates `commands.rs`. Build the LIB + the runtime test here; the full app (`--bins` may pull `lib.rs`/`commands.rs`) builds green only after Task 2. If `cargo build --lib --bins` fails solely due to `commands.rs::login` missing the `on_channel` arg, that's expected — note it and proceed; Task 2 fixes it. (Prefer `cargo build --lib` + `cargo test --lib node::runtime` to validate this task in isolation.)

---

### Task 2: IPC commands + the `redesign-channel-message` event + login wiring

**Files:** Modify `src-tauri/src/events.rs`, `src-tauri/src/redesign_commands.rs`, `src-tauri/src/commands.rs`, `src-tauri/src/lib.rs`.

- [ ] **Step 1: the event (events.rs)**

Mirror `EVENT_REDESIGN_DM_RECEIVED`. Add the const next to it:
```rust
pub const EVENT_REDESIGN_CHANNEL_MESSAGE: &str = "redesign-channel-message";
```
Add the payload struct next to `RedesignDmReceivedEvent`:
```rust
#[derive(serde::Serialize, Clone)]
pub struct RedesignChannelMessageEvent {
    pub channel_id: String, // hex
    pub channel_name: String,
    pub from: String,
    pub text: String,
}
```
Add an emit helper mirroring `emit_redesign_dm_received` (match its exact `<R: Runtime>` signature + `app.emit(...)` call):
```rust
pub fn emit_redesign_channel_message<R: Runtime>(
    app: &tauri::AppHandle<R>,
    channel_id: String,
    channel_name: String,
    from: String,
    text: String,
) {
    let _ = app.emit(
        EVENT_REDESIGN_CHANNEL_MESSAGE,
        RedesignChannelMessageEvent { channel_id, channel_name, from, text },
    );
}
```

- [ ] **Step 2: the IPC commands (redesign_commands.rs)**

Add a `ChannelInfo` DTO + four commands. Add `use crate::eventlog::event::ConversationId;` at the top. Helper for hex→id:
```rust
/// A channel as shown in the redesign UI.
#[derive(Serialize)]
pub struct ChannelInfo {
    pub channel_id: String, // hex
    pub name: String,
    pub member_count: usize,
}

fn parse_channel_id(hex_id: &str) -> Result<ConversationId, String> {
    let bytes = hex::decode(hex_id).map_err(|_| "invalid channel id".to_string())?;
    let arr: [u8; 32] = bytes
        .try_into()
        .map_err(|_| "channel id must be 32 bytes".to_string())?;
    Ok(ConversationId::new(arr))
}

#[tauri::command]
pub async fn redesign_list_channels(
    state: tauri::State<'_, RedesignState>,
) -> Result<Vec<ChannelInfo>, String> {
    let guard = state.0.lock().await;
    let rt = guard.as_ref().ok_or_else(|| NOT_STARTED.to_string())?;
    Ok(rt
        .list_channels()
        .into_iter()
        .map(|c| ChannelInfo {
            channel_id: hex::encode(c.id.as_bytes()),
            name: c.name,
            member_count: c.member_count,
        })
        .collect())
}

#[tauri::command]
pub async fn redesign_create_channel(
    state: tauri::State<'_, RedesignState>,
    name: String,
    member_ids: Vec<String>,
) -> Result<String, String> {
    let (node, members) = {
        let guard = state.0.lock().await;
        let rt = guard.as_ref().ok_or_else(|| NOT_STARTED.to_string())?;
        // Resolve each member user_id to its public identity via the roster.
        let mut members = Vec::new();
        for uid in &member_ids {
            let p = rt
                .peer_public(uid)
                .ok_or_else(|| format!("unknown peer: {uid}"))?;
            members.push(p);
        }
        (rt.handle(), members)
    };
    let id = node
        .create_channel(&name, members)
        .await
        .map_err(|e| e.to_string())?;
    Ok(hex::encode(id.as_bytes()))
}

#[tauri::command]
pub async fn redesign_send_channel_message(
    state: tauri::State<'_, RedesignState>,
    channel_id: String,
    text: String,
) -> Result<(), String> {
    let id = parse_channel_id(&channel_id)?;
    let node = {
        let guard = state.0.lock().await;
        let rt = guard.as_ref().ok_or_else(|| NOT_STARTED.to_string())?;
        rt.handle()
    };
    node.send_channel_message(id, text.as_bytes())
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn redesign_channel_history(
    state: tauri::State<'_, RedesignState>,
    channel_id: String,
    limit: usize,
) -> Result<Vec<HistoryItem>, String> {
    let limit = limit.min(500);
    let id = parse_channel_id(&channel_id)?;
    let guard = state.0.lock().await;
    let rt = guard.as_ref().ok_or_else(|| NOT_STARTED.to_string())?;
    Ok(rt
        .channel_history(id, limit)
        .into_iter()
        .map(|h| HistoryItem {
            from_me: h.from_me,
            who: h.who,
            text: String::from_utf8_lossy(&h.text).into_owned(),
            wall_clock: h.wall_clock,
        })
        .collect())
}
```
(`hex` is already a dependency. `create_channel` snapshots `handle()` + resolves members under the guard, then releases the lock before the await — same discipline as `redesign_send_dm`.)

- [ ] **Step 3: register handlers (lib.rs)**

Add the four commands to the `tauri::generate_handler![...]` macro alongside the existing `redesign_*` ones:
```rust
            redesign_list_channels,
            redesign_create_channel,
            redesign_send_channel_message,
            redesign_channel_history,
```
(Ensure they're imported / path-qualified the same way as `redesign_send_dm` in that file.)

- [ ] **Step 4: wire the forwarder in login (commands.rs)**

In `commands::login`, the `RedesignRuntime::start(...)` call currently passes one `on_dm` closure that calls `emit_redesign_dm_received`. It now must pass a second `on_channel` closure. Mirror the DM closure exactly (it captures an `AppHandle` clone). Add after the `on_dm` closure argument:
```rust
        move |msg: mesh_talk::node::channel::ReceivedChannelMessage| {
            crate::events::emit_redesign_channel_message(
                &app_handle_for_channel,
                hex::encode(msg.channel_id.as_bytes()),
                msg.channel_name,
                msg.from,
                String::from_utf8_lossy(&msg.text).into_owned(),
            );
        },
```
Clone the app handle for this closure next to wherever the DM closure's handle is cloned:
```rust
        let app_handle_for_channel = app_handle.clone();
```
(Match the actual variable names in `login` — the DM closure shows the exact pattern, including whether it's `app`, `app_handle`, or a captured `AppHandle<R>`. Use the same crate path for `ReceivedChannelMessage` that the file uses for other node types — `crate::node::...` vs `mesh_talk::node::...`.)

- [ ] **Step 5: build, clippy, fmt, commit**
```bash
cd src-tauri && nice -n 10 cargo build --lib --bins
cd src-tauri && nice -n 10 cargo clippy --lib --bins -- -D warnings 2>&1 | tail -20
cd src-tauri && nice -n 10 cargo fmt
cd /Users/kingsonwu/programming/rust-src/mesh-talk
git add src-tauri/src/events.rs src-tauri/src/redesign_commands.rs src-tauri/src/commands.rs src-tauri/src/lib.rs
git commit -m "feat(app): channel IPC commands + redesign-channel-message event + login forwarder"
git status | head -1
```
Expected: the whole app builds (the Task-1 signature change is now satisfied); clippy clean.

---

### Task 3: the channels UI in the `/redesign` route

**Files:** Modify `frontend/src/services/api.js`, `frontend/src/views/redesign/RedesignChatView.vue`.

- [ ] **Step 1: `channelsAPI` (api.js)**

Append to the `redesignAPI` object (or add a sibling `channelsAPI`) mirroring the existing `redesignAPI` invoke style:
```js
  listChannels: () => invoke('redesign_list_channels'),
  createChannel: (name, memberIds) => invoke('redesign_create_channel', { name, memberIds }),
  sendChannelMessage: (channelId, text) =>
    invoke('redesign_send_channel_message', { channelId, text }),
  channelHistory: (channelId, limit = 100) =>
    invoke('redesign_channel_history', { channelId, limit }),
```
(Match the existing parameter-naming convention — Tauri maps camelCase JS args to snake_case Rust params automatically in v2, but confirm against how `sendDm`/`history` pass `recipient`/`peer`/`limit`.)

- [ ] **Step 2: channels section in RedesignChatView.vue**

Add a channels list alongside the existing peer list, a "New channel" action (name + multi-select from the peer list), a per-channel message pane reusing the history/optimistic-send rendering, and a `listen('redesign-channel-message')` handler. Concretely:
- State: `channels` (from `channelsAPI.listChannels()`, polled on the same 3s timer as peers, or refreshed after create/receive), `activeChannel`, `channelMessages`.
- `loadChannels()`: `channels.value = await channelsAPI.listChannels()`.
- `selectChannel(c)`: set `activeChannel`, load `channelsAPI.channelHistory(c.channel_id)` into `channelMessages` (apply the SAME stale-guard the DM `loadHistory` uses — capture the target id, bail if `activeChannel.value?.channel_id !== target`).
- `createChannel()`: collect a name + selected peer `user_id`s, `await channelsAPI.createChannel(name, ids)`, then `loadChannels()` and select the new id.
- `sendChannelMessage()`: optimistic-append then `await channelsAPI.sendChannelMessage(activeChannel.value.channel_id, text)`.
- `onMounted`: `listen('redesign-channel-message', (e) => { ... })` — if `e.payload.channel_id === activeChannel.value?.channel_id`, append `{ from_me: false, who: e.payload.from, text: e.payload.text }` to `channelMessages`; otherwise bump an unread badge on that channel. Refresh `loadChannels()` so a brand-new channel created by a peer appears. Store the unlisten fn and call it in `onUnmounted` (match the DM listener's cleanup).
- Keep it visually consistent with the existing DM pane; a simple tab or two-column (peers | channels) layout is fine. Do not restyle the DM pane.

- [ ] **Step 3: build the frontend + commit**
```bash
cd frontend && npm run build
cd /Users/kingsonwu/programming/rust-src/mesh-talk
git add frontend/src/services/api.js frontend/src/views/redesign/RedesignChatView.vue
git commit -m "feat(ui): channels section in the /redesign route"
git status | head -1
```
Expected: the frontend builds; the channels list, create, send, and live-receive all function against the backend commands.

---

## Notes for the reviewer

- **End-to-end:** after this plan, a user can create a channel from the UI, it syncs to online members (Plan 4a's `distribute_channel`), members see it appear + receive messages live via `redesign-channel-message`, and history (own + others) renders. Two desktop instances on a LAN exercise the full path.
- **Reviewer checks:** the `create_channel`/`send_channel_message` commands snapshot `handle()` and release the `RedesignState` lock before the `.await` (no lock across await); hex id round-trips correctly (32 bytes, reject bad input); the login `on_channel` closure clones its OWN app handle (doesn't move the DM one); the Vue listener applies the stale-channel guard + cleans up its unlisten; `member_ids` resolution errors clearly on an unknown peer.
- **Deferred (Phase 2+):** dynamic add/remove member from the UI (the node rotation read-side is proven; live add-member over the network is a small follow-up), channel member listing/avatars, channel search, and the Double Ratchet / multi-device work.
