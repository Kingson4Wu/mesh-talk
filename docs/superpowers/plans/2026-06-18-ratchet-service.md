# Ratchet Service Layer Implementation Plan (Ratchet Plan 3)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development. Steps use checkbox syntax. SECURITY-SENSITIVE.

**Goal:** The durable + session-management layer between the pure ratchet core and the Node: an encrypted per-peer session store, an encrypted received-plaintext store, and a `DmRatchet` that establishes/loads sessions and encrypts/decrypts DM payloads with persistence. Fully tested in isolation (no Node surgery — that's Plan 4).

**Architecture:** `ReceivedLog` mirrors `SentLog` (received plaintext, so history survives key deletion). `RatchetSessions` is an append-last-wins encrypted store of serialized `RatchetState` per peer. `DmRatchet` ties them together: deterministic bootstrap (shared secret from the identity-key DH; first-sender is the initiator; `user_id` tie-break for simultaneous init), `encrypt(peer, plaintext) -> wire` / `decrypt(peer, wire) -> plaintext`, persisting session + received plaintext.

**Tech Stack:** Rust; `crate::ratchet`, `crate::storage::encryption` (`EncryptionKey`, `encrypt_data`/`decrypt_data`, `generate_salt`, `SALT_SIZE`, `NONCE_SIZE`), `crate::dm` (identity DH), `bincode`. No new dependencies.

---

## Background the implementer needs

- `crate::ratchet`: `RatchetState`, `init_alice(shared: &[u8;32], peer_ratchet_pub: &[u8;32])`, `init_bob(shared: &[u8;32], my_ratchet_secret: [u8;32])`, `state.ratchet_encrypt(pt) -> Result<(Header, Vec<u8>), RatchetError>`, `state.ratchet_decrypt(&Header, ct) -> Result<Vec<u8>, RatchetError>`, `state.serialize() -> Vec<u8>`, `RatchetState::deserialize(&[u8]) -> Option<RatchetState>`, `Header::{encode? no — Header has pub fields + decode}`. NOTE: `Header` currently has a private `encode`; ADD a `pub fn encode(&self) -> Vec<u8>` (mirror the existing private one) so the service can frame it. `Header { ratchet_pub: [u8;32], pn: u32, n: u32 }`, `Header::decode(&[u8]) -> Option<Header>`.
- `crate::identity::device::{DeviceIdentity, PublicIdentity}`: `identity.public().x25519_pub: [u8;32]`, `identity.public().user_id() -> String`, and the x25519 SECRET — check how `dm.rs` gets it: `StaticSecret::from(sender.secret_bytes().1)` (`secret_bytes()` returns `(ed25519, x25519)` bytes; the x25519 secret is `.1`). Use the SAME accessor.
- Shared secret: `HKDF-SHA256(salt=None, ikm = DH(my_x25519_secret, peer_x25519_pub))` expanded with info `b"MeshTalk-DM-Root"` → 32 bytes. Both peers compute the identical value (DH symmetry). Implement a small helper using `x25519_dalek::StaticSecret::diffie_hellman` + `hkdf` (mirror `dm.rs`).
- `crate::storage::encryption`: `EncryptionKey::from_password(password, &salt) -> Result<EncryptionKey, _>`, `encrypt_data(&[u8], &key) -> Result<(ciphertext, nonce), _>`, `decrypt_data(ct, &nonce, &key)`, `generate_salt() -> [u8; SALT_SIZE]`, consts `SALT_SIZE`, `NONCE_SIZE`. `crate::eventlog::LogError` for errors. `ConversationId` from `eventlog::event`.
- The `SentLog` file framing (magic + salt header, then length-prefixed `u32-BE ‖ nonce ‖ ciphertext` records, torn-trailing-record tolerant) is the exact template — read `src-tauri/src/node/sentlog.rs`.

**CPU/test discipline:** `nice -n 10`, `--test-threads=2`. `cargo test --lib node::received_log node::ratchet_sessions node::dm_ratchet ratchet`; build/clippy/fmt. Confirm branch line after committing.

---

### Task 1: `ReceivedLog` + `RatchetSessions` (durable encrypted stores)

**Files:** Create `src-tauri/src/node/received_log.rs`, `src-tauri/src/node/ratchet_sessions.rs`; modify `node/mod.rs`. Also add `pub fn encode` to `ratchet/state.rs`'s `Header`.

- [ ] **Step 1: `Header::encode` is public**

In `ratchet/state.rs`, change `Header`'s `fn encode` to `pub fn encode` (it's used internally for AAD; the service also needs it for wire framing). No behavior change.

- [ ] **Step 2: `received_log.rs` — mirror `SentLog`**

