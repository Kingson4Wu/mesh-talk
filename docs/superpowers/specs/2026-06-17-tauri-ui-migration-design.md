# Tauri UI Migration — redesign node behind commands + a `/redesign` route — Design Spec

Date: 2026-06-17
Status: Approved for planning

## 1. Purpose & scope

Phase 0 built the redesign `node` stack (identity → discovery → Noise transport → event-log + sync → sealed-box DM → post office → persistent history) and proved it with CLI rigs. This slice makes it the **real desktop app**: the Tauri v2 desktop application opens a redesign `Node` on login, exposes it through Tauri commands + events, and a new dedicated Vue route lets a user actually chat over it.

**Strategy: additive, zero-risk to the existing app.** The legacy `NodeService`/`AppState`/`ChatView` stack and its commands/events are left completely untouched and keep working. We ADD: a new managed runtime, four new commands, one new event, and one new self-contained Vue route. The two stacks coexist (their discovery uses different UDP ports).

**Out of scope (later):** channels/groups (Phase 1); merging the redesign into the main chat UI; contact aliasing/verification UI; files/reactions/threads/search; automated Tauri+Vue end-to-end tests. DMs only.

## 2. Components

### 2.1 `node::runtime` (new Rust module, `src-tauri/src/node/runtime.rs`)
The App-context glue between Tauri and the redesign `Node`.
- `pub struct RedesignRuntime { node: Arc<Node>, roster: Arc<Mutex<Roster>>, user_id: String, tcp_port: u16, tasks: Vec<tokio::task::JoinHandle<()>> }`.
- `RedesignRuntime::start(data_dir: &Path, user_id: &str, display_name: &str, password: &str, emit: impl Fn(ReceivedDm) + Send + 'static) -> Result<RedesignRuntime, RuntimeError>`:
  1. `dir = data_dir/redesign/<user_id>/`; `keystore::load_or_create(dir/identity.keystore, password)` → `DeviceIdentity`.
  2. bind an ephemeral TCP listener (`0.0.0.0:0`), read the real `tcp_port`.
  3. build the reuse+broadcast discovery socket on the redesign discovery port; build `Announce::new(&identity, display_name, tcp_port)`.
  4. `Node::open(identity, roster, incoming_tx, dir/messages.log, dir/sent.log, password)`.
  5. spawn `run_listen`, `run_broadcast`, `Node::run_accept_loop`, a periodic `drain_from_post_office`, and an **inbound forwarder** that drains the `ReceivedDm` mpsc and calls `emit(dm)`.
  6. return the runtime (holding the `Node` + roster + the `JoinHandle`s).
- `Drop for RedesignRuntime` aborts all spawned tasks (clean logout/shutdown).
- `RuntimeError` (hand-written enum: `Log(LogError)`, `Io(std::io::Error)`) + Display/Error.
- The discovery-socket helper currently in `bin/mesh-talk-node.rs` (`discovery_socket`) is the same pattern; it is lifted into `runtime` (or a shared `node::net` helper) so both the binary and the app use one implementation.

