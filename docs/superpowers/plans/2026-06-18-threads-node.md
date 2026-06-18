# Threads / Reply-To: Message Body + Node Implementation Plan (Threads Plan 1)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Carry an optional `reply_to` on chat messages via a backward-compatible `MessageBody` envelope, surfaced on history + live messages — validated by a reply round-tripping over loopback and a legacy raw message still opening.

**Architecture:** A `node::message::MessageBody { text, reply_to }` is magic-framed (`MTB1` + bincode) and wraps a message's plaintext only at `kind = Message` send/open boundaries. `send_dm`/`send_channel_message` keep their signatures (thin wrappers over `_reply` variants). The three message-open sites decode the envelope (raw-text fallback). `HistoryEntry`/`ReceivedDm`/`ReceivedChannelMessage` gain `reply_to`.

**Tech Stack:** Rust; `bincode` (fixint), the existing DM/channel seal paths. No new dependencies.

---

## Background the implementer needs

Spec: `docs/superpowers/specs/2026-06-18-threads-design.md`. The envelope applies ONLY to `kind = Message` events; `FileManifest`/`React` events seal their own structs and MUST NOT be touched. Verified current code:
```rust
// node/conversation.rs:
//   build_dm_event(sender, recipient, conv, seq, parents, lamport, wall_clock, text: &[u8]) -> Result<Event, DmError>  // seals `text`
//   open_dm_event(recipient, roster, event) -> Option<(UserId, String, Vec<u8>)>  // kind==Message only; dm::open -> plaintext
// node/node.rs:
//   send_dm: locks log, build_dm_event(.., text), append; sentlog.record(conv, seq, wall_clock, text); deliver+replicate; emit_new_messages
//   send_channel_message: state.seal_message(text); append(React? no -> Message); distribute_channel
//   emit_new_messages: for each new non-self Message event -> open_dm_event -> ReceivedDm { from, from_name, text }
//   history: received via open_dm_event (has event.id); sent via sentlog (SentEntry { seq, wall_clock, plaintext })
//   channel_history: Message events -> state.open_message(ciphertext) -> HistoryEntry
//   HistoryEntry { id: EventId, from_me, who, text: Vec<u8>, wall_clock }
//   ReceivedDm { from: UserId, from_name: String, text: Vec<u8> }   // derives Debug,Clone,PartialEq,Eq
// node/channel.rs:
//   ChannelBook::process Message arm: state.open_message(&event.ciphertext) -> text -> ReceivedChannelMessage { channel_id, channel_name, from, text }
//   ReceivedChannelMessage { channel_id, channel_name, from, text: Vec<u8> }
//   ChannelState::open_message(&[u8]) -> Option<Vec<u8>>  (epoch-framed; returns the sealed plaintext)
// EventId: [u8;32] newtype, new(), as_bytes(), Serialize/Deserialize/Copy/Eq.
```

**CPU/test discipline:** `nice -n 10`, one filter per run. `cargo test --lib node::message`/`node::node`/`node::channel`; `cargo build --lib --bins`; `cargo clippy --lib --bins -- -D warnings`; `cargo fmt`. Confirm branch line after committing.

---

### Task 1: `node::message::MessageBody` (pure, magic-framed, raw fallback)

**Files:** Create `src-tauri/src/node/message.rs`; modify `src-tauri/src/node/mod.rs`.

- [ ] **Step 1: write `node/message.rs`**
```rust
//! The chat-message envelope. A `kind = Message` event's plaintext is a framed
//! `MessageBody` (text + optional `reply_to`) rather than raw bytes, so a reply can
//! reference its parent. Backward-compatible: anything not starting with the magic
//! (or not decoding) is treated as a legacy raw-text message (`reply_to = None`).

use crate::eventlog::event::EventId;
use bincode::Options;
use serde::{Deserialize, Serialize};

/// Frames a structured body so it's distinguishable from legacy raw-text plaintext.
const MSG_MAGIC: &[u8] = b"MTB1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageBody {
    pub text: Vec<u8>,
    pub reply_to: Option<EventId>,
}

impl MessageBody {
    pub fn new(text: Vec<u8>, reply_to: Option<EventId>) -> Self {
        MessageBody { text, reply_to }
    }

    /// `MSG_MAGIC ‖ bincode(self)` — the plaintext that gets sealed.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(MSG_MAGIC.len() + 64);
        out.extend_from_slice(MSG_MAGIC);
        out.extend_from_slice(
            &bincode::DefaultOptions::new()
                .with_fixint_encoding()
                .serialize(self)
                .expect("message body serializes"),
        );
        out
    }

    /// Recover a body from sealed-then-opened plaintext. Structured if it starts with
    /// the magic and decodes; otherwise the bytes are a legacy raw message.
    pub fn decode(bytes: &[u8]) -> MessageBody {
        if let Some(rest) = bytes.strip_prefix(MSG_MAGIC) {
            if let Ok(body) = bincode::DefaultOptions::new()
                .with_fixint_encoding()
                .reject_trailing_bytes()
                .deserialize::<MessageBody>(rest)
            {
                return body;
            }
        }
        MessageBody { text: bytes.to_vec(), reply_to: None }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_with_and_without_reply() {
        let plain = MessageBody::new(b"hello".to_vec(), None);
        assert_eq!(MessageBody::decode(&plain.encode()), plain);
        let reply = MessageBody::new(b"yes!".to_vec(), Some(EventId::new([7u8; 32])));
        assert_eq!(MessageBody::decode(&reply.encode()), reply);
    }

    #[test]
    fn legacy_raw_text_decodes_as_textonly() {
        // A pre-envelope message (raw UTF-8, no magic) still opens.
        let body = MessageBody::decode(b"just raw text");
        assert_eq!(body.text, b"just raw text");
        assert_eq!(body.reply_to, None);
    }

    #[test]
    fn text_that_starts_like_magic_but_isnt_a_body_falls_back() {
        // Bytes beginning with the magic but not a valid body are treated as raw.
        let mut bytes = MSG_MAGIC.to_vec();
        bytes.extend_from_slice(&[0xFF, 0xFF]); // not a valid MessageBody encoding
        let body = MessageBody::decode(&bytes);
        assert_eq!(body.text, bytes);
        assert_eq!(body.reply_to, None);
    }
}
```

