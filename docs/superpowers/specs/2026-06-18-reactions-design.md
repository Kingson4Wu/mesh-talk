# Message Reactions (Phase 2) Design

**Status:** Derived from the master redesign spec (Phase 2 "reactions"; `EventKind::React = 3` reserved). Authored autonomously against the north-star.

## Goal

Let a user react to a message (a DM or channel message) with an emoji, and toggle it off. Reactions are E2E-encrypted, aggregate across members, and sync through the existing event log with no new transport or stream.

## Core idea: React events, aggregated on read

A reaction is a `React` event in the same conversation as its target message. Its (sealed) payload names the target message's `EventId` + an emoji + a remove flag. React events sync exactly like messages (the store ingests any signed event), so distribution + offline delivery come for free. Reactions are not pushed on a live stream; `reactions(conv)` aggregates the conversation's React events on demand (the UI reloads them on message activity / its existing poll).

### Crypto

The `React` payload (`ReactionPayload { target, emoji, remove }`) is sealed exactly like a message — DM sealed-box for a DM, the channel group key for a channel — so only members learn what was reacted and with what. The post office relays opaque React ciphertext.

### Aggregation

`reactions(conv)` opens each React event (DM: the author's X25519 from the roster; channel: the channel group key), decodes the payload, and folds them **in event order** into a map `(target EventId, emoji) → set<author user_id>`: a normal React adds the author, `remove = true` removes them. Empty sets are dropped. The result is a list of `ReactionView { target (hex), emoji, who: [user_id] }`; the UI derives counts and "did I react".

### Targeting + the sent-message id gap

To react, the UI needs the target message's `EventId`. So `HistoryEntry`/`HistoryItem` gain an `id` (hex):
- Channel messages + received DMs come from log events → `id = event.id`.
- **Sent DMs** are rendered from the plaintext sidecar (`SentLog`), which has the `seq` but not the event id. `history()` recovers the id by matching the sidecar entry's `seq` to this node's authored `Message` event in the conversation (built once per call as a `seq → EventId` map). So every rendered message has a stable id to react to and to map reactions onto.

### Why no new node stream / constructor change

React events are ingested by the normal store gate during sync (`serve_one`/drain need no changes), and reactions render on read, so unlike DMs/channels/files there is no `mpsc` sink and no `Node::open` signature change. The UI refreshes reactions on the same cadence it already refreshes messages.

## Scope (MVP) and deferrals

- **In:** react/unreact in DMs + channels; emoji + toggle; aggregated chips under each message; `id` on history. DMs and channels both.
- **Deferred:** a live `reaction-received` push (reactions appear on the next reload — frequent enough via message events + poll); reaction on files; a curated emoji palette beyond a small default set; @mentions + threads (separate slices).

## Decomposition

1. **Model + node** (`node::reaction` + `Node`): `ReactionPayload` (encode/decode), `aggregate` (fold), `react_dm`/`react_channel`, `reactions(conv)`, `HistoryEntry.id` (+ sent-DM seq→id mapping). Loopback rig: a node reacts to a peer's message; the peer's `reactions()` reflects it.
2. **App + UI**: IPC `redesign_react_dm`/`redesign_react_channel`/`redesign_reactions` + `HistoryItem.id` + a react button + reaction chips in `RedesignChatView.vue` (reload on send + on inbound message).