Copy `SentLog`'s structure exactly, with magic `b"MTRECV"`, and a `ReceivedEntry { conversation: ConversationId, from: String, wall_clock: u64, plaintext: Vec<u8> }`. Same `open`/`load`/`parse_records` (torn-record tolerant) / `record(conversation, from, wall_clock, plaintext)` / `entries(&conversation) -> Vec<ReceivedEntry>` (sorted by `wall_clock`). Port `SentLog`'s tests adapted (records survive reopen, wrong password fails, torn record dropped).

- [ ] **Step 3: `ratchet_sessions.rs` — append-last-wins per peer**

An encrypted store mapping peer `user_id -> serialized RatchetState`. Same header framing (magic `b"MTRTCH"`). Records are `SessionRecord { peer: String, state: Vec<u8> }` (the `state` is `RatchetState::serialize()` output — already secret, doubly-encrypted here is fine). On `load`, replay records into a `HashMap<String, Vec<u8>>` (last write wins per peer → the latest session). API:
```rust
pub struct RatchetSessions { file: File, key: EncryptionKey, latest: HashMap<String, Vec<u8>> }
impl RatchetSessions {
    pub fn open(path: &Path, password: &str) -> Result<Self, LogError>; // like SentLog::open
    /// The latest stored session for `peer`, deserialized, if any.
    pub fn get(&self, peer: &str) -> Option<RatchetState>;   // RatchetState::deserialize(self.latest.get(peer)?)
    /// Persist `state` for `peer` (append a record; update the index).
    pub fn put(&mut self, peer: &str, state: &RatchetState) -> Result<(), LogError>; // serialize + append + latest.insert
    pub fn has(&self, peer: &str) -> bool;
}
```
Tests: put then get round-trips a session; reopen restores the latest (a later put for the same peer wins); wrong password fails.

- [ ] **Step 4: register modules (`node/mod.rs`)**

`pub mod received_log;` + `pub mod ratchet_sessions;` + re-exports `pub use received_log::{ReceivedEntry, ReceivedLog};` and `pub use ratchet_sessions::RatchetSessions;`.

- [ ] **Step 5: build, test, commit**
```bash
cd src-tauri && nice -n 10 cargo test --lib node::received_log node::ratchet_sessions -- --test-threads=2
cd src-tauri && nice -n 10 cargo build --lib && nice -n 10 cargo clippy --lib -- -D warnings 2>&1 | tail -20
cd src-tauri && nice -n 10 cargo fmt
cd /Users/kingsonwu/programming/rust-src/mesh-talk
git add src-tauri/src/node/received_log.rs src-tauri/src/node/ratchet_sessions.rs src-tauri/src/node/mod.rs src-tauri/src/ratchet/state.rs
git commit -m "feat(node): received-plaintext store + durable ratchet-session store"
git status | head -1
```

---

### Task 2: `DmRatchet` — session establishment + encrypt/decrypt with persistence

**Files:** Create `src-tauri/src/node/dm_ratchet.rs`; modify `node/mod.rs`.

- [ ] **Step 1: the shared-secret helper + wire framing**

In `dm_ratchet.rs`:
```rust
//! The DM ratchet service: per-peer session establishment + encrypt/decrypt with
//! durable session persistence. Bootstrap is deterministic — the shared root secret
//! is the identity-key DH; the first sender is the initiator (`init_alice`), the
//! receiver bootstraps (`init_bob`); simultaneous init is resolved by a user-id
//! tie-break. The wire payload frames the ratchet header + ciphertext.

use crate::eventlog::LogError;
use crate::identity::device::{DeviceIdentity, PublicIdentity};
use crate::node::ratchet_sessions::RatchetSessions;
use crate::ratchet::{init_alice, init_bob, Header, RatchetState};
use hkdf::Hkdf;
use sha2::Sha256;
use x25519_dalek::{PublicKey, StaticSecret};

/// Derive the shared root secret from the two identities' X25519 keys (symmetric).
fn shared_root(me: &DeviceIdentity, peer: &PublicIdentity) -> [u8; 32] {
    let my_secret = StaticSecret::from(me.secret_bytes().1); // x25519 secret (mirror dm.rs)
    let dh = my_secret.diffie_hellman(&PublicKey::from(peer.x25519_pub)).to_bytes();
    let hk = Hkdf::<Sha256>::new(None, &dh);
    let mut out = [0u8; 32];
    hk.expand(b"MeshTalk-DM-Root", &mut out).expect("32 valid");
    out
}

/// Frame a header + ciphertext into one wire blob: `u16-BE header_len ‖ header ‖ ct`.
fn frame(header: &Header, ct: &[u8]) -> Vec<u8> {
    let h = header.encode();
    let mut out = Vec::with_capacity(2 + h.len() + ct.len());
    out.extend_from_slice(&(h.len() as u16).to_be_bytes());
    out.extend_from_slice(&h);
    out.extend_from_slice(ct);
    out
}

fn unframe(wire: &[u8]) -> Option<(Header, &[u8])> {
    if wire.len() < 2 {
        return None;
    }
    let hlen = u16::from_be_bytes([wire[0], wire[1]]) as usize;
    if wire.len() < 2 + hlen {
        return None;
    }
    let header = Header::decode(&wire[2..2 + hlen])?;
    Some((header, &wire[2 + hlen..]))
}
```