- [ ] **Step 2: register the module (`node/mod.rs`)**

Add `pub mod message;` + `pub use message::MessageBody;` (match the existing grouping).

- [ ] **Step 3: run, fmt, clippy, commit**
```bash
cd src-tauri && nice -n 10 cargo test --lib node::message -- --test-threads=2
cd src-tauri && nice -n 10 cargo build --lib && nice -n 10 cargo clippy --lib -- -D warnings 2>&1 | tail -20
cd src-tauri && nice -n 10 cargo fmt
cd /Users/kingsonwu/programming/rust-src/mesh-talk
git add src-tauri/src/node/message.rs src-tauri/src/node/mod.rs
git commit -m "feat(node): MessageBody envelope (text + reply_to) with raw-text fallback"
git status | head -1
```

---

### Task 2: thread reply_to through send/open + the structs

**Files:** Modify `src-tauri/src/node/node.rs`, `node/conversation.rs`, `node/channel.rs`. (Check `node/runtime.rs` + tests for `ReceivedDm`/`ReceivedChannelMessage` construction.)

- [ ] **Step 1: structs gain `reply_to`**

`ReceivedDm` (node.rs): add `pub reply_to: Option<EventId>,`.
`HistoryEntry` (node.rs): add `pub reply_to: Option<EventId>,`.
`ReceivedChannelMessage` (node/channel.rs): add `pub reply_to: Option<EventId>,`.
Add `use crate::node::message::MessageBody;` where needed (node.rs, conversation.rs, channel.rs).

- [ ] **Step 2: `open_dm_event` decodes the body (conversation.rs)**

Change `open_dm_event` to return `Option<(UserId, String, Vec<u8>, Option<EventId>)>`:
```rust
    let plaintext = dm::open(recipient, &peer.public.x25519_pub, &event.ciphertext).ok()?;
    let body = MessageBody::decode(&plaintext);
    Some((author_user_id, peer.name.clone(), body.text, body.reply_to))
```
Add `use crate::node::message::MessageBody;` to conversation.rs. Update conversation.rs's own tests that destructure the 3-tuple to the 4-tuple (`let (from, name, text, _reply) = ...`).

- [ ] **Step 3: send paths wrap the body (node.rs)**

Refactor `send_dm` → a `_reply` variant the original delegates to. The wrap happens before `build_dm_event`, and the WRAPPED bytes go to the sidecar:
```rust
    pub async fn send_dm(&self, recipient: &str, text: &[u8]) -> Result<(), NodeError> {
        self.send_dm_reply(recipient, text, None).await
    }

    /// Send a DM that replies to `reply_to` (or `None` for a normal message).
    pub async fn send_dm_reply(
        &self,
        recipient: &str,
        text: &[u8],
        reply_to: Option<EventId>,
    ) -> Result<(), NodeError> {
        // ... existing body, but with `wrapped` in place of `text` ...
        let wrapped = MessageBody::new(text.to_vec(), reply_to).encode();
        // build_dm_event(.., &wrapped) ; sentlog.record(conv, seq, wall_clock, wrapped.clone())
    }
```
Take the EXISTING `send_dm` body and: (a) compute `let wrapped = MessageBody::new(text.to_vec(), reply_to).encode();` up front; (b) pass `&wrapped` to `build_dm_event`; (c) pass `wrapped` (clone if needed) to `sentlog.record(...)` instead of `text`. Everything else (locking, distribution, emit) unchanged.

