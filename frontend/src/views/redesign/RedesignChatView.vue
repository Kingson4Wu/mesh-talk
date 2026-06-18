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

        <div class="peers-head channels-head">
          <span>Channels ({{ channels.length }})</span>
          <button class="icon" title="New channel" @click="showCreate = !showCreate">+</button>
        </div>

        <div v-if="showCreate" class="create-panel">
          <input
            v-model="newChannelName"
            class="create-input"
            placeholder="channel name"
          />
          <div class="create-members">
            <label v-for="p in peers" :key="p.user_id" class="member-row">
              <input type="checkbox" v-model="selectedMembers[p.user_id]" />
              {{ p.name || p.user_id.slice(0, 8) }}
            </label>
            <span v-if="!peers.length" class="empty">No peers to add.</span>
          </div>
          <div class="create-actions">
            <button
              class="create-btn"
              :disabled="!newChannelName.trim()"
              @click="createChannel"
            >Create</button>
            <button class="cancel-btn" @click="showCreate = false">Cancel</button>
          </div>
        </div>

        <ul>
          <li
            v-for="c in channels"
            :key="c.channel_id"
            :class="{ active: activeChannel && c.channel_id === activeChannel.channel_id }"
            @click="selectChannel(c)"
          >
            <span class="ch-hash">#</span>
            <span class="name">{{ c.name }}</span>
            <span class="uid">{{ c.member_count }}m</span>
            <span v-if="channelUnread[c.channel_id]" class="badge">{{ channelUnread[c.channel_id] }}</span>
          </li>
          <li v-if="!channels.length" class="empty">No channels yet.</li>
        </ul>
      </aside>

      <section v-if="activePeer || activeChannel" class="chat">
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
            :placeholder="activeChannel
              ? `Message #${activeChannel.name}…`
              : `Message ${activePeer.name || activePeer.user_id.slice(0, 8)}…`"
          />
          <button type="submit" :disabled="!draft.trim()">Send</button>
        </form>
        <p v-if="error" class="error">{{ error }}</p>
      </section>

      <section v-else class="chat empty-chat">
        <p>Select a peer or channel to start chatting.</p>
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

const channels = ref([]);
const activeChannel = ref(null);
const channelUnread = reactive({});
const showCreate = ref(false);
const newChannelName = ref("");
const selectedMembers = reactive({});

let refreshTimer = null;
let unlisten = null;
let unlistenChannel = null;

async function refreshPeers() {
  try {
    peers.value = await API.redesign.listPeers();
  } catch (_e) {
    // node may still be starting; leave the list as-is
  }
}

async function refreshChannels() {
  try {
    channels.value = await API.redesign.listChannels();
  } catch (_e) {
    // node may still be starting; leave the list as-is
  }
}

async function selectPeer(p) {
  activeChannel.value = null;
  activePeer.value = p;
  delete unread[p.user_id];
  await loadHistory();
}

async function loadHistory() {
  const target = activePeer.value;
  if (!target) return;
  error.value = "";
  try {
    const items = await API.redesign.history(target.user_id, 100);
    // Bail if the user switched peers while this history was loading, so we
    // never render one peer's history under another's header.
    if (activePeer.value?.user_id !== target.user_id) return;
    messages.value = items;
    await scrollDown();
  } catch (e) {
    error.value = String(e);
  }
}

async function selectChannel(c) {
  activePeer.value = null;
  activeChannel.value = c;
  delete channelUnread[c.channel_id];
  await loadChannelHistory();
}

async function loadChannelHistory() {
  const target = activeChannel.value;
  if (!target) return;
  error.value = "";
  try {
    const items = await API.redesign.channelHistory(target.channel_id, 100);
    if (activeChannel.value?.channel_id !== target.channel_id) return;
    messages.value = items;
    await scrollDown();
  } catch (e) {
    error.value = String(e);
  }
}

