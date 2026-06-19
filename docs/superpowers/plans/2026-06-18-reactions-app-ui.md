# Reactions: App + UI Implementation Plan (Reactions Plan 2)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make reactions user-facing in `/redesign`: IPC commands to react + query reactions, the message `id` on history, and a react button + reaction chips in the chat view.

**Architecture:** Mirrors the file/channel app wiring. A tiny `Node::reactions_dm` wrapper (like `dm_history`) lets a DM reaction query resolve a peer to its conversation. IPC commands `redesign_react_dm`/`redesign_react_channel`/`redesign_reactions`/`redesign_channel_reactions` delegate to the node. `HistoryItem` gains `id` so the UI can target a message. The Vue view adds a react affordance per message and renders aggregated chips, reloading reactions on send + on inbound activity (no live push this slice).

**Tech Stack:** Rust + Tauri v2; Vue 3; `hex` (already used). No new dependencies.

---

## Background the implementer needs

Reactions Plan 1 (done) added on `Node`:
```rust
// react_dm(&self, recipient: &str, target: EventId, emoji: &str, remove: bool) -> Result<(), NodeError>
// react_channel(&self, channel: ConversationId, target: EventId, emoji: &str, remove: bool) -> Result<(), NodeError>
// reactions(&self, conv: ConversationId) -> Vec<ReactionView>   // ReactionView { target: String(hex), emoji: String, who: Vec<String> }
// HistoryEntry now has `id: EventId` (first field)
```
**Patterns to mirror (read first):**
- `src-tauri/src/redesign_commands.rs`: `redesign_history` (resolves a `peer` user-id to `rt.peer_public(peer)` then calls `rt.history(&public, limit)`), `redesign_channel_history` (uses `parse_channel_id`), `HistoryItem { from_me, who, text, wall_clock }`, the `NOT_STARTED` guard, `hex::encode`, the snapshot-`handle()`-then-await pattern in `redesign_send_channel_message`.
- `src-tauri/src/node/runtime.rs`: `history(&self, peer: &PublicIdentity, limit) -> Vec<HistoryEntry>` wraps `node.dm_history`; `channel_history(&self, ConversationId, usize)`; `peer_public(&str) -> Option<PublicIdentity>`; `handle() -> Arc<Node>`.
- `src-tauri/src/node/node.rs`: `dm_history(&self, peer: &PublicIdentity, limit) -> Vec<HistoryEntry>` = `self.history(dm_conversation_id(&self.identity.public(), peer), limit)`. Mirror this for reactions.
- `src-tauri/src/lib.rs`: the `redesign_*` handler list.
- `frontend/src/services/api.js`: `redesignAPI` (`history`, `channelHistory`, `sendChannelMessage`, file methods…).
- `frontend/src/views/redesign/RedesignChatView.vue`: `messages` items render `{ from_me, who, text|file }`; `loadHistory`/`loadChannelHistory`; the inbound listeners; the 3s poll.

**CPU/test discipline:** `nice -n 10`. Build `cargo build --lib --bins`, `clippy --lib --bins -- -D warnings`, `cargo fmt`; frontend `npm run build`. Confirm branch line after committing.

---

### Task 1: backend — `Node::reactions_dm` + runtime accessors + IPC commands + `HistoryItem.id`

**Files:** Modify `src-tauri/src/node/node.rs`, `node/runtime.rs`, `redesign_commands.rs`, `lib.rs`.

- [ ] **Step 1: `Node::reactions_dm` (node.rs)**

Mirror `dm_history`:
```rust
    /// Aggregated reactions in the DM with `peer` (derives the conversation id).
    pub fn reactions_dm(&self, peer: &PublicIdentity) -> Vec<ReactionView> {
        let conv = dm_conversation_id(&self.identity.public(), peer);
        self.reactions(conv)
    }
```

- [ ] **Step 2: runtime accessors (runtime.rs)**

Add (mirroring `history`/`channel_history`):
```rust
    /// Aggregated reactions in the DM with `peer`.
    pub fn reactions_dm(&self, peer: &PublicIdentity) -> Vec<crate::node::reaction::ReactionView> {
        self.node.reactions_dm(peer)
    }
    /// Aggregated reactions in a channel.
    pub fn channel_reactions(
        &self,
        channel: crate::eventlog::event::ConversationId,
    ) -> Vec<crate::node::reaction::ReactionView> {
        self.node.reactions(channel)
    }
```
(React SENDING goes through `handle()` in the command, like `redesign_send_dm`, so no runtime wrapper for `react_*`.)

