# Threads / Reply-To: App + UI Implementation Plan (Threads Plan 2)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make reply-to user-facing in `/redesign`: send commands + received events + history carry `reply_to`, and the chat view gets a reply button, composer reply-context, and parent-snippet rendering.

**Architecture:** Mirrors the reactions/file app wiring. The send commands gain an optional `reply_to` (hex) → `send_dm_reply`/`send_channel_message_reply`. `HistoryItem` + the `redesign-dm-received`/`redesign-channel-message` events carry `reply_to` (hex). The Vue view sets a pending reply target, sends it, and renders each message's parent context by resolving `reply_to` against messages already in view.

**Tech Stack:** Rust + Tauri v2; Vue 3; `hex`. No new dependencies.

---

## Background the implementer needs

Threads Plan 1 (done) added on `Node`:
```rust
// send_dm_reply(&self, recipient: &str, text: &[u8], reply_to: Option<EventId>) -> Result<(), NodeError>
// send_channel_message_reply(&self, channel: ConversationId, text: &[u8], reply_to: Option<EventId>) -> Result<(), NodeError>
// (send_dm / send_channel_message still exist, delegating with None)
// HistoryEntry { id, from_me, who, text, wall_clock, reply_to: Option<EventId> }
// ReceivedDm { from, from_name, text, reply_to: Option<EventId> }
// ReceivedChannelMessage { channel_id, channel_name, from, text, reply_to: Option<EventId> }
```
**Patterns to mirror (read first):**
- `src-tauri/src/redesign_commands.rs`: `redesign_send_dm`/`redesign_send_channel_message` (snapshot `handle()`, await), `redesign_history`/`redesign_channel_history` (`HistoryItem` mapping), `parse_event_id` (hex → `EventId`, added for reactions), `hex::encode`.
- `src-tauri/src/events.rs`: `RedesignDmReceivedEvent { from, from_name, text }` + `emit_redesign_dm_received(app, from, from_name, text)`; `RedesignChannelMessageEvent { channel_id, channel_name, from, text }` + `emit_redesign_channel_message(...)`.
- `src-tauri/src/commands.rs` `login`: the `on_dm`/`on_channel` closures call the emit fns with `dm.*`/`msg.*` fields.
- `frontend/src/services/api.js`: `redesignAPI.sendDm`/`sendChannelMessage`.
- `frontend/src/views/redesign/RedesignChatView.vue`: `messages` items `{ id, from_me, who, text|file, reply_to? }`; `send()`; the inbound listeners; reaction bar render.

**CPU/test discipline:** `nice -n 10`; `cargo build --lib --bins`; `cargo clippy --lib --bins -- -D warnings`; `cargo fmt`; frontend `npm run build`. Confirm branch line after committing.

---

### Task 1: backend — reply_to on send commands + events + history

**Files:** Modify `src-tauri/src/redesign_commands.rs`, `events.rs`, `commands.rs`.

- [ ] **Step 1: send commands gain optional `reply_to` (redesign_commands.rs)**

Add `reply_to: Option<String>` (hex) as a trailing param to `redesign_send_dm` and `redesign_send_channel_message`. Parse it (None → None; Some → `parse_event_id`) and call the `_reply` variant:
```rust
// redesign_send_dm: after snapshotting `node`:
    let reply = match reply_to {
        Some(h) => Some(parse_event_id(&h)?),
        None => None,
    };
    node.send_dm_reply(&recipient, text.as_bytes(), reply).await.map_err(|e| e.to_string())
// redesign_send_channel_message: likewise with node.send_channel_message_reply(id, text.as_bytes(), reply)
```
(Tauri maps an omitted JS arg to `None` for an `Option` param, so existing callers keep working.)

- [ ] **Step 2: `HistoryItem.reply_to` (redesign_commands.rs)**

Add `pub reply_to: Option<String>` to `HistoryItem`; in BOTH `redesign_history` and `redesign_channel_history` mappers add:
```rust
            reply_to: h.reply_to.map(|id| hex::encode(id.as_bytes())),
```

- [ ] **Step 3: events carry `reply_to` (events.rs)**

Add `pub reply_to: Option<String>` to `RedesignDmReceivedEvent` and `RedesignChannelMessageEvent`. Add a `reply_to: Option<EventId>` param to `emit_redesign_dm_received` and `emit_redesign_channel_message`, set on the event as `reply_to: reply_to.map(|id| hex::encode(id.as_bytes()))`. (Import `EventId` / `hex` as needed; `hex` is already used elsewhere.)

- [ ] **Step 4: login closures pass `reply_to` (commands.rs)**

