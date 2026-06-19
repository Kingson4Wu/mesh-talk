# Account-Addressed Messaging + Fan-out + Self-Sync Implementation Plan (Multi-Device Plan 3)

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:subagent-driven-development. SECURITY-SENSITIVE (routing rides inside the sealed plaintext). Checkbox steps; full code inline.

**Goal:** Send a DM to an *account* (which may have several devices) by fanning out a per-device ratcheted copy to every known device of that account, and **self-syncing** a copy to the sender's own other devices — so all of a user's devices show the same conversation. Receive surfaces the message under an account-keyed conversation; history is keyed by account.

**Architecture:** The device-pair Double Ratchet, event log, and sync stay exactly as-is — they remain the *transport+crypto* layer (one ratchet endpoint per device pair). A thin **logical account layer** rides on top: the sealed DM plaintext is wrapped in a `DmEnvelope` carrying a `DmRoute { sender_account, recipient_account }`. The receiver unwraps the route, computes the **account conversation id** (hash of the sorted account-id pair), and files the message under it — deriving `from_me` from whether the route's sender is its own account. `send_to_account` enumerates the target account's devices (and the sender's own other devices) from the roster (`devices_of_account`, Plan 2) and reuses the existing per-device seal→append→deliver path for each. No `msg_id` is needed: each logical message is sealed once per destination device, so no device ever receives a duplicate; cross-delivery idempotency is already handled by the event-id `emitted` set.

**Staging note:** until device linking (Plan 4) makes a user's devices share one account, `devices_of_account(my_account)` returns only this device, so self-sync is a no-op and fan-out targets the peer's single device — i.e. Plan 3 behaves like today's 1:1 DM but routed through the account layer. Multi-device fan-out/self-sync is exercised now via integration tests that construct several nodes sharing an `account_id`.

**Tech Stack:** Rust; `sha2`, `bincode`, `serde`, `rand`. All already dependencies.

## Global Constraints

- **CPU/test discipline:** prefix cargo with `nice -n 10`; always `-- --test-threads=2`. Scope tests to the touched module/test name. Node integration tests are slow (per-node KDFs); run specific test names.
- **Branch:** `feat/redesign-phase0`.
- **No behavior regressions:** the device-addressed `send_dm`/`dm_history` path stays intact and is what the current UI uses; the account path is additive. The full pre-commit health gate (whole test suite) must pass on every commit.
- **Fail closed / backward compatible:** a sealed plaintext without the `DmEnvelope` magic is treated as a legacy device-addressed `MessageBody` (current behavior).

---

### Task 1: Pure foundations — account conversation id + DM routing envelope

**Files:**
- Modify: `src-tauri/src/node/conversation.rs` (add `account_conversation_id`)
- Create: `src-tauri/src/node/dm_envelope.rs`
- Modify: `src-tauri/src/node/mod.rs` (register `mod dm_envelope;` + re-export)

**Interfaces:**
- Produces: `account_conversation_id(a: &str, b: &str) -> ConversationId`; `DmRoute { sender_account: String, recipient_account: String }`; `DmEnvelope { route: DmRoute, body: Vec<u8> }` with `encode(&self) -> Vec<u8>` and `decode(bytes: &[u8]) -> Option<DmEnvelope>`.

- [ ] **Step 1: add `account_conversation_id` to `conversation.rs`**

Append to `src-tauri/src/node/conversation.rs` (after the existing `dm_conversation_id`):
```rust
/// Domain separator for account-conversation-id derivation.
const ACCOUNT_CONV_DOMAIN: &[u8] = b"mesh-talk-account-conversation-v1";

/// The deterministic conversation id for the logical 1:1 conversation between two
/// accounts — a hash of the SORTED pair of account ids, so both accounts compute the
/// same id. This keys account-level history; it is distinct from the per-device-pair
/// [`dm_conversation_id`] used for transport/crypto.
pub fn account_conversation_id(a: &str, b: &str) -> ConversationId {
    let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
    let mut hasher = Sha256::new();
    hasher.update(ACCOUNT_CONV_DOMAIN);
    hasher.update((lo.len() as u32).to_be_bytes());
    hasher.update(lo.as_bytes());
    hasher.update(hi.as_bytes());
    ConversationId::new(hasher.finalize().into())
}
```

Add tests to `conversation.rs`'s `mod tests`:
```rust
    #[test]
    fn account_conversation_id_is_symmetric_and_deterministic() {
        let id = account_conversation_id("alice", "bob");
        assert_eq!(id, account_conversation_id("bob", "alice"));
        assert_eq!(id, account_conversation_id("alice", "bob"));
    }

    #[test]
    fn account_conversation_id_is_unambiguous_across_boundary() {
        // Length-prefixing the low id prevents ("a","bc") colliding with ("ab","c").
        assert_ne!(
            account_conversation_id("a", "bc"),
            account_conversation_id("ab", "c")
        );
    }

    #[test]
    fn different_account_pairs_differ() {
        assert_ne!(
            account_conversation_id("alice", "bob"),
            account_conversation_id("alice", "carol")
        );
    }
```

- [ ] **Step 2: create `node/dm_envelope.rs`**

```rust
//! The DM routing envelope for account-addressed messaging (multi-device). A DM's
//! sealed plaintext is `DmEnvelope = MAGIC ‖ bincode({ route, body })`, where `body`
//! is the inner [`crate::node::message::MessageBody`] bytes and `route` names the
//! sender/recipient ACCOUNTS (not devices). The receiver uses the route to file the
//! message under the right account conversation and to decide `from_me` (true when the
//! route's sender is its own account — i.e. a self-synced copy of its own send).
//!
//! Backward compatible: a sealed plaintext that does not start with the magic (a
//! legacy device-addressed message) is handled by the caller as a raw `MessageBody`.

use bincode::Options;
use serde::{Deserialize, Serialize};

/// Frames the envelope so it is distinguishable from a legacy raw `MessageBody`.
const DM_ENV_MAGIC: &[u8] = b"MTDE1";

/// Logical routing for an account-addressed DM: which account sent it and which
/// account it is addressed to. Both are 32-hex `account_id`s.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DmRoute {
    pub sender_account: String,
    pub recipient_account: String,
}

/// The sealed-plaintext envelope: a route plus the inner `MessageBody` bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DmEnvelope {
    pub route: DmRoute,
    pub body: Vec<u8>,
}

impl DmEnvelope {
    pub fn new(sender_account: String, recipient_account: String, body: Vec<u8>) -> Self {
        DmEnvelope {
            route: DmRoute {
                sender_account,
                recipient_account,
            },
            body,
        }
    }

    /// `DM_ENV_MAGIC ‖ bincode(self)` — the plaintext that gets sealed.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(DM_ENV_MAGIC.len() + 64);
        out.extend_from_slice(DM_ENV_MAGIC);
        out.extend_from_slice(
            &bincode::DefaultOptions::new()
                .with_fixint_encoding()
                .serialize(self)
                .expect("dm envelope serializes"),
        );
        out
    }

    /// Recover an envelope from opened plaintext, or `None` if the bytes are not a
    /// framed envelope (a legacy device-addressed message — caller falls back).
    pub fn decode(bytes: &[u8]) -> Option<DmEnvelope> {
        let rest = bytes.strip_prefix(DM_ENV_MAGIC)?;
        bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .reject_trailing_bytes()
            .deserialize::<DmEnvelope>(rest)
            .ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        let env = DmEnvelope::new("alice".into(), "bob".into(), b"inner-body".to_vec());
        assert_eq!(DmEnvelope::decode(&env.encode()), Some(env));
    }

    #[test]
    fn legacy_plaintext_is_not_an_envelope() {
        assert_eq!(DmEnvelope::decode(b"just a MessageBody"), None);
        // Magic prefix but garbage body → not a valid envelope.
        let mut bytes = DM_ENV_MAGIC.to_vec();
        bytes.extend_from_slice(&[0xFF, 0xFF]);
        assert_eq!(DmEnvelope::decode(&bytes), None);
    }

    #[test]
    fn rejects_trailing_bytes() {
        let env = DmEnvelope::new("a".into(), "b".into(), b"x".to_vec());
        let mut bytes = env.encode();
        bytes.push(0xAB);
        assert_eq!(DmEnvelope::decode(&bytes), None);
    }
}
```

- [ ] **Step 3: register the module (`node/mod.rs`)**

Add `mod dm_envelope;` with the other `mod`s and re-export the types next to where `message`/`conversation` items are exported (mirror the existing `pub use` style):
```rust
mod dm_envelope;
pub use dm_envelope::{DmEnvelope, DmRoute};
```
Also ensure `account_conversation_id` is reachable where `dm_conversation_id` is used (it's `pub` in `conversation.rs`; if `conversation` items are re-exported via `pub use conversation::dm_conversation_id;`, add `account_conversation_id` to that re-export).

- [ ] **Step 4: test, clippy, fmt, commit**
```bash
cd src-tauri && nice -n 10 cargo test --lib node::conversation node::dm_envelope -- --test-threads=2
cd src-tauri && nice -n 10 cargo clippy --lib -- -D warnings 2>&1 | tail -20
cd src-tauri && nice -n 10 cargo fmt
cd /Users/kingsonwu/programming/rust-src/mesh-talk
git add src-tauri/src/node/conversation.rs src-tauri/src/node/dm_envelope.rs src-tauri/src/node/mod.rs docs/superpowers/plans/2026-06-19-account-addressed-fanout.md
git commit -m "feat(node): account conversation id + DM routing envelope (multi-device plan 3)"
```

---

### Task 2: Thread the account id into `Node` (no behavior change)

**Files:**
- Modify: `src-tauri/src/node/node.rs` (struct field, `open` → `open_with_account`, accessor)
- Modify: `src-tauri/src/node/runtime.rs` (call `open_with_account` with the real account id)
- Modify: `src-tauri/src/bin/mesh-talk-node.rs` (normal-node path)

**Interfaces:**
- Produces: `Node::open_with_account(identity, account_id: String, roster, incoming, channel_incoming, file_incoming, log_path, sent_path, password) -> Result<Arc<Self>, LogError>`; `Node::open(...)` unchanged in signature (delegates with `account_id = identity.public().user_id()`); `Node::account_id(&self) -> &str`.

- [ ] **Step 1: add the field + split the constructor (`node.rs`)**

Add to the `Node` struct (near `identity`):
```rust
    /// This node's cryptographic account id (cross-device handle). Defaults to the
    /// device's own user-id when opened via [`Node::open`]; the real per-account id is
    /// injected by [`Node::open_with_account`]. Account-addressed sends route by this.
    account_id: String,
```

Replace the current `pub fn open(...) -> Result<Arc<Self>, LogError> {` header and its body-start so that `open` delegates and the real body moves into `open_with_account`. Concretely, rename the existing `pub fn open` to `pub fn open_with_account` and insert `account_id: String,` as its 2nd parameter (right after `identity`), then set `account_id` in the constructed `Node { .. }` literal. Add a thin `open` above it:
```rust
    /// Open a node, defaulting its account id to the device's own user-id (single
    /// "account per device" — the pre-linking reality). Most call sites use this.
    pub fn open(
        identity: DeviceIdentity,
        roster: Arc<Mutex<Roster>>,
        incoming: mpsc::UnboundedSender<ReceivedDm>,
        channel_incoming: mpsc::UnboundedSender<ReceivedChannelMessage>,
        file_incoming: mpsc::UnboundedSender<ReceivedFile>,
        log_path: &Path,
        sent_path: &Path,
        password: &str,
    ) -> Result<Arc<Self>, LogError> {
        let account_id = identity.public().user_id();
        Self::open_with_account(
            identity,
            account_id,
            roster,
            incoming,
            channel_incoming,
            file_incoming,
            log_path,
            sent_path,
            password,
        )
    }
```
In the `Node { .. }` literal at the end of `open_with_account`, add `account_id,`.

Add the accessor (near `user_id`):
```rust
    /// This node's cryptographic account id.
    pub fn account_id(&self) -> &str {
        &self.account_id
    }
```

- [ ] **Step 2: runtime + bin pass the real account id**

In `src-tauri/src/node/runtime.rs`, change the `Node::open(` call (currently passing `identity` first) to `Node::open_with_account(` with `account.account_id()` as the 2nd argument:
```rust
        let node = Node::open_with_account(
            identity,
            account.account_id(),
            Arc::clone(&roster),
            incoming_tx,
            channel_tx,
            file_tx,
            &dir.join("messages.log"),
            &dir.join("sent.log"),
            password,
        )?;
```
(The `account` is already loaded earlier in `start` from Plan 2; `account_id` local is already derived — reuse `account.account_id()` or the existing `account_id` local. Note the existing `account_id` local already equals `account.account_id()`.)

In `src-tauri/src/bin/mesh-talk-node.rs` normal-node `main`, change `Node::open(identity, ...)` to `Node::open_with_account(identity, account.account_id(), ...)` (the `account` is already loaded from Plan 2).

- [ ] **Step 3: build, test, clippy, fmt, commit**
```bash
cd src-tauri && nice -n 10 cargo build --lib --bins 2>&1 | tail -15
cd src-tauri && nice -n 10 cargo test --lib node::runtime -- --test-threads=2
cd src-tauri && nice -n 10 cargo clippy --lib --bins -- -D warnings 2>&1 | tail -20
cd src-tauri && nice -n 10 cargo fmt
cd /Users/kingsonwu/programming/rust-src/mesh-talk
git add src-tauri/src/node/node.rs src-tauri/src/node/runtime.rs src-tauri/src/bin/mesh-talk-node.rs
git commit -m "feat(node): thread cryptographic account id into Node (multi-device plan 3)"
```
Expected: build OK; existing tests unaffected (open delegates with the device-user-id default).

---

### Task 3: `send_to_account` (fan-out + self-sync), receive, and account history

**Files:**
- Modify: `src-tauri/src/node/node.rs`

**Interfaces:**
- Consumes: `account_conversation_id`, `DmEnvelope` (Task 1); `Node::account_id` (Task 2); existing `deliver_direct`, `replicate_to_post_office`, `append_event`, `dm_ratchet`, `sentlog`, `received`, `emitted`, roster.
- Produces: `Node::send_to_account(&self, target_account_id: &str, text: &[u8], reply_to: Option<EventId>) -> Result<(), NodeError>`; `Node::account_history(&self, peer_account_id: &str, limit: usize) -> Vec<HistoryEntry>`.

- [ ] **Step 1: add `send_to_account` + a per-device helper (`node.rs`)**

Add near `send_dm_reply`:
```rust
    /// Send a DM to an ACCOUNT: fan out a per-device ratcheted copy to every known
    /// device of `target_account_id`, and self-sync a copy to this user's own other
    /// devices. Records ONE sent entry under the account conversation (so own history
    /// shows it once). Best-effort delivery per device (direct, else post office).
    /// Errors only if there is no known device of the target account to send to.
    pub async fn send_to_account(
        &self,
        target_account_id: &str,
        text: &[u8],
        reply_to: Option<EventId>,
    ) -> Result<(), NodeError> {
        let my_account = self.account_id.clone();
        let body = MessageBody::new(text.to_vec(), reply_to).encode();
        let envelope = DmEnvelope::new(my_account.clone(), target_account_id.to_string(), body)
            .encode();

        // Resolve destinations: the target account's devices + our OWN other devices.
        let me = self.identity.public().user_id();
        let (targets, own): (Vec<PeerRecord>, Vec<PeerRecord>) = {
            let roster = self.roster.lock().expect("roster mutex not poisoned");
            let targets = roster
                .peers()
                .into_iter()
                .filter(|p| p.account_id.as_deref() == Some(target_account_id))
                .collect();
            let own = roster
                .peers()
                .into_iter()
                .filter(|p| p.account_id.as_deref() == Some(my_account.as_str()))
                .filter(|p| p.public.user_id() != me)
                .collect();
            (targets, own)
        };

        if targets.is_empty() {
            return Err(NodeError::UnknownPeer(target_account_id.to_string()));
        }

        // Record one plaintext copy for our own account history (from_me side).
        let conv_account = account_conversation_id(&my_account, target_account_id);
        let wall_clock = now_millis();
        {
            let mut sentlog = self.sentlog.lock().expect("sentlog mutex not poisoned");
            let seq = sentlog.entries(&conv_account).len() as u64 + 1;
            let inner = MessageBody::new(text.to_vec(), reply_to).encode();
            let _ = sentlog.record(conv_account, seq, wall_clock, &inner);
        }

        // Fan out: seal+append+deliver one copy per destination device.
        for peer in targets.iter().chain(own.iter()) {
            self.deliver_enveloped(peer, &envelope).await;
        }
        Ok(())
    }

    /// Seal `plaintext` to `peer`'s device ratchet, append it to the device-pair
    /// conversation's event log, and deliver it (direct, then post office). Best-effort:
    /// transport failures are swallowed (the event is durably logged and will sync
    /// later). The conversation id here is the per-DEVICE-pair id (transport layer).
    async fn deliver_enveloped(&self, peer: &PeerRecord, plaintext: &[u8]) {
        let conv = dm_conversation_id(&self.identity.public(), &peer.public);
        let wire = {
            let mut r = self
                .dm_ratchet
                .lock()
                .expect("dm_ratchet mutex not poisoned");
            match r.encrypt(&self.identity, &peer.public, plaintext) {
                Ok(w) => w,
                Err(_) => return,
            }
        };
        if self.append_event(conv, EventKind::Message, wire).is_err() {
            return;
        }
        let _ = self.deliver_direct(peer, conv).await;
        let _ = self.replicate_to_post_office(conv).await;
        self.emit_new_messages(conv);
    }
```

- [ ] **Step 2: handle the envelope on receive (`emit_new_messages`)**

In `emit_new_messages`, after the line that computes `let wrapped = { ... decrypt ... };` (the decrypted plaintext) and the existing `let body = MessageBody::decode(&wrapped);`, branch on whether `wrapped` is an envelope. Replace the existing record + emit block so that an enveloped message is filed under the ACCOUNT conversation, with the recorded `from` set to the route's sender account (account history derives `from_me` from it), while a legacy message keeps today's device-pair behavior:
```rust
        // Account-addressed (multi-device) envelope? File under the account
        // conversation; otherwise fall back to the legacy device-pair conversation.
        let (record_conv, record_from, inner_body) = match DmEnvelope::decode(&wrapped) {
            Some(env) => {
                let counterparty = if env.route.sender_account == self.account_id {
                    env.route.recipient_account.clone() // a self-synced copy of our own send
                } else {
                    env.route.sender_account.clone()
                };
                let acct_conv = account_conversation_id(&self.account_id, &counterparty);
                (acct_conv, env.route.sender_account.clone(), env.body)
            }
            None => (conv, author_uid.clone(), wrapped.clone()),
        };
        let body = MessageBody::decode(&inner_body);

        let _ = self
            .received
            .lock()
            .expect("received mutex not poisoned")
            .record(
                record_conv,
                record_from,
                event.wall_clock,
                &inner_body,
                event.id,
            );

        self.emitted
            .lock()
            .expect("emitted mutex not poisoned")
            .insert(event.id);

        let _ = self.incoming.send(ReceivedDm {
            from: author_uid,
            from_name: peer_name,
            text: body.text,
            reply_to: body.reply_to,
        });
```
(This replaces the existing `let body = ...; let _ = self.received...record(conv, author_uid.clone(), ...); self.emitted...insert; self.incoming.send(...)` tail of the loop. The live `ReceivedDm` still carries the author device's id/name as before — display switches to account history below; the live signal is just a "something changed" poke.)

- [ ] **Step 3: add `account_history` (`node.rs`)**

Add near `dm_history`:
```rust
    /// Account-level conversation history with `peer_account_id`, merging our sent
    /// copies (recorded once under the account conversation) and the per-device copies
    /// we received and filed under it. `from_me` is derived from the recorded sender
    /// account, so self-synced copies of our own sends show as ours.
    pub fn account_history(&self, peer_account_id: &str, limit: usize) -> Vec<HistoryEntry> {
        let conv = account_conversation_id(&self.account_id, peer_account_id);
        let mut entries: Vec<HistoryEntry> = Vec::new();

        for sent in self
            .sentlog
            .lock()
            .expect("sentlog mutex not poisoned")
            .entries(&conv)
        {
            let body = MessageBody::decode(&sent.plaintext);
            entries.push(HistoryEntry {
                id: EventId::new([0u8; 32]),
                from_me: true,
                who: "you".to_string(),
                text: body.text,
                wall_clock: sent.wall_clock,
                reply_to: body.reply_to,
            });
        }

        for rcv in self
            .received
            .lock()
            .expect("received mutex not poisoned")
            .entries(&conv)
        {
            let body = MessageBody::decode(&rcv.plaintext);
            let from_me = rcv.from == self.account_id;
            entries.push(HistoryEntry {
                id: rcv.event_id,
                from_me,
                who: if from_me { "you".to_string() } else { rcv.from },
                text: body.text,
                wall_clock: rcv.wall_clock,
                reply_to: body.reply_to,
            });
        }

        entries.sort_by_key(|e| e.wall_clock);
        if entries.len() > limit {
            entries.drain(0..entries.len() - limit);
        }
        entries
    }
```

- [ ] **Step 4: integration tests (append to `node.rs` `mod tests`)**

Add a helper that seeds a roster entry carrying an account, then two tests: a peer-account fan-out and a self-sync.
```rust
    /// Seed `roster` with `peer` advertised under `account`, reachable at `port`.
    fn add_account_peer(
        roster: &Arc<Mutex<Roster>>,
        peer: &DeviceIdentity,
        account: &crate::identity::account::Account,
        name: &str,
        port: u16,
        self_user_id: &str,
    ) {
        roster.lock().unwrap().update(
            &Announce::new_with_account(peer, account, name, port),
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            self_user_id,
        );
    }

    #[tokio::test]
    async fn send_to_account_delivers_to_a_peer_account_over_loopback() {
        use crate::identity::account::Account;
        let alice = DeviceIdentity::generate();
        let alice_acct = Account::generate();
        let bob = DeviceIdentity::generate();
        let bob_acct = Account::generate();
        let alice_uid = alice.public().user_id();
        let bob_uid = bob.public().user_id();
        let dir = tempfile::tempdir().unwrap();

        let bob_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let bob_addr = bob_listener.local_addr().unwrap();

        let alice_roster = Arc::new(Mutex::new(Roster::default()));
        add_account_peer(&alice_roster, &bob, &bob_acct, "Bob", bob_addr.port(), &alice_uid);
        let bob_roster = Arc::new(Mutex::new(Roster::default()));
        add_account_peer(&bob_roster, &alice, &alice_acct, "Alice", 4000, &bob_uid);

        let (a_dm, _a_dm_r) = mpsc::unbounded_channel();
        let (a_ch, _a_ch_r) = mpsc::unbounded_channel();
        let (a_f, _a_f_r) = mpsc::unbounded_channel();
        let (b_dm, _b_dm_r) = mpsc::unbounded_channel();
        let (b_ch, _b_ch_r) = mpsc::unbounded_channel();
        let (b_f, _b_f_r) = mpsc::unbounded_channel();

        let alice_node = Node::open_with_account(
            alice, alice_acct.account_id(), alice_roster, a_dm, a_ch, a_f,
            &dir.path().join("a.log"), &dir.path().join("a-sent.log"), "pw",
        ).unwrap();
        let bob_node = Node::open_with_account(
            bob, bob_acct.account_id(), bob_roster, b_dm, b_ch, b_f,
            &dir.path().join("b.log"), &dir.path().join("b-sent.log"), "pw",
        ).unwrap();

        tokio::spawn(Arc::clone(&bob_node).run_accept_loop(bob_listener));

        alice_node
            .send_to_account(&bob_acct.account_id(), b"hi account", None)
            .await
            .unwrap();

        // Bob's account history (keyed by Alice's account) shows the message.
        let mut bob_hist = Vec::new();
        for _ in 0..50 {
            bob_hist = bob_node.account_history(&alice_acct.account_id(), 10);
            if !bob_hist.is_empty() { break; }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        assert_eq!(bob_hist.len(), 1, "bob received the account-addressed message");
        assert!(!bob_hist[0].from_me);
        assert_eq!(bob_hist[0].text, b"hi account");

        // Alice's own account history shows it once, from her.
        let a_hist = alice_node.account_history(&bob_acct.account_id(), 10);
        assert_eq!(a_hist.len(), 1);
        assert!(a_hist[0].from_me);
        assert_eq!(a_hist[0].text, b"hi account");
    }

    #[tokio::test]
    async fn send_to_account_self_syncs_to_own_other_device() {
        use crate::identity::account::Account;
        // Alice has TWO devices sharing one account; Bob is a separate account.
        let a1 = DeviceIdentity::generate();
        let a2 = DeviceIdentity::generate();
        let alice_acct = Account::generate();
        let bob = DeviceIdentity::generate();
        let bob_acct = Account::generate();
        let a1_uid = a1.public().user_id();
        let dir = tempfile::tempdir().unwrap();

        let a2_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let a2_addr = a2_listener.local_addr().unwrap();
        let bob_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let bob_addr = bob_listener.local_addr().unwrap();

        // A1 knows its sibling A2 (same account) and Bob (other account).
        let a1_roster = Arc::new(Mutex::new(Roster::default()));
        add_account_peer(&a1_roster, &a2, &alice_acct, "A2", a2_addr.port(), &a1_uid);
        add_account_peer(&a1_roster, &bob, &bob_acct, "Bob", bob_addr.port(), &a1_uid);
        let a2_roster = Arc::new(Mutex::new(Roster::default()));
        add_account_peer(&a2_roster, &a1, &alice_acct, "A1", 4000, &a2.public().user_id());
        let bob_roster = Arc::new(Mutex::new(Roster::default()));
        add_account_peer(&bob_roster, &a1, &alice_acct, "A1", 4000, &bob.public().user_id());

        let (a1_dm, _x1) = mpsc::unbounded_channel();
        let (a1_ch, _x2) = mpsc::unbounded_channel();
        let (a1_f, _x3) = mpsc::unbounded_channel();
        let (a2_dm, _x4) = mpsc::unbounded_channel();
        let (a2_ch, _x5) = mpsc::unbounded_channel();
        let (a2_f, _x6) = mpsc::unbounded_channel();
        let (b_dm, _x7) = mpsc::unbounded_channel();
        let (b_ch, _x8) = mpsc::unbounded_channel();
        let (b_f, _x9) = mpsc::unbounded_channel();

        let a1_node = Node::open_with_account(
            a1, alice_acct.account_id(), a1_roster, a1_dm, a1_ch, a1_f,
            &dir.path().join("a1.log"), &dir.path().join("a1-sent.log"), "pw",
        ).unwrap();
        let a2_node = Node::open_with_account(
            a2, alice_acct.account_id(), a2_roster, a2_dm, a2_ch, a2_f,
            &dir.path().join("a2.log"), &dir.path().join("a2-sent.log"), "pw",
        ).unwrap();
        let bob_node = Node::open_with_account(
            bob, bob_acct.account_id(), bob_roster, b_dm, b_ch, b_f,
            &dir.path().join("b.log"), &dir.path().join("b-sent.log"), "pw",
        ).unwrap();

        tokio::spawn(Arc::clone(&a2_node).run_accept_loop(a2_listener));
        tokio::spawn(Arc::clone(&bob_node).run_accept_loop(bob_listener));

        a1_node
            .send_to_account(&bob_acct.account_id(), b"to bob", None)
            .await
            .unwrap();

        // A2 (Alice's other device) shows the message in the Alice↔Bob conversation,
        // marked from_me (it was sent by Alice's account).
        let mut a2_hist = Vec::new();
        for _ in 0..50 {
            a2_hist = a2_node.account_history(&bob_acct.account_id(), 10);
            if !a2_hist.is_empty() { break; }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        assert_eq!(a2_hist.len(), 1, "A2 self-synced the message");
        assert!(a2_hist[0].from_me, "shown as ours on the other device");
        assert_eq!(a2_hist[0].text, b"to bob");

        // Bob receives it once, not from_me.
        let mut b_hist = Vec::new();
        for _ in 0..50 {
            b_hist = bob_node.account_history(&alice_acct.account_id(), 10);
            if !b_hist.is_empty() { break; }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        assert_eq!(b_hist.len(), 1);
        assert!(!b_hist[0].from_me);
        assert_eq!(b_hist[0].text, b"to bob");
    }
```

- [ ] **Step 5: test, clippy, fmt, commit**
```bash
cd src-tauri && nice -n 10 cargo test --lib node::node::tests::send_to_account -- --test-threads=2
cd src-tauri && nice -n 10 cargo clippy --lib -- -D warnings 2>&1 | tail -20
cd src-tauri && nice -n 10 cargo fmt
cd /Users/kingsonwu/programming/rust-src/mesh-talk
git add src-tauri/src/node/node.rs
git commit -m "feat(node): account-addressed send_to_account with fan-out + self-sync + account history (multi-device plan 3)"
```

---

### Task 4: Expose account messaging through the runtime + IPC

**Files:**
- Modify: `src-tauri/src/node/runtime.rs` (`send_to_account`, `account_history`, `account_id` already added in Plan 2)
- Modify: `src-tauri/src/redesign_commands.rs` (IPC commands)
- Modify: `src-tauri/src/lib.rs` (register the new commands in the invoke handler)

**Interfaces:**
- Produces: `RedesignRuntime::{send_to_account, account_history}`; Tauri commands `redesign_send_to_account`, `redesign_account_history`, `redesign_account_id`.

- [ ] **Step 1: runtime methods (`runtime.rs`)**

Add to `impl RedesignRuntime` (near `send_dm`/`history`):
```rust
    /// Send a DM addressed to an account (fan-out to its devices + self-sync to ours).
    pub async fn send_to_account(
        &self,
        target_account_id: &str,
        text: &[u8],
        reply_to: Option<crate::eventlog::event::EventId>,
    ) -> Result<(), NodeError> {
        self.node
            .send_to_account(target_account_id, text, reply_to)
            .await
    }

    /// Account-level conversation history with `peer_account_id`.
    pub fn account_history(&self, peer_account_id: &str, limit: usize) -> Vec<HistoryEntry> {
        self.node.account_history(peer_account_id, limit)
    }
```

- [ ] **Step 2: IPC commands (`redesign_commands.rs`)**

Mirror `redesign_send_dm` / `redesign_history`. Add:
```rust
#[tauri::command]
pub async fn redesign_send_to_account(
    state: tauri::State<'_, RedesignState>,
    account: String,
    text: String,
    reply_to: Option<String>,
) -> Result<(), String> {
    let node = {
        let guard = state.0.lock().await;
        let rt = guard.as_ref().ok_or_else(|| NOT_STARTED.to_string())?;
        rt.handle()
    };
    let reply = match reply_to {
        Some(h) => Some(parse_event_id(&h)?),
        None => None,
    };
    node.send_to_account(&account, text.as_bytes(), reply)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn redesign_account_history(
    state: tauri::State<'_, RedesignState>,
    account: String,
    limit: usize,
) -> Result<Vec<HistoryItem>, String> {
    let limit = limit.min(500);
    let guard = state.0.lock().await;
    let rt = guard.as_ref().ok_or_else(|| NOT_STARTED.to_string())?;
    Ok(rt
        .account_history(&account, limit)
        .into_iter()
        .map(|h| HistoryItem {
            id: hex::encode(h.id.as_bytes()),
            from_me: h.from_me,
            who: h.who,
            text: String::from_utf8_lossy(&h.text).into_owned(),
            wall_clock: h.wall_clock,
            reply_to: h.reply_to.map(|id| hex::encode(id.as_bytes())),
        })
        .collect())
}

#[tauri::command]
pub async fn redesign_account_id(
    state: tauri::State<'_, RedesignState>,
) -> Result<String, String> {
    let guard = state.0.lock().await;
    let rt = guard.as_ref().ok_or_else(|| NOT_STARTED.to_string())?;
    Ok(rt.account_id().to_string())
}
```
(`node` here is `Arc<Node>`; `rt.handle()` returns it. If `redesign_commands.rs` does not already `use` the node type, the existing `redesign_send_dm` shows the pattern — copy its imports.)

- [ ] **Step 3: register the commands (`lib.rs`)**

Find the `tauri::generate_handler![ ... ]` list containing `redesign_send_dm` and add `redesign_send_to_account`, `redesign_account_history`, `redesign_account_id` (with the `redesign_commands::` path prefix matching the existing entries).

- [ ] **Step 4: build, clippy, fmt, commit**
```bash
cd src-tauri && nice -n 10 cargo build --lib --bins 2>&1 | tail -15
cd src-tauri && nice -n 10 cargo clippy --lib --bins -- -D warnings 2>&1 | tail -20
cd src-tauri && nice -n 10 cargo fmt
cd /Users/kingsonwu/programming/rust-src/mesh-talk
git add src-tauri/src/node/runtime.rs src-tauri/src/redesign_commands.rs src-tauri/src/lib.rs
git commit -m "feat(ipc): expose account-addressed send + history to the frontend (multi-device plan 3)"
```

---

## Notes for the reviewer

- **Delivered:** account-addressed DMs. `send_to_account` fans out a per-device ratcheted copy to every known device of the target account and self-syncs to the sender's own other devices; the receiver files each copy under an account-keyed conversation and `account_history` merges them with `from_me` derived from the route. The device-pair ratchet/log/sync are untouched (still the transport).
- **Reviewer checks:**
  - Routing lives INSIDE the sealed plaintext (`DmEnvelope`), so a relay/post office never sees who the logical sender/recipient accounts are beyond what the device-pair already reveals.
  - No `msg_id` dedup is needed: each logical message is sealed once per destination device; re-delivery idempotency is the existing event-id `emitted` set. The two integration tests confirm exactly one history entry per side.
  - `from_me` for a self-synced copy comes from the route (`sender_account == my account`), NOT event authorship (the author is the sibling device) — the self-sync test asserts this.
  - Legacy device-addressed `send_dm`/`dm_history` untouched; a non-enveloped plaintext still decodes as a raw `MessageBody`.
- **Staging:** with each device its own account (pre-linking), `send_to_account` to a peer hits their single device and self-sync is a no-op — equivalent to today's DM. Plan 4 (device linking) makes a user's devices share one account, at which point fan-out + self-sync light up for real, and the UI switches from device-addressed to account-addressed conversations.
- **Deferred to Plan 4:** the device-linking pairing flow (transfer the account secret to a new device over the LAN Noise channel) + the "link a device" screen + switching the /redesign conversation UI to address peers by account.
