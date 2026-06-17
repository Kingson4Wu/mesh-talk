# Tauri UI Migration — Frontend Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a self-contained Vue 3 `/redesign` route that exercises the redesign node over the four `redesign_*` IPC commands + the `redesign-dm-received` event: a peer list, a chat window with history, a send box, and live inbound DMs — leaving the existing chat UI untouched.

**Architecture:** A new `api.redesign` namespace wraps the four `invoke(...)` calls; a new `/redesign` route renders `RedesignChatView.vue` (auth-guarded, mirroring the existing view patterns); a nav button in the existing chat sidebar links to it; the view subscribes to `redesign-dm-received` for live inbound. No existing component's behavior changes (only an added route, an added api namespace, and an added nav button).

**Tech Stack:** Vue 3 (`<script setup>`) + Vue Router (hash history) + Pinia + Vite 6; `@tauri-apps/api` v2 (`invoke` from `/core`, `listen` from `/event`).

---

## Background the implementer needs

**This is Plan 2 (of 2) of the "Tauri UI migration" slice** (spec: `docs/superpowers/specs/2026-06-17-tauri-ui-migration-design.md`). Plan 1 (committed) added the backend: managed `RedesignState`, the four commands, the event, login/logout wiring. This plan is the Vue surface.

**Backend IPC contract (already shipped):**
- `redesign_my_id() -> String` (the node's user_id).
- `redesign_list_peers() -> [{ user_id, name, addr, post_office }]`.
- `redesign_send_dm({ recipient, text }) -> ()` (errors as a string).
- `redesign_history({ peer, limit }) -> [{ from_me, who, text, wall_clock }]`.
- Event `redesign-dm-received`, payload `{ from, from_name, text }`.
- Commands return `Err("redesign node not started")` until login finishes opening the node (the view tolerates this).

**Frontend facts (verified):**
- `frontend/src/services/api.js` imports `import { invoke } from "@tauri-apps/api/core";` and exports `export const API = { auth, node, contacts, messages, files, network };` (each a const object of arrow fns calling `invoke("cmd", {...})`). Tauri v2 maps JS arg keys to the Rust params (our params `recipient`/`text`/`peer`/`limit` are single words → keys match directly).
- `frontend/src/router/index.js` exports a router with `routes` (`/` → `ChatView` `meta:{requiresAuth:true}`, `/login` → `LoginView`, catch-all → `/`). The guard in `frontend/src/main.js` redirects to `login` when `to.meta.requiresAuth && !isAuthenticated`.
- Events: `import { listen } from "@tauri-apps/api/event";` → `const unlisten = await listen("name", (ev) => { const p = ev.payload ?? {}; ... });` and cleanup calls `unlisten()`.
- Auth in a view: `import { useAppStore } from "../../stores/appStore"; const store = useAppStore();` then `store.isAuthenticated` (bool) and `store.user` (`{ name, user_id, address }`). Views guard in `onMounted`: `if (!store.isAuthenticated) { router.replace({ name: "login" }); return; }`.
- Dark theme palette used in scoped styles: online green `#4ade80`, offline/slate `#94a3b8`, accent blue `rgba(59,130,246,…)`, bg `rgba(15,23,42,…)`, text `rgba(226,232,240,…)`, border `rgba(148,163,184,…)`.

**Verification discipline:** the frontend compiles via Vite; the pre-commit hook also runs ESLint + the frontend build. Build the frontend explicitly:
```bash
cd /Users/kingsonwu/programming/rust-src/mesh-talk/frontend && npm run build
```
(If `node_modules` is missing, run `npm install` first.) There is no automated Tauri+Vue e2e; the true acceptance is the documented manual two-instance run (Task 2). Do NOT run a CPU-heavy `cargo` build here — this plan is frontend-only.

**Conventions:** match the existing `.vue` view style (`<script setup>`, `<style scoped>`); keep the route self-contained (import nothing from the legacy store except `useAppStore` for the auth check).

---

### Task 1: API namespace + route + nav link + view skeleton

**Files:**
- Modify: `frontend/src/services/api.js`
- Modify: `frontend/src/router/index.js`
- Create: `frontend/src/views/redesign/RedesignChatView.vue`
- Modify: `frontend/src/views/chat/ChatView.vue` (add one nav button)

- [ ] **Step 1: Add the `redesign` API namespace (`frontend/src/services/api.js`)**

Add this const object next to the other `*API` consts:
```javascript
// Redesign node API (the new serverless E2E DM stack)
const redesignAPI = {
  myId: () => invoke("redesign_my_id"),
  listPeers: () => invoke("redesign_list_peers"),
  sendDm: (recipient, text) => invoke("redesign_send_dm", { recipient, text }),
  history: (peer, limit) => invoke("redesign_history", { peer, limit }),
};
```
and add `redesign: redesignAPI,` to the exported `API` object:
```javascript
export const API = {
  auth: authAPI,
  node: nodeAPI,
  contacts: contactsAPI,
  messages: messagesAPI,
  files: filesAPI,
  network: networkAPI,
  redesign: redesignAPI,
};
```
(Match the file's real namespace list; just append `redesign`.)

- [ ] **Step 2: Register the `/redesign` route (`frontend/src/router/index.js`)**

Add the import at the top:
```javascript
import RedesignChatView from "../views/redesign/RedesignChatView.vue";
```
and a route entry BEFORE the catch-all (`/:pathMatch(.*)*`):
```javascript
  {
    path: "/redesign",
    name: "redesign",
    component: RedesignChatView,
    meta: { requiresAuth: true },
  },
```

- [ ] **Step 3: Create the skeleton `frontend/src/views/redesign/RedesignChatView.vue`**

A minimal version that proves the route + IPC plumbing build and render. (Task 2 replaces the body with the full chat.)
```vue
<template>
  <div class="redesign">
    <header class="rd-header">
      <h2>Redesign chat <span class="beta">beta</span></h2>
      <div class="me">you: <code>{{ myId || "(starting…)" }}</code></div>
      <router-link class="back" :to="{ name: 'chat' }">← Back</router-link>
    </header>
    <p v-if="error" class="error">{{ error }}</p>
    <p>Peers discovered: {{ peers.length }}</p>
  </div>
</template>

<script setup>
import { ref, onMounted } from "vue";
import { useRouter } from "vue-router";
import { useAppStore } from "../../stores/appStore";
import { API } from "../../services/api";

const router = useRouter();
const store = useAppStore();
const myId = ref("");
const peers = ref([]);
const error = ref("");

onMounted(async () => {
  if (!store.isAuthenticated) {
    router.replace({ name: "login" });
    return;
  }
  try {
    myId.value = await API.redesign.myId();
  } catch (e) {
    error.value = "Redesign node not started yet.";
  }
  try {
    peers.value = await API.redesign.listPeers();
  } catch (e) {
    /* node may still be starting */
  }
});
</script>

<style scoped>
.redesign {
  height: 100vh;
  padding: 16px;
  color: rgba(226, 232, 240, 1);
  background: rgba(15, 23, 42, 1);
}
.rd-header {
  display: flex;
  align-items: center;
  gap: 16px;
}
.beta {
  font-size: 11px;
  color: #4ade80;
  border: 1px solid #4ade80;
  border-radius: 6px;
  padding: 1px 6px;
}
.me code {
  color: rgba(148, 163, 184, 1);
}
.back {
  margin-left: auto;
  color: rgba(59, 130, 246, 1);
  text-decoration: none;
}
.error {
  color: #f87171;
}
</style>
```

- [ ] **Step 4: Add a nav button to the chat sidebar (`frontend/src/views/chat/ChatView.vue`)**

Find the existing logout button in the sidebar (there is a `logout`/`Logout` button). Next to it, add a button that navigates to the redesign route. In the `<template>`, near the logout button:
```vue
        <button class="redesign-nav-button" @click="goToRedesign">Redesign (beta)</button>
```
In the `<script setup>` (where `router` is already available via `useRouter()`), add:
```javascript
function goToRedesign() {
  router.push({ name: "redesign" });
}
```
In `<style scoped>`, add a minimal style mirroring the logout button:
```css
.redesign-nav-button {
  margin-top: 8px;
  padding: 6px 12px;
  border: 1px solid #4ade80;
  border-radius: 8px;
  background: transparent;
  color: #4ade80;
  cursor: pointer;
}
```
(Match the existing button placement/markup in ChatView; only ADD this button + handler + style — change nothing else.)

- [ ] **Step 5: Build the frontend**
```bash
cd /Users/kingsonwu/programming/rust-src/mesh-talk/frontend && npm run build
```
Expected: the Vite build succeeds (the new route + view + api compile; no template/script errors). If `node_modules` is missing, run `npm install` first.

- [ ] **Step 6: Commit**
```bash
cd /Users/kingsonwu/programming/rust-src/mesh-talk
git add frontend/src/services/api.js frontend/src/router/index.js frontend/src/views/redesign/RedesignChatView.vue frontend/src/views/chat/ChatView.vue
git commit -m "feat(ui): redesign route skeleton + api.redesign namespace + nav link"
```
(Pre-commit hook runs ESLint + the build — let it run; fix any lint error it reports.)

---

### Task 2: Full redesign chat view (peers, history, send, live inbound)

**Files:**
- Modify: `frontend/src/views/redesign/RedesignChatView.vue` (replace the skeleton with the full view)
- Create: `docs/redesign-ui-manual-test.md` (the documented manual e2e)

- [ ] **Step 1: Replace `RedesignChatView.vue` with the full view**
```vue
<template>
  <div class="redesign">
    <header class="rd-header">
      <h2>Redesign chat <span class="beta">beta</span></h2>
      <div class="me">you: <code>{{ myId || "(starting…)" }}</code></div>
      <router-link class="back" :to="{ name: 'chat' }">← Back</router-link>
    </header>

    <div class="rd-body">
      <aside class="peers">
        <div class="peers-head">
          <span>Peers ({{ peers.length }})</span>
          <button class="icon" title="Refresh" @click="refreshPeers">⟳</button>
        </div>
        <ul>
          <li
            v-for="p in peers"
            :key="p.user_id"
            :class="{ active: activePeer && p.user_id === activePeer.user_id }"
            @click="selectPeer(p)"
          >
            <span class="name">{{ p.name || "(unnamed)" }}</span>
            <span class="uid">{{ p.user_id.slice(0, 8) }}</span>
            <span v-if="p.post_office" class="po" title="post office">PO</span>
            <span v-if="unread[p.user_id]" class="badge">{{ unread[p.user_id] }}</span>
          </li>
          <li v-if="!peers.length" class="empty">No peers discovered yet…</li>
        </ul>
      </aside>

      <section v-if="activePeer" class="chat">
        <div ref="msgList" class="messages">
          <div
            v-for="(m, i) in messages"
            :key="i"
            class="msg"
            :class="{ mine: m.from_me }"
          >
            <span class="who">{{ m.from_me ? "you" : m.who }}</span>
            <span class="text">{{ m.text }}</span>
          </div>
          <div v-if="!messages.length" class="empty">No messages yet.</div>
        </div>
        <form class="composer" @submit.prevent="send">
          <input
            v-model="draft"
            :placeholder="`Message ${activePeer.name || activePeer.user_id.slice(0, 8)}…`"
          />
          <button type="submit" :disabled="!draft.trim()">Send</button>
        </form>
        <p v-if="error" class="error">{{ error }}</p>
      </section>

      <section v-else class="chat empty-chat">
        <p>Select a peer to start chatting.</p>
        <p v-if="error" class="error">{{ error }}</p>
      </section>
    </div>
  </div>
</template>

<script setup>
import { ref, reactive, onMounted, onBeforeUnmount, nextTick } from "vue";
import { useRouter } from "vue-router";
import { useAppStore } from "../../stores/appStore";
import { API } from "../../services/api";
import { listen } from "@tauri-apps/api/event";

const router = useRouter();
const store = useAppStore();

const myId = ref("");
const peers = ref([]);
const activePeer = ref(null);
const messages = ref([]);
const draft = ref("");
const error = ref("");
const unread = reactive({});
const msgList = ref(null);

let refreshTimer = null;
let unlisten = null;

async function refreshPeers() {
  try {
    peers.value = await API.redesign.listPeers();
  } catch (e) {
    // node may still be starting; leave the list as-is
  }
}

async function selectPeer(p) {
  activePeer.value = p;
  delete unread[p.user_id];
  await loadHistory();
}

async function loadHistory() {
  if (!activePeer.value) return;
  error.value = "";
  try {
    messages.value = await API.redesign.history(activePeer.value.user_id, 100);
    await scrollDown();
  } catch (e) {
    error.value = String(e);
  }
}

async function send() {
  const text = draft.value.trim();
  if (!text || !activePeer.value) return;
  error.value = "";
  try {
    await API.redesign.sendDm(activePeer.value.user_id, text);
    // We get no inbound echo for our own message, so append optimistically.
    messages.value.push({ from_me: true, who: "you", text, wall_clock: Date.now() });
    draft.value = "";
    await scrollDown();
  } catch (e) {
    error.value = String(e);
  }
}

function onInbound(payload) {
  const from = payload.from;
  if (activePeer.value && from === activePeer.value.user_id) {
    messages.value.push({
      from_me: false,
      who: payload.from_name,
      text: payload.text,
      wall_clock: Date.now(),
    });
    scrollDown();
  } else if (from) {
    unread[from] = (unread[from] || 0) + 1;
  }
}

async function scrollDown() {
  await nextTick();
  if (msgList.value) {
    msgList.value.scrollTop = msgList.value.scrollHeight;
  }
}

onMounted(async () => {
  if (!store.isAuthenticated) {
    router.replace({ name: "login" });
    return;
  }
  try {
    myId.value = await API.redesign.myId();
  } catch (e) {
    error.value = "Redesign node not started yet — give it a moment after login.";
  }
  await refreshPeers();
  refreshTimer = setInterval(refreshPeers, 3000);
  unlisten = await listen("redesign-dm-received", (ev) => onInbound(ev.payload ?? {}));
});

onBeforeUnmount(() => {
  if (refreshTimer) clearInterval(refreshTimer);
  if (typeof unlisten === "function") unlisten();
});
</script>

<style scoped>
.redesign {
  display: flex;
  flex-direction: column;
  height: 100vh;
  color: rgba(226, 232, 240, 1);
  background: rgba(15, 23, 42, 1);
}
.rd-header {
  display: flex;
  align-items: center;
  gap: 16px;
  padding: 12px 16px;
  border-bottom: 1px solid rgba(148, 163, 184, 0.25);
}
.beta {
  font-size: 11px;
  color: #4ade80;
  border: 1px solid #4ade80;
  border-radius: 6px;
  padding: 1px 6px;
}
.me code {
  color: rgba(148, 163, 184, 1);
}
.back {
  margin-left: auto;
  color: rgba(59, 130, 246, 1);
  text-decoration: none;
}
.rd-body {
  display: flex;
  flex: 1;
  min-height: 0;
}
.peers {
  width: 240px;
  border-right: 1px solid rgba(148, 163, 184, 0.25);
  overflow-y: auto;
}
.peers-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 10px 12px;
  color: rgba(148, 163, 184, 1);
  font-size: 13px;
}
.peers .icon {
  background: transparent;
  border: none;
  color: rgba(148, 163, 184, 1);
  cursor: pointer;
}
.peers ul {
  list-style: none;
  margin: 0;
  padding: 0;
}
.peers li {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 8px 12px;
  cursor: pointer;
}
.peers li.active {
  background: rgba(59, 130, 246, 0.18);
}
.peers li .name {
  flex: 1;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.peers li .uid {
  font-family: monospace;
  font-size: 11px;
  color: rgba(148, 163, 184, 1);
}
.peers li .po {
  font-size: 10px;
  color: #fbbf24;
  border: 1px solid #fbbf24;
  border-radius: 4px;
  padding: 0 4px;
}
.peers li .badge {
  background: #4ade80;
  color: #0f172a;
  border-radius: 9px;
  font-size: 11px;
  padding: 0 6px;
}
.empty {
  color: rgba(148, 163, 184, 0.8);
  padding: 12px;
  font-size: 13px;
}
.chat {
  flex: 1;
  display: flex;
  flex-direction: column;
  min-width: 0;
}
.empty-chat {
  align-items: center;
  justify-content: center;
  color: rgba(148, 163, 184, 0.8);
}
.messages {
  flex: 1;
  overflow-y: auto;
  padding: 16px;
  display: flex;
  flex-direction: column;
  gap: 6px;
}
.msg {
  display: flex;
  flex-direction: column;
  max-width: 70%;
  padding: 6px 10px;
  border-radius: 10px;
  background: rgba(148, 163, 184, 0.12);
}
.msg.mine {
  align-self: flex-end;
  background: rgba(59, 130, 246, 0.25);
}
.msg .who {
  font-size: 11px;
  color: rgba(148, 163, 184, 1);
}
.composer {
  display: flex;
  gap: 8px;
  padding: 12px 16px;
  border-top: 1px solid rgba(148, 163, 184, 0.25);
}
.composer input {
  flex: 1;
  padding: 8px 10px;
  border-radius: 8px;
  border: 1px solid rgba(148, 163, 184, 0.3);
  background: rgba(15, 23, 42, 1);
  color: rgba(226, 232, 240, 1);
}
.composer button {
  padding: 8px 16px;
  border-radius: 8px;
  border: none;
  background: #4ade80;
  color: #0f172a;
  cursor: pointer;
}
.composer button:disabled {
  opacity: 0.5;
  cursor: default;
}
.error {
  color: #f87171;
  padding: 0 16px 8px;
}
</style>
```

- [ ] **Step 2: Write the manual-test doc `docs/redesign-ui-manual-test.md`**
```markdown
# Redesign UI — manual two-instance test

The `/redesign` route exercises the redesign node end-to-end. There is no
automated Tauri+Vue e2e; this is the manual acceptance check.

## Setup (two instances on one machine)
Run two app instances with separate HOME dirs so their per-account data
(`~/.mesh-talk/redesign/<account>/`) and legacy data don't collide:

```bash
# Terminal 1
HOME=/tmp/mesh-a npm run tauri dev    # (or the project's run command)
# Terminal 2
HOME=/tmp/mesh-b npm run tauri dev
```

## Steps
1. In each instance, register/log in as a different user (e.g. `alice` / `bob`).
   Wait a few seconds after login for the redesign node to finish opening
   (it does a password-KDF keystore unlock).
2. In each, click **Redesign (beta)** in the chat sidebar → the `/redesign` route.
   The header shows `you: <32-hex-id>` once the node is up.
3. Within ~5s each should list the other under **Peers** (UDP discovery).
4. From Alice, select Bob, type a message, **Send**. It appears immediately on
   Alice (optimistic) and within a moment on Bob (live `redesign-dm-received`).
5. Send back from Bob → appears on Alice.
6. Restart Alice's instance, log in, open `/redesign`, select Bob → **history**
   shows the prior messages (persisted), proving durability.

## Expected
- Peers appear by user_id/name; a running post office (if any) shows a `PO` tag.
- Messages flow both ways live; history survives restart.
- If a command errors with "redesign node not started", the node is still
  opening — wait and retry.
```

- [ ] **Step 3: Build the frontend**
```bash
cd /Users/kingsonwu/programming/rust-src/mesh-talk/frontend && npm run build
```
Expected: the Vite build succeeds.

- [ ] **Step 4: Commit**
```bash
cd /Users/kingsonwu/programming/rust-src/mesh-talk
git add frontend/src/views/redesign/RedesignChatView.vue docs/redesign-ui-manual-test.md
git commit -m "feat(ui): full redesign chat view (peers/history/send/live inbound) + manual test doc"
```
(Pre-commit hook runs ESLint + the build — let it run; fix any lint error.)

---

## Notes for the reviewer

- **What this delivers:** a self-contained `/redesign` Vue route — peer list (polled every 3s), a chat window populated from `redesign_history`, a send box (`redesign_send_dm` with optimistic append since the sender gets no inbound echo), and live inbound via `listen('redesign-dm-received')` (badging non-active conversations). A sidebar button links to it; a Back link returns to the legacy chat. Nothing in the legacy UI changes behaviorally.
- **Verification:** the Vite build + the pre-commit ESLint are the automated gates; the true end-to-end acceptance is the documented manual two-instance run (`docs/redesign-ui-manual-test.md`). The backend beneath (Plan 1 + the node rigs) is automatically tested.
- **Accepted limitations (from the spec):** dedicated route (not merged into the main chat); peers by user_id/name only; the redesign node may take a moment to start after login (the view tolerates "redesign node not started"); DMs only (channels next).
- **Reviewer checks:** confirm the ESLint passes (the project enforces it in pre-commit); confirm the route name `redesign` is unique; confirm `invoke` arg keys match the Rust params (`recipient`/`text`/`peer`/`limit`).
