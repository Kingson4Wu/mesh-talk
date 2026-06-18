# @mentions (frontend) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add @mentions to the `/redesign` chat: an autocomplete when typing `@`, highlighted `@name` tokens in rendered messages, and a "mentioned you" marker on received messages that mention the current user.

**Architecture:** Pure frontend — mentions live in the message text (no new event/command). The composer offers an `@` autocomplete from the already-loaded `peers` list; rendered messages segment their text to highlight `@tokens`; a received message is flagged when its text contains `@<my display name>` (from `store.user.name`) or `@all`/`@everyone`.

**Tech Stack:** Vue 3 `<script setup>` (only `frontend/src/views/redesign/RedesignChatView.vue`).

---

## Background the implementer needs

`RedesignChatView.vue` already has: `peers` (ref of `{ user_id, name }`), `activePeer`/`activeChannel`, `draft` (composer v-model), `messages` (items render `{ from_me, who, text|file, id }`), `myId` (fingerprint), and `import { useAppStore } from "../../stores/appStore"` with `const store = useAppStore()`. `store.user?.name` is the logged-in display name. The text of a message renders as `<span class="text">{{ m.text }}</span>` (file messages use a `.file-card` branch — leave those untouched). This task modifies ONLY `RedesignChatView.vue`.

**CPU/test discipline:** `cd frontend && npm run build` must succeed; run a `lint` script if `package.json` defines one. Confirm `git status | head -1` == `On branch feat/redesign-phase0` after committing.

---

### Task 1: mention autocomplete + highlight + "mentioned you"

**Files:** Modify `frontend/src/views/redesign/RedesignChatView.vue`.

- [ ] **Step 1: script — helpers + autocomplete state**

Add to `<script setup>`:
```js
const myName = computed(() => store.user?.name || "");

// --- @mention autocomplete ---
const mentionOpen = ref(false);
const mentionQuery = ref("");
const mentionCandidates = computed(() => {
  if (!mentionOpen.value) return [];
  const q = mentionQuery.value.toLowerCase();
  return peers.value
    .filter((p) => (p.name || p.user_id).toLowerCase().startsWith(q))
    .slice(0, 6);
});

// Called on composer input: detect a trailing "@word" being typed.
function onDraftInput() {
  const m = /@(\S*)$/.exec(draft.value);
  if (m) {
    mentionOpen.value = true;
    mentionQuery.value = m[1];
  } else {
    mentionOpen.value = false;
  }
}

function applyMention(peer) {
  const name = peer.name || peer.user_id.slice(0, 8);
  // Replace the trailing "@word" with "@name ".
  draft.value = draft.value.replace(/@(\S*)$/, `@${name} `);
  mentionOpen.value = false;
}

// --- rendering helpers ---
// Split text into segments, marking @mentions (a run of non-space chars after @).
function mentionParts(text) {
  const parts = [];
  const re = /@[^\s@]+/g;
  let last = 0;
  let m;
  while ((m = re.exec(text)) !== null) {
    if (m.index > last) parts.push({ text: text.slice(last, m.index), mention: false });
    parts.push({ text: m[0], mention: true });
    last = m.index + m[0].length;
  }
  if (last < text.length) parts.push({ text: text.slice(last), mention: false });
  return parts;
}

// True if a (received) message mentions the current user.
function mentionsMe(text) {
  if (!text) return false;
  const t = text.toLowerCase();
  const n = myName.value.toLowerCase();
  return (
    t.includes("@all") ||
    t.includes("@everyone") ||
    (n.length > 0 && t.includes("@" + n))
  );
}
```
Ensure `computed` and `ref` are imported (the file already imports from `vue` — add `computed` if missing).

- [ ] **Step 2: template — wire the composer input + autocomplete dropdown**

On the composer `<input v-model="draft" ...>`, add `@input="onDraftInput"` and `@keydown.esc="mentionOpen = false"`. Wrap the composer area so a dropdown can position above it. Add the dropdown (just above or below the input, styled as an overlay):
```html
<div v-if="mentionOpen && mentionCandidates.length" class="mention-pop">
  <button
    v-for="p in mentionCandidates"
    :key="p.user_id"
    type="button"
    class="mention-item"
    @click="applyMention(p)"
  >
    @{{ p.name || p.user_id.slice(0, 8) }}
  </button>
</div>
```
(Place it inside the `.composer` container with `position: relative` on the container so the pop can be `position: absolute`.)

- [ ] **Step 3: template — highlight tokens + "mentioned you" marker**

Replace the text render `<span class="text">{{ m.text }}</span>` with a segmented render, and add a mention marker on received messages:
```html
<span v-else class="text" :class="{ 'mentions-me': !m.from_me && mentionsMe(m.text) }">
  <template v-for="(seg, si) in mentionParts(m.text)" :key="si">
    <span v-if="seg.mention" class="mention">{{ seg.text }}</span>
    <template v-else>{{ seg.text }}</template>
  </template>
</span>
```
(Keep the `v-if="m.file"` file-card branch exactly as-is; this replaces only the `v-else` text branch. `m.text` is undefined for file messages, but the `v-else` only renders for non-file messages, so `mentionParts(m.text)` is safe — still, `mentionParts`/`mentionsMe` guard against a falsy `text`.)

- [ ] **Step 4: styles**

Add minimal scoped styles consistent with the dark theme:
```css
.mention { color: #93c5fd; font-weight: 600; }
.text.mentions-me { border-left: 3px solid #fbbf24; padding-left: 6px; }
.mention-pop {
  position: absolute;
  bottom: 100%;
  left: 16px;
  margin-bottom: 4px;
  background: rgba(15, 23, 42, 1);
  border: 1px solid rgba(148, 163, 184, 0.3);
  border-radius: 8px;
  overflow: hidden;
  z-index: 10;
}
.mention-item {
  display: block;
  width: 100%;
  text-align: left;
  padding: 6px 12px;
  background: transparent;
  border: none;
  color: rgba(226, 232, 240, 1);
  cursor: pointer;
}
.mention-item:hover { background: rgba(59, 130, 246, 0.18); }
```
Add `position: relative;` to the existing `.composer` rule so `.mention-pop` anchors to it.

- [ ] **Step 5: build + commit**
```bash
cd frontend && npm run build 2>&1 | tail -20
cd /Users/kingsonwu/programming/rust-src/mesh-talk
git add frontend/src/views/redesign/RedesignChatView.vue
git commit -m "feat(ui): @mentions — autocomplete, highlighted tokens, mentioned-you marker"
git status | head -1
```
Expected: `npm run build` succeeds; typing `@` shows peer suggestions, `@tokens` render highlighted, and a message containing `@<my name>` shows the amber marker.

---

## Notes for the reviewer

- **End-to-end:** typing `@` autocompletes from known peers; sent text carries `@name`; everyone sees `@name` highlighted; a recipient whose display name is mentioned (or `@all`/`@everyone`) sees an amber left-border marker.
- **Reviewer checks:** the file-card branch is untouched; `mentionParts`/`mentionsMe` guard a falsy `text`; the autocomplete only triggers on a trailing `@word`; `applyMention` replaces just that token; no backend/node changes.
- **Known MVP limitations (acceptable):** mentions are name-based (display names aren't globally unique on a LAN) and a single `@token` is a run of non-space chars (a display name with spaces highlights only its first word); no mention-triggered notification/badge beyond the in-message marker; autocomplete triggers at the end of the draft only. A structured mention model (user-id-backed) + notifications are deferred.