Same for channels:
```rust
    pub async fn send_channel_message(&self, channel: ConversationId, text: &[u8]) -> Result<(), NodeError> {
        self.send_channel_message_reply(channel, text, None).await
    }
    pub async fn send_channel_message_reply(
        &self,
        channel: ConversationId,
        text: &[u8],
        reply_to: Option<EventId>,
    ) -> Result<(), NodeError> {
        let wrapped = MessageBody::new(text.to_vec(), reply_to).encode();
        // ... existing send_channel_message body, sealing `&wrapped` instead of `text` ...
    }
```

- [ ] **Step 4: open sites decode + populate `reply_to`**

`emit_new_messages` (node.rs): `if let Some((from, from_name, text, reply_to)) = open_dm_event(...) { ReceivedDm { from, from_name, text, reply_to } }`.
`history` (node.rs):
- received: the `open_dm_event` 4-tuple gives `reply_to`; push `HistoryEntry { id: event.id, from_me: false, who: name, text, wall_clock: event.wall_clock, reply_to }`.
- sent: decode the sidecar plaintext — `let body = MessageBody::decode(&sent.plaintext); HistoryEntry { id: <seq-map>, from_me: true, who: "you", text: body.text, wall_clock: sent.wall_clock, reply_to: body.reply_to }`.
`channel_history` (node.rs): for each Message event, `let body = MessageBody::decode(&open_message_result); HistoryEntry { id: event.id, ..., text: body.text, reply_to: body.reply_to }`.
`ChannelBook::process` Message arm (node/channel.rs): `let body = MessageBody::decode(&plaintext); ReceivedChannelMessage { channel_id, channel_name, from, text: body.text, reply_to: body.reply_to }` (decode the `state.open_message` result).

- [ ] **Step 5: fix ripples**

`grep -rn "ReceivedDm {" "ReceivedChannelMessage {" "HistoryEntry {"` across `src-tauri/src` + `src-tauri/tests` — add `reply_to` to every literal construction. The runtime forwarder reads specific fields (`.from`/`.text`/etc.) and is unaffected by the new field; only LITERAL constructions + full-struct equality asserts need updating. Channel rig + DM rig assert field access — confirm they compile.

- [ ] **Step 6: loopback rig — a reply round-trips with its reply_to**

Add to `node/node.rs` `mod tests` (mirror the DM loopback rig; capture publics before move; both nodes serve so the reply flows back). After Alice sends "hi" and Bob receives it, Bob looks up the message id from `bob_node.dm_history(&alice_pub, 10)`, calls `bob_node.send_dm_reply(&alice_uid, b"hey back", Some(that_id))`, and Alice (a) receives a `ReceivedDm` with `reply_to == Some(that_id)` on her DM stream, and (b) her `dm_history(&bob_pub, 10)` shows the reply entry with `reply_to == Some(that_id)`. Also assert a NORMAL `send_dm` yields `reply_to == None`. Use the real `dm_history(&PublicIdentity, usize)` / `reactions`-style direct API (no pseudo-helpers).

- [ ] **Step 7: build, test, clippy, fmt, commit**
```bash
cd src-tauri && nice -n 10 cargo test --lib node::node -- --test-threads=2
cd src-tauri && nice -n 10 cargo test --lib node::channel -- --test-threads=2
cd src-tauri && nice -n 10 cargo test --lib node::conversation -- --test-threads=2
cd src-tauri && nice -n 10 cargo build --lib --bins && nice -n 10 cargo clippy --lib --bins -- -D warnings 2>&1 | tail -20
cd src-tauri && nice -n 10 cargo fmt
cd /Users/kingsonwu/programming/rust-src/mesh-talk
git add -A
git commit -m "feat(node): reply_to on messages — send_*_reply, body decode at open sites, history/streams carry it"
git status | head -1
```
Expected: the reply rig + ALL existing DM/channel/file/reaction rigs pass (legacy + new); clippy clean.

---

## Notes for the reviewer / next plan

- **Delivered:** reply-to on DM + channel messages via the `MessageBody` envelope, surfaced on `HistoryEntry`/`ReceivedDm`/`ReceivedChannelMessage`. Backward-compatible (legacy raw messages decode as `reply_to = None`). The envelope is confined to `kind = Message`; file/reaction events are untouched.
- **Reviewer checks:** the envelope is NOT applied to `FileManifest`/`React` sends/opens; the sidecar stores the wrapped bytes so sent replies carry `reply_to`; `open_dm_event`'s new 4-tuple is handled at both call sites; all existing rigs still pass (DM/channel/file/reaction); the magic-fallback can't misparse a normal message into a wrong reply.
- **Deferred to Threads Plan 2 (app+UI):** `reply_to` on the send commands + `HistoryItem` + the DM/channel received events; a reply button + composer reply-context + parent-snippet rendering in `RedesignChatView.vue`.