- [ ] **Step 3: IPC commands + DTO + `HistoryItem.id` (redesign_commands.rs)**

Add `id` to `HistoryItem` and map it in BOTH `redesign_history` and `redesign_channel_history`:
```rust
pub struct HistoryItem {
    pub id: String, // hex EventId
    pub from_me: bool,
    pub who: String,
    pub text: String,
    pub wall_clock: u64,
}
// in both history mappers, add: id: hex::encode(h.id.as_bytes()),
```
Add a reactions DTO + four commands. To parse a hex `EventId`, add a helper (mirror `parse_channel_id`):
```rust
use crate::eventlog::event::EventId;

#[derive(Serialize)]
pub struct ReactionInfo {
    pub target: String, // hex EventId
    pub emoji: String,
    pub who: Vec<String>,
}

fn parse_event_id(hex_id: &str) -> Result<EventId, String> {
    let bytes = hex::decode(hex_id).map_err(|_| "invalid event id".to_string())?;
    let arr: [u8; 32] = bytes.try_into().map_err(|_| "event id must be 32 bytes".to_string())?;
    Ok(EventId::new(arr))
}

fn to_reaction_infos(views: Vec<crate::node::reaction::ReactionView>) -> Vec<ReactionInfo> {
    views.into_iter().map(|v| ReactionInfo { target: v.target, emoji: v.emoji, who: v.who }).collect()
}

#[tauri::command]
pub async fn redesign_react_dm(
    state: tauri::State<'_, RedesignState>,
    recipient: String,
    target: String,
    emoji: String,
    remove: bool,
) -> Result<(), String> {
    let id = parse_event_id(&target)?;
    let node = {
        let guard = state.0.lock().await;
        let rt = guard.as_ref().ok_or_else(|| NOT_STARTED.to_string())?;
        rt.handle()
    };
    node.react_dm(&recipient, id, &emoji, remove).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn redesign_react_channel(
    state: tauri::State<'_, RedesignState>,
    channel_id: String,
    target: String,
    emoji: String,
    remove: bool,
) -> Result<(), String> {
    let channel = parse_channel_id(&channel_id)?;
    let id = parse_event_id(&target)?;
    let node = {
        let guard = state.0.lock().await;
        let rt = guard.as_ref().ok_or_else(|| NOT_STARTED.to_string())?;
        rt.handle()
    };
    node.react_channel(channel, id, &emoji, remove).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn redesign_reactions(
    state: tauri::State<'_, RedesignState>,
    peer: String,
) -> Result<Vec<ReactionInfo>, String> {
    let guard = state.0.lock().await;
    let rt = guard.as_ref().ok_or_else(|| NOT_STARTED.to_string())?;
    let public = rt.peer_public(&peer).ok_or_else(|| format!("unknown peer: {peer}"))?;
    Ok(to_reaction_infos(rt.reactions_dm(&public)))
}

#[tauri::command]
pub async fn redesign_channel_reactions(
    state: tauri::State<'_, RedesignState>,
    channel_id: String,
) -> Result<Vec<ReactionInfo>, String> {
    let channel = parse_channel_id(&channel_id)?;
    let guard = state.0.lock().await;
    let rt = guard.as_ref().ok_or_else(|| NOT_STARTED.to_string())?;
    Ok(to_reaction_infos(rt.channel_reactions(channel)))
}
```

- [ ] **Step 4: register handlers (lib.rs)**
```rust
            redesign_react_dm,
            redesign_react_channel,
            redesign_reactions,
            redesign_channel_reactions,
```

- [ ] **Step 5: build, clippy, fmt, commit**
```bash
cd src-tauri && nice -n 10 cargo build --lib --bins
cd src-tauri && nice -n 10 cargo clippy --lib --bins -- -D warnings 2>&1 | tail -20
cd src-tauri && nice -n 10 cargo fmt
cd /Users/kingsonwu/programming/rust-src/mesh-talk
git add src-tauri/src/node/node.rs src-tauri/src/node/runtime.rs src-tauri/src/redesign_commands.rs src-tauri/src/lib.rs
git commit -m "feat(app): reaction IPC commands + HistoryItem.id + reactions query"
git status | head -1
```

---

### Task 2: frontend — react button + reaction chips (RedesignChatView.vue)

**Files:** Modify `frontend/src/services/api.js`, `frontend/src/views/redesign/RedesignChatView.vue`.