async function send() {
  const text = draft.value.trim();
  if (!text) return;
  if (!activePeer.value && !activeChannel.value) return;
  error.value = "";
  try {
    if (activeChannel.value) {
      await API.redesign.sendChannelMessage(activeChannel.value.channel_id, text);
    } else {
      await API.redesign.sendDm(activePeer.value.user_id, text);
    }
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
    void scrollDown();
  } else if (from) {
    unread[from] = (unread[from] || 0) + 1;
  }
}

function onChannelInbound(payload) {
  if (activeChannel.value && payload.channel_id === activeChannel.value.channel_id) {
    messages.value.push({
      from_me: false,
      who: payload.from,
      text: payload.text,
      wall_clock: Date.now(),
    });
    void scrollDown();
  } else {
    channelUnread[payload.channel_id] = (channelUnread[payload.channel_id] || 0) + 1;
  }
  // Refresh so a brand-new channel (created by a peer) appears in the list.
  refreshChannels();
}

async function createChannel() {
  const ids = Object.keys(selectedMembers).filter((k) => selectedMembers[k]);
  const name = newChannelName.value.trim();
  if (!name) return;
  try {
    const id = await API.redesign.createChannel(name, ids);
    newChannelName.value = "";
    Object.keys(selectedMembers).forEach((k) => delete selectedMembers[k]);
    showCreate.value = false;
    await refreshChannels();
    const created = channels.value.find((c) => c.channel_id === id);
    if (created) await selectChannel(created);
  } catch (e) {
    error.value = String(e);
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
  } catch (_e) {
    error.value = "Redesign node not started yet — give it a moment after login.";
  }
  await refreshPeers();
  await refreshChannels();
  refreshTimer = setInterval(() => { refreshPeers(); refreshChannels(); }, 3000);
  unlisten = await listen("redesign-dm-received", (ev) => onInbound(ev.payload ?? {}));
  unlistenChannel = await listen("redesign-channel-message", (ev) => onChannelInbound(ev.payload ?? {}));
});

onBeforeUnmount(() => {
  if (refreshTimer) clearInterval(refreshTimer);
  if (typeof unlisten === "function") unlisten();
  if (typeof unlistenChannel === "function") unlistenChannel();
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
.channels-head {
  border-top: 1px solid rgba(148, 163, 184, 0.15);
  margin-top: 4px;
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
.ch-hash {
  color: rgba(148, 163, 184, 0.6);
  font-size: 13px;
}
.empty {
  color: rgba(148, 163, 184, 0.8);
  padding: 12px;
  font-size: 13px;
}
.create-panel {
  padding: 8px 12px;
  display: flex;
  flex-direction: column;
  gap: 6px;
  border-bottom: 1px solid rgba(148, 163, 184, 0.15);
}
.create-input {
  padding: 6px 8px;
  border-radius: 6px;
  border: 1px solid rgba(148, 163, 184, 0.3);
  background: rgba(15, 23, 42, 1);
  color: rgba(226, 232, 240, 1);
  font-size: 13px;
}
.create-members {
  max-height: 120px;
  overflow-y: auto;
  display: flex;
  flex-direction: column;
  gap: 4px;
}
.member-row {
  display: flex;
  align-items: center;
  gap: 6px;
  font-size: 13px;
  color: rgba(226, 232, 240, 1);
  cursor: pointer;
}
.create-actions {
  display: flex;
  gap: 6px;
}
.create-btn {
  flex: 1;
  padding: 5px 0;
  border-radius: 6px;
  border: none;
  background: #4ade80;
  color: #0f172a;
  cursor: pointer;
  font-size: 13px;
}
.create-btn:disabled {
  opacity: 0.5;
  cursor: default;
}
.cancel-btn {
  flex: 1;
  padding: 5px 0;
  border-radius: 6px;
  border: 1px solid rgba(148, 163, 184, 0.3);
  background: transparent;
  color: rgba(148, 163, 184, 1);
  cursor: pointer;
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
