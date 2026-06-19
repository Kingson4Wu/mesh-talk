# Threads / Reply-To (Phase 2) Design

**Status:** Derived from the master redesign spec (Phase 2 "threads"). Authored autonomously against the north-star. Scoped to **reply-to** (inline reply context), not full nested thread panes.

## Goal

Let a message reply to an earlier message; the reply renders with the parent's context ("↩ replying to …"). E2E-encrypted, works in DMs and channels, backward-compatible with existing messages.

## Core idea: a `MessageBody` envelope at the message boundary

Chat message plaintext becomes a framed envelope `MessageBody { text, reply_to: Option<EventId> }` instead of raw bytes. The envelope is applied ONLY when sending a `kind = Message` event and interpreted only when opening one — `FileManifest`/`React`/membership events seal their own structs and are untouched. Backward compatibility: the envelope is prefixed with a 4-byte magic (`MTB1`); on open, bytes starting with the magic that decode as a `MessageBody` are structured, anything else is treated as legacy raw text (`reply_to = None`). So old messages keep working and the change is confined to the message path.

### Where it applies

- **Send:** `send_dm`/`send_channel_message` wrap `text` (+ optional `reply_to`) into a `MessageBody`, then seal as today (DM sealed-box / channel group key). Existing zero-reply sends go through the same path with `reply_to = None`, so their signatures are preserved (a `_reply` variant carries the parent; the original delegates with `None`). The DM sent-plaintext sidecar stores the *wrapped* bytes, so a sender's own replies render their context too.
- **Open:** the `kind = Message` open sites — `open_dm_event` (DM history + live), `ChannelBook` message replay, `channel_history` — decode the `MessageBody` to recover `(text, reply_to)`. `open_message`/`dm::open` stay raw-returning; the kind-specific decode happens in the caller.

### Surfacing

`HistoryEntry`, `ReceivedDm`, `ReceivedChannelMessage` gain `reply_to: Option<EventId>`. Commands map it to hex; the `redesign-dm-received` / `redesign-channel-message` events + `HistoryItem` carry it. The UI: a "reply" affordance per message sets a pending parent; the composer shows "replying to <snippet>"; a sent/received reply renders the parent's author + snippet above it (resolved by matching `reply_to` to a message already in view).

## Scope (MVP) and deferrals

- **In:** reply-to on DM + channel messages; reply context rendering; backward-compatible envelope. 
- **Deferred:** nested/collapsible thread panes; reply counts/"N replies"; jumping to a parent out of view (MVP resolves the parent only if it's already loaded, else shows "replying to a message"); reply notifications.

## Decomposition

1. **Message body + node core:** `node::message::MessageBody` (magic-framed encode/decode + raw fallback); `send_dm_reply`/`send_channel_message_reply` (+ originals delegating); decode at the three open sites; `reply_to` on `HistoryEntry`/`ReceivedDm`/`ReceivedChannelMessage`; loopback rig (a reply round-trips with its `reply_to`). Backward-compat test (legacy raw message still opens).
2. **App + UI:** `reply_to` on the send commands + `HistoryItem` + the DM/channel received events; a reply button + composer reply-context + parent-snippet rendering in `RedesignChatView.vue`.