- [ ] **Step 1: `redesignAPI` methods (api.js)**
```js
  reactDm: (recipient, target, emoji, remove) =>
    invoke("redesign_react_dm", { recipient, target, emoji, remove }),
  reactChannel: (channelId, target, emoji, remove) =>
    invoke("redesign_react_channel", { channelId, target, emoji, remove }),
  reactions: (peer) => invoke("redesign_reactions", { peer }),
  channelReactions: (channelId) => invoke("redesign_channel_reactions", { channelId }),
```

- [ ] **Step 2: react UI (RedesignChatView.vue)**

Messages now carry `id` (from history) — keep it on each message object built in `loadHistory`/`loadChannelHistory` (they map the items; ensure `id` passes through; optimistic locally-sent messages have no id yet, so guard the react UI on `m.id`).

**Script:**
- `const myId = ref("")` already exists (the view shows it). Use it to compute "did I react".
- `const reactions = ref([])` — the active conversation's `ReactionInfo[]`.
- `const EMOJIS = ["👍", "❤️", "😂", "🎉", "👀"];` a small fixed palette.
- `async function loadReactions()`:
  ```js
  try {
    reactions.value = activeChannel.value
      ? await API.redesign.channelReactions(activeChannel.value.channel_id)
      : activePeer.value
      ? await API.redesign.reactions(activePeer.value.user_id)
      : [];
  } catch (_e) {}
  ```
- Call `loadReactions()` at the end of `loadHistory` and `loadChannelHistory`, after any inbound DM/channel message handler (so reactions refresh on activity), and in the 3s poll alongside the existing refreshes.
- `function reactionsFor(messageId)`: `return reactions.value.filter(r => r.target === messageId);` (returns `[{emoji, who}]`).
- `function iReacted(r)`: `return r.who.includes(myId.value);`
- `async function toggleReaction(message, emoji)`:
  ```js
  if (!message.id) return;
  const existing = reactions.value.find(r => r.target === message.id && r.emoji === emoji);
  const remove = !!(existing && existing.who.includes(myId.value));
  try {
    if (activeChannel.value) await API.redesign.reactChannel(activeChannel.value.channel_id, message.id, emoji, remove);
    else await API.redesign.reactDm(activePeer.value.user_id, message.id, emoji, remove);
    await loadReactions();
  } catch (e) { error.value = String(e); }
  ```

**Template (inside the message loop, after the text/file content):**
- A small reaction bar under each message with `m.id`: existing chips + a "＋" that reveals the emoji palette.
  ```html
  <div v-if="m.id" class="reactions">
    <button
      v-for="r in reactionsFor(m.id)"
      :key="r.emoji"
      class="chip"
      :class="{ mine: r.who.includes(myId) }"
      @click="toggleReaction(m, r.emoji)"
    >{{ r.emoji }} {{ r.who.length }}</button>
    <span class="react-add">
      <button class="chip add" @click="m._pick = !m._pick">＋</button>
      <span v-if="m._pick" class="palette">
        <button v-for="e in EMOJIS" :key="e" @click="toggleReaction(m, e); m._pick = false">{{ e }}</button>
      </span>
    </span>
  </div>
  ```
  (`m._pick` is transient per-message UI state; since `messages` items are plain objects it can hold it. If Vue reactivity on a new property is an issue, instead use a single `openPickerFor = ref(null)` keyed by `m.id`.)
- Add minimal styles (`.reactions`, `.chip`, `.chip.mine`, `.palette`) consistent with the dark theme. Don't restyle existing elements.

- [ ] **Step 3: build + commit**
```bash
cd frontend && npm run build 2>&1 | tail -20
cd /Users/kingsonwu/programming/rust-src/mesh-talk
git add frontend/src/services/api.js frontend/src/views/redesign/RedesignChatView.vue
git commit -m "feat(ui): message reactions (react button + chips) in the /redesign view"
git status | head -1
```
Expected: `npm run build` succeeds; reacting adds/removes a chip and persists across a reload.

---

## Notes for the reviewer

- **End-to-end:** a user reacts to any DM/channel message via an emoji palette; chips show counts and highlight the user's own reactions; toggling removes; reactions reload on send + inbound activity + the poll.
- **Reviewer checks:** the react/query commands snapshot `handle()`/release the lock before await; `parse_event_id` rejects bad hex; the UI guards the react bar on `m.id` (optimistic sent messages have none until reload); `iReacted`/`mine` uses `myId`; `toggleReaction` computes `remove` from current state.
- **Deferred:** live reaction push (currently reload-based); reaction on files; a richer emoji palette/search; @mentions + threads.
