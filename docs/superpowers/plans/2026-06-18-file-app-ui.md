# File Sharing in the App + UI Implementation Plan (File Plan 3)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make file sharing user-facing in `/redesign`: forward received files to a Tauri event, add IPC commands to send/save files, and add an attach button + file cards (with Save) to the chat view.

**Architecture:** Mirrors the channel app/UI wiring. `RedesignRuntime::start` gains an `on_file` callback wired to the node's `file_incoming` stream; `commands::login` emits a `redesign-file-received` event. IPC commands `redesign_send_file_dm`/`redesign_send_file_channel`/`redesign_save_file` delegate to the node, converting hex ↔ `ConversationId`. The Vue view gets a 📎 attach button (native file picker → send) and renders received files as cards with a Save button (native save dialog → `save_file`).

**Tech Stack:** Rust + Tauri v2; Vue 3 `<script setup>`; `@tauri-apps/plugin-dialog` (`open`, `save` — already a dependency, used by the legacy `ChatView.vue`); `hex` (already used in `redesign_commands.rs`).

---

## Background the implementer needs

This is **File Plan 3**, the last file slice. File Plan 2 + bounded sync shipped the node API:
```rust
// Node:
//   send_file_dm(&self, recipient: &str, path: &Path) -> Result<ConversationId, NodeError>
//   send_file_channel(&self, channel: ConversationId, path: &Path) -> Result<ConversationId, NodeError>
//   save_file(&self, file_conv: ConversationId, dest: &Path) -> Result<(), NodeError>   // SYNC
// crate::node::filebook::ReceivedFile { conv: ConversationId, from: String, name: String, size: u64, mime: String, file_conv: ConversationId }
// files ≤ 8 MB transfer over multiple sync rounds.
```
**Patterns to mirror (read them first):**
- `src-tauri/src/node/runtime.rs` `RedesignRuntime::start`: has `on_dm` + `on_channel` callbacks, each with a forwarder task draining `incoming_rx`/`channel_rx`. The file receiver is currently created and dropped: `let (file_tx, _file_rx) = mpsc::unbounded_channel::<crate::node::filebook::ReceivedFile>();`. `handle() -> Arc<Node>`, `peer_public(&str) -> Option<PublicIdentity>`.
- `src-tauri/src/events.rs`: `EVENT_REDESIGN_CHANNEL_MESSAGE` + `RedesignChannelMessageEvent` + `emit_redesign_channel_message<R: Runtime>(app, ...)`. Mirror for files.
- `src-tauri/src/redesign_commands.rs`: `RedesignState`, `NOT_STARTED`, `parse_channel_id(&str) -> Result<ConversationId, String>` (REUSE for file_conv + channel_id), `redesign_send_channel_message` (snapshot `handle()`, release lock, await), `hex::encode(id.as_bytes())`.
- `src-tauri/src/commands.rs` `login` (~line 295-323): builds `app_handle_for_dm` + `app_handle_for_channel` clones and passes `move |dm| {...}`, `move |msg| {...}` closures to `RedesignRuntime::start`.
- `src-tauri/src/lib.rs`: `tauri::generate_handler![...]` lists the `redesign_*` commands.
- `frontend/src/services/api.js`: `redesignAPI { ..., listChannels, createChannel, sendChannelMessage, channelHistory }`, exported as `API.redesign`.
- `frontend/src/views/redesign/RedesignChatView.vue`: DM + channel chat. `messages` items are `{ from_me, who, text, wall_clock }`. `activePeer` / `activeChannel` (mutually exclusive). `send()` branches on which is active. `onMounted` sets a 3s poll + `listen('redesign-dm-received')` + `listen('redesign-channel-message')`, cleaned up in `onBeforeUnmount`.
- `frontend/src/views/chat/ChatView.vue` (legacy, for the dialog API): `import { open as openDialog, save as saveDialog } from "@tauri-apps/plugin-dialog";` — `const sel = await openDialog({ multiple: false })` returns a path string (or null); `const dest = await saveDialog({ defaultPath: name })` returns a path string (or null).