- [ ] **Step 2: `DmRatchet` (encrypt/decrypt with bootstrap + persistence)**
```rust
/// Owns the durable session store + this node's identity; manages per-peer ratchets.
pub struct DmRatchet {
    sessions: RatchetSessions,
}

impl DmRatchet {
    pub fn new(sessions: RatchetSessions) -> Self {
        DmRatchet { sessions }
    }

    /// Encrypt `plaintext` to `peer`. Establishes the session (as initiator) on first
    /// use. Returns the wire blob (header + ciphertext). Persists the advanced session.
    pub fn encrypt(
        &mut self,
        me: &DeviceIdentity,
        peer: &PublicIdentity,
        plaintext: &[u8],
    ) -> Result<Vec<u8>, LogError> {
        let peer_id = peer.user_id();
        let mut state = match self.sessions.get(&peer_id) {
            Some(s) => s,
            None => init_alice(&shared_root(me, peer), &peer.x25519_pub),
        };
        let (header, ct) = state
            .ratchet_encrypt(plaintext)
            .map_err(|_| LogError::Serialization("ratchet encrypt".into()))?;
        self.sessions.put(&peer_id, &state)?;
        Ok(frame(&header, &ct))
    }

    /// Decrypt a wire blob from `peer`. Bootstraps as responder on first contact;
    /// resolves a simultaneous-init race by a user-id tie-break. Persists the session.
    pub fn decrypt(
        &mut self,
        me: &DeviceIdentity,
        peer: &PublicIdentity,
        wire: &[u8],
    ) -> Result<Vec<u8>, LogError> {
        let (header, ct) = unframe(wire).ok_or_else(|| LogError::Serialization("bad wire".into()))?;
        let peer_id = peer.user_id();
        let my_secret = me.secret_bytes().1; // x25519 secret bytes

        let mut state = match self.sessions.get(&peer_id) {
            Some(s) => s,
            None => init_bob(&shared_root(me, peer), my_secret),
        };
        match state.ratchet_decrypt(&header, ct) {
            Ok(pt) => {
                self.sessions.put(&peer_id, &state)?;
                Ok(pt)
            }
            Err(_) => {
                // Possible simultaneous init: we set up as initiator, but the peer's
                // message expects us to be the responder. Tie-break: the LOWER user-id
                // is the canonical initiator. If the PEER is canonical-initiator, drop
                // our session, re-bootstrap as responder, and retry once.
                if peer_id < me.public().user_id() {
                    let mut fresh = init_bob(&shared_root(me, peer), my_secret);
                    let pt = fresh
                        .ratchet_decrypt(&header, ct)
                        .map_err(|_| LogError::Serialization("ratchet decrypt".into()))?;
                    self.sessions.put(&peer_id, &fresh)?;
                    Ok(pt)
                } else {
                    Err(LogError::Serialization("ratchet decrypt".into()))
                }
            }
        }
    }

    pub fn has_session(&self, peer: &PublicIdentity) -> bool {
        self.sessions.has(&peer.user_id())
    }
}
```
(`me.secret_bytes()` returns `(ed25519_bytes, x25519_bytes)`; `.1` is the x25519 secret `[u8;32]`. Verify against `dm.rs` usage and adjust if the accessor differs.)

- [ ] **Step 3: register + tests (`node/mod.rs` + `dm_ratchet.rs`)**