In the `on_dm` closure: `emit_redesign_dm_received(&app_handle_for_dm, dm.from, dm.from_name, dm.text, dm.reply_to)`. In the `on_channel` closure: `emit_redesign_channel_message(&app_handle_for_channel, hex::encode(msg.channel_id.as_bytes()), msg.channel_name, msg.from, msg.text, msg.reply_to)`. (The emit fn takes `Option<EventId>` and hex-encodes internally.)

- [ ] **Step 5: build, clippy, fmt, commit**
```bash
cd src-tauri && nice -n 10 cargo build --lib --bins
cd src-tauri && nice -n 10 cargo clippy --lib --bins -- -D warnings 2>&1 | tail -20
cd src-tauri && nice -n 10 cargo fmt
cd /Users/kingsonwu/programming/rust-src/mesh-talk
git add src-tauri/src/redesign_commands.rs src-tauri/src/events.rs src-tauri/src/commands.rs
git commit -m "feat(app): reply_to on send commands + received events + HistoryItem"
git status | head -1
```

---

### Task 2: frontend — reply button + composer context + parent snippet

**Files:** Modify `frontend/src/services/api.js`, `frontend/src/views/redesign/RedesignChatView.vue`.

- [ ] **Step 1: api methods (api.js)**

Update the send methods to pass an optional `replyTo`:
```js
  sendDm: (recipient, text, replyTo = null) =>
    invoke("redesign_send_dm", { recipient, text, replyTo }),
  sendChannelMessage: (channelId, text, replyTo = null) =>
    invoke("redesign_send_channel_message", { channelId, text, replyTo }),
```

- [ ] **Step 2: reply UI (RedesignChatView.vue)**

**Script:**
- `const replyingTo = ref(null);` (the message being replied to, or null).
- `function startReply(m) { if (m.id) replyingTo.value = m; }`
- `function cancelReply() { replyingTo.value = null; }`
- `function messageById(id) { return messages.value.find((m) => m.id === id); }`
- `function replyPreview(m) { const p = m.reply_to ? messageById(m.reply_to) : null; if (!p) return null; return { who: p.from_me ? "you" : p.who, text: (p.text || "").slice(0, 60) }; }`
- Update `send()`: pass `replyingTo.value?.id || null` to `sendDm`/`sendChannelMessage`; include `reply_to` on the optimistic pushed message (`reply_to: replyingTo.value?.id || null`); then `replyingTo.value = null`.
- The inbound DM/channel listeners already append messages — include `reply_to: payload.reply_to ?? null` on the appended object.
- `loadHistory`/`loadChannelHistory` already map items — ensure each carries `reply_to` (the backend now returns it; just don't strip it).

**Template:**
- A "↩" reply button on each message with an `m.id` (place it near the reaction bar / on hover). `@click="startReply(m)"`.
- A parent-snippet line ABOVE a message's content when `m.reply_to` resolves:
  ```html
  <span v-if="m.reply_to && replyPreview(m)" class="reply-context">
    ↩ {{ replyPreview(m).who }}: {{ replyPreview(m).text }}
  </span>
  ```
  (When the parent isn't loaded, `replyPreview` returns null → show a muted fallback `<span v-else-if="m.reply_to" class="reply-context muted">↩ replying to a message</span>`.)
- A composer reply banner when `replyingTo` is set, above the composer input:
  ```html
  <div v-if="replyingTo" class="reply-banner">
    Replying to {{ replyingTo.from_me ? "you" : replyingTo.who }}: "{{ (replyingTo.text || "").slice(0, 50) }}"
    <button type="button" class="cancel" @click="cancelReply">✕</button>
  </div>
  ```
- Minimal dark-theme styles for `.reply-context`, `.reply-context.muted`, `.reply-banner`, the reply button. Don't restyle existing elements.

- [ ] **Step 3: build + commit**
```bash
cd frontend && npm run build 2>&1 | tail -20
cd /Users/kingsonwu/programming/rust-src/mesh-talk
git add frontend/src/services/api.js frontend/src/views/redesign/RedesignChatView.vue
git commit -m "feat(ui): reply-to — reply button, composer context, parent snippet"
git status | head -1
```
Expected: `npm run build` succeeds; clicking ↩ sets a reply target, sending threads it, and the reply renders its parent context.

---

## Notes for the reviewer

- **End-to-end:** a user clicks ↩ on a message, the composer shows the reply target, sending carries `reply_to`, and the reply renders "↩ <who>: <snippet>" above it (for both peers). Works in DMs + channels; backward-compatible (old messages have no `reply_to`).
- **Reviewer checks:** the send commands snapshot `handle()` before await; `parse_event_id` rejects bad hex; optional `reply_to` defaults to `None` for existing callers; the optimistic + inbound + history message objects all carry `reply_to`; `replyPreview` handles an unloaded parent gracefully.
- **Deferred:** nested thread panes; reply counts; jump-to-parent when off-screen; reply notifications.