**CPU/test discipline:** `nice -n 10`; build with `cargo build --lib --bins`; `cargo clippy --lib --bins -- -D warnings`; `cargo fmt`; frontend `npm run build`. Confirm `git status | head -1` == `On branch feat/redesign-phase0` after committing.

---

### Task 1: backend — runtime `on_file` + event + IPC commands + login wiring

**Files:** Modify `src-tauri/src/node/runtime.rs`, `events.rs`, `redesign_commands.rs`, `lib.rs`, `commands.rs`.

- [ ] **Step 1: runtime `on_file` forwarder (runtime.rs)**

Add a third callback param after `on_channel`:
```rust
        on_file: impl Fn(crate::node::filebook::ReceivedFile) + Send + 'static,
```
Bind the file receiver (change `_file_rx` to `mut file_rx`):
```rust
        let (file_tx, mut file_rx) =
            mpsc::unbounded_channel::<crate::node::filebook::ReceivedFile>();
```
After the `on_channel` forwarder task, add:
```rust
        tasks.push(tokio::spawn(async move {
            while let Some(f) = file_rx.recv().await {
                on_file(f);
            }
        }));
```
Update BOTH `RedesignRuntime::start(...)` calls in runtime.rs's own `#[cfg(test)] mod tests` to pass an extra `|_f| {}` after the `|_ch| {}` closure.

- [ ] **Step 2: the event (events.rs)**

Add next to `EVENT_REDESIGN_CHANNEL_MESSAGE`:
```rust
pub const EVENT_REDESIGN_FILE_RECEIVED: &str = "redesign-file-received";
```
Add the payload struct:
```rust
#[derive(serde::Serialize, Clone)]
pub struct RedesignFileReceivedEvent {
    pub conv: String,      // hex (channel id for a channel file; the DM conv otherwise)
    pub from: String,      // sender user-id
    pub name: String,
    pub size: u64,
    pub file_conv: String, // hex — pass to save
}
```
Add the emit helper mirroring `emit_redesign_channel_message`:
```rust
pub fn emit_redesign_file_received<R: Runtime>(
    app_handle: &tauri::AppHandle<R>,
    conv: String,
    from: String,
    name: String,
    size: u64,
    file_conv: String,
) {
    let event = RedesignFileReceivedEvent { conv, from, name, size, file_conv };
    if let Err(e) = app_handle.emit(EVENT_REDESIGN_FILE_RECEIVED, event) {
        log::error!("Failed to emit redesign file event: {e}");
    }
}
```

- [ ] **Step 3: IPC commands (redesign_commands.rs)**

Add `use std::path::PathBuf;` (or use `Path::new`). Add three commands (REUSE the existing `parse_channel_id` for both `channel_id` and `file_conv`):
```rust
#[tauri::command]
pub async fn redesign_send_file_dm(
    state: tauri::State<'_, RedesignState>,
    recipient: String,
    path: String,
) -> Result<String, String> {
    let node = {
        let guard = state.0.lock().await;
        let rt = guard.as_ref().ok_or_else(|| NOT_STARTED.to_string())?;
        rt.handle()
    };
    let id = node
        .send_file_dm(&recipient, std::path::Path::new(&path))
        .await
        .map_err(|e| e.to_string())?;
    Ok(hex::encode(id.as_bytes()))
}

#[tauri::command]
pub async fn redesign_send_file_channel(
    state: tauri::State<'_, RedesignState>,
    channel_id: String,
    path: String,
) -> Result<String, String> {
    let id = parse_channel_id(&channel_id)?;
    let node = {
        let guard = state.0.lock().await;
        let rt = guard.as_ref().ok_or_else(|| NOT_STARTED.to_string())?;
        rt.handle()
    };
    let file_conv = node
        .send_file_channel(id, std::path::Path::new(&path))
        .await
        .map_err(|e| e.to_string())?;
    Ok(hex::encode(file_conv.as_bytes()))
}

#[tauri::command]
pub async fn redesign_save_file(
    state: tauri::State<'_, RedesignState>,
    file_conv: String,
    dest: String,
) -> Result<(), String> {
    let id = parse_channel_id(&file_conv)?;
    let node = {
        let guard = state.0.lock().await;
        let rt = guard.as_ref().ok_or_else(|| NOT_STARTED.to_string())?;
        rt.handle()
    };
    // save_file is synchronous (reads chunk events, decrypts, writes) — run it on a
    // blocking thread so it doesn't stall the async runtime on a large file.
    tokio::task::spawn_blocking(move || node.save_file(id, std::path::Path::new(&dest)))
        .await
        .map_err(|e| format!("join error: {e}"))?
        .map_err(|e| e.to_string())
}
```
(`Arc<Node>` is `Send + Sync`, so the `spawn_blocking` move is fine. If `parse_channel_id` is currently private, keep it private — these commands are in the same module.)