`pub mod dm_ratchet;` + `pub use dm_ratchet::DmRatchet;`.
Tests (two managers over temp session stores; build `DeviceIdentity`s):
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::device::DeviceIdentity;

    fn manager(dir: &std::path::Path, name: &str) -> DmRatchet {
        DmRatchet::new(RatchetSessions::open(&dir.join(name), "pw").unwrap())
    }

    #[test]
    fn two_parties_exchange_and_persist() {
        let dir = tempfile::tempdir().unwrap();
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let mut am = manager(dir.path(), "a.sess");
        let mut bm = manager(dir.path(), "b.sess");

        let w1 = am.encrypt(&alice, &bob.public(), b"hi bob").unwrap();
        assert_eq!(bm.decrypt(&bob, &alice.public(), &w1).unwrap(), b"hi bob");
        let w2 = bm.encrypt(&bob, &alice.public(), b"hi alice").unwrap();
        assert_eq!(am.decrypt(&alice, &bob.public(), &w2).unwrap(), b"hi alice");

        // Reload both managers from disk; the session continues.
        let mut am2 = manager(dir.path(), "a.sess");
        let w3 = am2.encrypt(&alice, &bob.public(), b"still works").unwrap();
        assert_eq!(bm.decrypt(&bob, &alice.public(), &w3).unwrap(), b"still works");
    }

    #[test]
    fn out_of_order_delivery_opens() {
        let dir = tempfile::tempdir().unwrap();
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let mut am = manager(dir.path(), "a.sess");
        let mut bm = manager(dir.path(), "b.sess");
        let w0 = am.encrypt(&alice, &bob.public(), b"m0").unwrap();
        let w1 = am.encrypt(&alice, &bob.public(), b"m1").unwrap();
        let w2 = am.encrypt(&alice, &bob.public(), b"m2").unwrap();
        assert_eq!(bm.decrypt(&bob, &alice.public(), &w2).unwrap(), b"m2");
        assert_eq!(bm.decrypt(&bob, &alice.public(), &w0).unwrap(), b"m0");
        assert_eq!(bm.decrypt(&bob, &alice.public(), &w1).unwrap(), b"m1");
    }

    #[test]
    fn simultaneous_init_is_resolved_by_tie_break() {
        // Both sides send first (each becomes initiator), then each receives the
        // other's first message. The tie-break re-bootstraps the higher-id side as
        // responder so at least the canonical-initiator's messages open.
        let dir = tempfile::tempdir().unwrap();
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let mut am = manager(dir.path(), "a.sess");
        let mut bm = manager(dir.path(), "b.sess");
        let wa = am.encrypt(&alice, &bob.public(), b"from-a").unwrap();
        let wb = bm.encrypt(&bob, &alice.public(), b"from-b").unwrap();
        // Identify the canonical initiator (lower user-id) and assert its message opens
        // on the other side after the tie-break.
        if alice.public().user_id() < bob.public().user_id() {
            assert_eq!(bm.decrypt(&bob, &alice.public(), &wa).unwrap(), b"from-a");
        } else {
            assert_eq!(am.decrypt(&alice, &bob.public(), &wb).unwrap(), b"from-b");
        }
    }
}
```

- [ ] **Step 4: build, test, clippy, fmt, commit**
```bash
cd src-tauri && nice -n 10 cargo test --lib node::dm_ratchet -- --test-threads=2
cd src-tauri && nice -n 10 cargo build --lib && nice -n 10 cargo clippy --lib -- -D warnings 2>&1 | tail -20
cd src-tauri && nice -n 10 cargo fmt
cd /Users/kingsonwu/programming/rust-src/mesh-talk
git add src-tauri/src/node/dm_ratchet.rs src-tauri/src/node/mod.rs
git commit -m "feat(node): DmRatchet — per-peer session establishment + encrypt/decrypt with persistence"
git status | head -1
```

---

## Notes for the reviewer

- **Delivered:** the durable session + received-plaintext stores and `DmRatchet` (deterministic bootstrap, encrypt/decrypt with per-message session persistence, simultaneous-init tie-break). Tested in isolation: exchange + persistence + out-of-order + the race.
- **Reviewer checks:** the shared-secret helper computes identically on both sides (DH symmetry + HKDF); the initiator/responder bootstrap aligns the first DH ratchet (Alice's bootstrap dhr = Bob's identity x25519 pub; Bob's bootstrap dhs = his identity x25519 secret — the first ratchet DH matches); the session is persisted AFTER every encrypt/decrypt (so a reload continues); the tie-break is deterministic (lower user-id is canonical initiator) and only re-bootstraps the higher-id side; the wire framing round-trips and rejects malformed input; the session blob is encrypted at rest (`RatchetSessions`).
- **Deferred to Plan 4 (Node wiring):** route the redesign DM `Message` path through `DmRatchet` (replace `dm::seal`/`open_dm_event` for messages — the redesign is unreleased, so NO legacy coexistence); store received plaintext in `ReceivedLog`; serve `history` from `SentLog` + `ReceivedLog` (the wire key is gone); thread the stores into `Node::open`; loopback rig proving forward secrecy end-to-end. (File-manifest + reaction events keep using `dm::seal` — they are not the message path.)