### 2.2 Tauri managed state + commands (`src-tauri/src/redesign_commands.rs`, new)
- Managed state: `pub struct RedesignState(pub Arc<tokio::sync::Mutex<Option<RedesignRuntime>>>)` — `None` until login, `Some` after; cleared on logout. Registered via `.manage(...)`.
- Commands (registered in `generate_handler!`):
  - `redesign_my_id(state) -> Result<String, String>` — the node's `user_id`.
  - `redesign_list_peers(state) -> Result<Vec<PeerInfo>, String>` — `PeerInfo { user_id, name, addr, post_office }` from the roster snapshot.
  - `redesign_send_dm(state, recipient: String, text: String) -> Result<(), String>` — `Node::send_dm`.
  - `redesign_history(state, peer: String, limit: usize) -> Result<Vec<HistoryItem>, String>` — `HistoryItem { from_me, who, text, wall_clock }` from `Node::dm_history` (resolve `peer` → the peer's `PublicIdentity` via the roster).
  - Each locks `RedesignState`; if `None`, returns `Err("redesign node not started")`.
- Serde DTOs (`PeerInfo`, `HistoryItem`) are `#[derive(Serialize)]` for the IPC boundary; `text` is sent as a UTF-8 `String` (lossy) for display.

### 2.3 Login/logout wiring (`src-tauri/src/lib.rs` + `commands.rs`)
- `.manage(RedesignState(Arc::new(Mutex::new(None))))` in the builder; add the four commands to `generate_handler!`.
- In the existing `login` command, AFTER `login_impl` succeeds (and alongside the legacy network start), spawn `RedesignRuntime::start(app_data_dir, user_id, display_name, password, move |dm| app.emit("redesign-dm-received", DmPayload::from(dm)))` and store it into `RedesignState`. The app-data dir comes from `app.path().app_data_dir()`. A start failure is logged and leaves `RedesignState = None` (legacy login still succeeds).
- In `logout`, take the `RedesignRuntime` out of `RedesignState` and drop it (aborting its tasks).
- New event constant `EVENT_REDESIGN_DM_RECEIVED = "redesign-dm-received"`, payload `{ from: String, from_name: String, text: String }`.

### 2.4 Frontend: a dedicated `/redesign` route (Vue 3)
- `frontend/src/services/api.js`: add a `redesign` namespace wrapping the four `invoke(...)` calls.
- `frontend/src/views/redesign/RedesignChatView.vue` (new): left = peer list (`redesign_list_peers`, refreshed on an interval + a manual refresh button); center = the selected peer's chat (loaded via `redesign_history`, with a send box calling `redesign_send_dm` and optimistic local append since the sender gets no inbound echo); a `listen('redesign-dm-received')` subscription appends live inbound to the active conversation (or marks an unread badge otherwise). Shows the node's own `user_id` (`redesign_my_id`).
- `frontend/src/router` (wherever routes are defined): register `/redesign` and add a nav entry to reach it. No existing component is modified beyond adding the route + nav link.

## 3. Data flow
- **Login:** `login` succeeds → `RedesignRuntime::start` opens the per-user keystore (login password) + node, starts discovery/accept/drain + the inbound forwarder → `RedesignState = Some(runtime)`.
- **Open `/redesign`:** fetch `redesign_my_id` + `redesign_list_peers`; on selecting a peer, `redesign_history`; subscribe to `redesign-dm-received`.
- **Send:** `redesign_send_dm(uid, text)` → `Node::send_dm` (direct dial + sync round; PO-replicate if a post office is on the LAN). UI appends the sent line optimistically.
- **Receive:** peer's event arrives → `Node` emits `ReceivedDm` → forwarder `app.emit("redesign-dm-received", …)` → the view appends it (active conv) or badges it.
- **Logout:** `RedesignRuntime` dropped → tasks aborted; `RedesignState = None`.

## 4. Error handling
- `RedesignRuntime::start` failure (bad password, corrupt files, bind failure) → logged; `RedesignState` stays `None`; legacy login is unaffected; commands return `"redesign node not started"`; the route shows an "unavailable" state.
- A command while `RedesignState = None` → `Err("redesign node not started")` surfaced to the UI.
- `send_dm` errors (unknown peer, all paths failed) → `Err(<message>)` shown in the composer.
- Two app instances on one host share the redesign discovery UDP port via `SO_REUSEADDR`/`SO_REUSEPORT`; per-user `<app-data>/redesign/<user_id>/` dirs prevent the `messages.log`/`sent.log` collision.
- All node-layer paths are already fail-soft (a bad packet/peer/event never crashes the node).

## 5. Testing
- **Rust unit/integration (the reliable layer):** `RedesignRuntime::start` over a `tempfile` app-data dir opens a node and exposes a working `user_id`/roster; the command wrappers are thin pass-throughs over the `Node` API (already rig-proven for send/receive/history/offline). Reuse the node test patterns; CPU-throttled.
- **Frontend:** Vitest component test of `RedesignChatView.vue` against a mocked `api.redesign` + a mocked event bus (renders peers, sends, appends an inbound event, shows history) — if the project's Vitest setup supports it; otherwise a thin render/smoke test.
- **Manual end-to-end (documented, the true acceptance):** build the app; run two instances (two OS users or two data dirs) on one LAN; log in as two users; from `/redesign` send a DM and confirm it appears live on the other and in `/history` after a relaunch. A short doc note records the steps. (Automated Tauri+Vue e2e is out of scope.)
- **CI:** existing gates carry over (clippy `-D warnings`, fmt, tests, frontend lint), CPU-throttled.

## 6. Module boundaries & dependencies
- `node::runtime` depends on `node` (Node), `discovery`, `identity::keystore`, `tokio` — and Tauri only via the injected `emit` closure (so the runtime itself stays Tauri-agnostic and unit-testable).
- `redesign_commands` depends on `node::runtime` + `tauri`.
- The Vue route depends only on the four commands + the one event; it imports nothing from the legacy store/components.

## 7. Decomposition into plans
Two implementation plans, each independently testable:
1. **Backend** — `node::runtime` (`RedesignRuntime` + `start`/Drop + the shared discovery-socket helper), `RedesignState` + the four commands + the `redesign-dm-received` event, and the login/logout wiring. Rust-testable.
2. **Frontend** — the `api.redesign` wrapper, `RedesignChatView.vue`, and the route + nav registration. Component/smoke test + the documented manual e2e.

## 8. Accepted limitations (this slice)
- A dedicated `/redesign` route, not merged into the main chat UI (legacy chat untouched).
- Peers shown by `user_id`/name only (no contact aliasing/verification UI).
- DMs only — channels/groups are the next slice.
- Offline delivery works only if a post office runs on the LAN (capability present; the app does not yet elect/run one in-GUI).
- The redesign and legacy stacks coexist (two discovery broadcasts on different ports); unifying them is later work.
- End-to-end verification of the GUI path is manual; the node core beneath it is automatically rig-proven.