- [ ] **Step 4: register handlers (lib.rs)**

Add to `tauri::generate_handler![...]` next to the other `redesign_*` entries:
```rust
            redesign_send_file_dm,
            redesign_send_file_channel,
            redesign_save_file,
```

- [ ] **Step 5: wire the forwarder in login (commands.rs)**

Near `let app_handle_for_channel = app_handle.clone();`, add `let app_handle_for_file = app_handle.clone();`. Add the third closure to the `RedesignRuntime::start(...)` call after the `on_channel` closure:
```rust
                    move |f: mesh_talk::node::filebook::ReceivedFile| {
                        crate::events::emit_redesign_file_received(
                            &app_handle_for_file,
                            hex::encode(f.conv.as_bytes()),
                            f.from,
                            f.name,
                            f.size,
                            hex::encode(f.file_conv.as_bytes()),
                        );
                    },
```
(Use the crate path the file already uses for node types — `crate::node::...` vs `mesh_talk::node::...`; match the `on_channel` closure's `ReceivedChannelMessage` path.)

- [ ] **Step 6: build, clippy, fmt, commit**
```bash
cd src-tauri && nice -n 10 cargo build --lib --bins
cd src-tauri && nice -n 10 cargo clippy --lib --bins -- -D warnings 2>&1 | tail -20
cd src-tauri && nice -n 10 cargo fmt
cd /Users/kingsonwu/programming/rust-src/mesh-talk
git add src-tauri/src/node/runtime.rs src-tauri/src/events.rs src-tauri/src/redesign_commands.rs src-tauri/src/lib.rs src-tauri/src/commands.rs
git commit -m "feat(app): file IPC commands + redesign-file-received event + runtime/login forwarder"
git status | head -1
```
Expected: full app builds; clippy clean; runtime tests still pass (`nice -n 10 cargo test --lib node::runtime -- --test-threads=2`).

---

### Task 2: frontend — attach button + file cards (RedesignChatView.vue)

**Files:** Modify `frontend/src/services/api.js`, `frontend/src/views/redesign/RedesignChatView.vue`.

- [ ] **Step 1: `redesignAPI` file methods (api.js)**

Append to `redesignAPI`:
```js
  sendFileDm: (recipient, path) => invoke("redesign_send_file_dm", { recipient, path }),
  sendFileChannel: (channelId, path) => invoke("redesign_send_file_channel", { channelId, path }),
  saveFile: (fileConv, dest) => invoke("redesign_save_file", { fileConv, dest }),
```

- [ ] **Step 2: attach button + file cards (RedesignChatView.vue)**

**Script:**
- Import the dialog API: `import { open as openDialog, save as saveDialog } from "@tauri-apps/plugin-dialog";`.
- `let unlistenFile = null;` near the other unlisten vars.
- `async function attachFile()`:
  ```js
  if (!activePeer.value && !activeChannel.value) return;
  const sel = await openDialog({ multiple: false });
  const path = typeof sel === "string" ? sel : null;
  if (!path) return;
  const name = path.split(/[\\/]/).pop();
  error.value = "";
  try {
    let fileConv;
    if (activeChannel.value) {
      fileConv = await API.redesign.sendFileChannel(activeChannel.value.channel_id, path);
    } else {
      fileConv = await API.redesign.sendFileDm(activePeer.value.user_id, path);
    }
    messages.value.push({ from_me: true, who: "you", file: { name, size: null, file_conv: fileConv }, wall_clock: Date.now() });
    await scrollDown();
  } catch (e) { error.value = String(e); }
  ```
- `function onFileReceived(payload)`:
  ```js
  const card = { from_me: false, who: payload.from, file: { name: payload.name, size: payload.size, file_conv: payload.file_conv }, wall_clock: Date.now() };
  if (activeChannel.value && payload.conv === activeChannel.value.channel_id) {
    messages.value.push(card); void scrollDown();
  } else if (activePeer.value && payload.from === activePeer.value.user_id) {
    messages.value.push(card); void scrollDown();
  }
  // else: a file for an inactive conversation — ignored in this MVP (no unread badge for files)
  ```
- `async function saveReceivedFile(file)`:
  ```js
  const dest = await saveDialog({ defaultPath: file.name });
  if (typeof dest !== "string" || !dest) return;
  error.value = "";
  try { await API.redesign.saveFile(file.file_conv, dest); }
  catch (e) { error.value = String(e); }
  ```
- In `onMounted`, after the channel listener: `unlistenFile = await listen("redesign-file-received", (ev) => onFileReceived(ev.payload ?? {}));`.
- In `onBeforeUnmount`: `if (typeof unlistenFile === "function") unlistenFile();`.

**Template:**
- Add a 📎 attach button inside the composer (next to the send button), shown whenever the chat pane is active, `@click="attachFile"`:
  ```html
  <button type="button" class="attach" title="Send a file" @click="attachFile">📎</button>
  ```
- In the messages loop, render a file card when `m.file` is set instead of the text span:
  ```html
  <div v-for="(m, i) in messages" :key="i" class="msg" :class="{ mine: m.from_me }">
    <span class="who">{{ m.from_me ? "you" : m.who }}</span>
    <template v-if="m.file">
      <span class="file-card">
        📄 {{ m.file.name }}<span v-if="m.file.size"> · {{ Math.ceil(m.file.size / 1024) }} KB</span>
        <button v-if="!m.from_me" class="save" @click="saveReceivedFile(m.file)">Save</button>
      </span>
    </template>
    <span v-else class="text">{{ m.text }}</span>
  </div>
  ```
- Add minimal styles for `.attach`, `.file-card`, `.save` consistent with the existing dark theme (reuse `.composer button` colors; a file card can be a subtle bordered row). Do NOT restyle existing elements.

- [ ] **Step 3: build + commit**
```bash
cd frontend && npm run build 2>&1 | tail -20
cd /Users/kingsonwu/programming/rust-src/mesh-talk
git add frontend/src/services/api.js frontend/src/views/redesign/RedesignChatView.vue
git commit -m "feat(ui): file attach button + received-file cards with Save in the /redesign view"
git status | head -1
```
Expected: `npm run build` succeeds; attach picks a file and sends it; received files appear as cards with a working Save.

---

## Notes for the reviewer

- **End-to-end:** a user attaches a file in a DM or channel (native picker → `send_file_*`), members receive a `redesign-file-received` event and see a file card, and Save writes the reassembled+verified bytes to a chosen path. Two desktop instances exercise the full path for files ≤ 8 MB.
- **Reviewer checks:** the send/save commands snapshot `handle()` and release the `RedesignState` lock before the `.await` (no lock across await); `save_file` runs on `spawn_blocking` (it's sync + can be slow on a large file); the login `on_file` closure clones its OWN app handle; the Vue file listener routes by `conv` (channel) or `from` (DM) and is cleaned up on unmount; hex round-trips for `file_conv`.
- **Deferred (note, don't build):** file cards are session-only (no durable received-file history — `FileBook` is in-memory, so a restart loses them and an inactive-conversation file is dropped, not badged); progress UI during transfer; drag-and-drop; image previews; on-demand chunk fetch + `have`-set compaction for files > 8 MB.
