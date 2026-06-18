# Double Ratchet Hardening + Serialization Implementation Plan (Ratchet Plan 2)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development. Steps use checkbox (`- [ ]`) syntax. SECURITY-CRITICAL — preserve the algorithm; only add serialization + the two hardening items.

**Goal:** Make `RatchetState` durable-ready (serialize/deserialize a live session losslessly) and fold in the adversarial review's two follow-ups: bound the skipped-key map (memory-DoS) and zeroize raw key material on drop.

**Architecture:** Additions to `crate::ratchet::state` only. A private `RatchetWire` (all-bytes serde mirror) backs `RatchetState::serialize`/`deserialize`. A FIFO-bounded skipped-key map caps total memory. A `Drop` impl zeroizes raw key arrays (the X25519 `StaticSecret` already self-zeroizes).

**Tech Stack:** Rust; `bincode` (fixint), `zeroize` (add as a direct dep — already in the tree via transitive deps), the existing ratchet primitives.

---

## Background the implementer needs

`crate::ratchet::state::RatchetState` (Plan 1, reviewed sound) holds: `dhs_secret: StaticSecret`, `dhs_public: PublicKey`, `dhr: Option<PublicKey>`, `rk/cks/ckr` (`[u8;32]` / `Option`), `ns/nr/pn: u32`, `skipped: HashMap<([u8;32], u32), [u8;32]>`. `StaticSecret::to_bytes() -> [u8;32]`, `StaticSecret::from([u8;32])`, `PublicKey::from(&StaticSecret)`, `PublicKey::from([u8;32])`, `PublicKey::to_bytes() -> [u8;32]`. `skip_message_keys` inserts into `skipped`; `ratchet_decrypt` removes from it. The review flagged: (6) `skipped` is unbounded across steps; (10) raw key arrays aren't zeroized.

**CPU/test discipline:** `nice -n 10`, `--test-threads=2`. `cargo test --lib ratchet`; `cargo build --lib`; `cargo clippy --lib -- -D warnings`; `cargo fmt`. Confirm branch line after committing.

---

### Task 1: lossless serialization of a live session

**Files:** Modify `src-tauri/src/ratchet/state.rs`, `ratchet/mod.rs`.

- [ ] **Step 1: `RatchetWire` + `serialize`/`deserialize`**

In `state.rs`, add a private serde mirror + the two methods on `RatchetState`:
```rust
#[derive(Serialize, Deserialize)]
struct RatchetWire {
    dhs_secret: [u8; 32],
    dhr: Option<[u8; 32]>,
    rk: [u8; 32],
    cks: Option<[u8; 32]>,
    ckr: Option<[u8; 32]>,
    ns: u32,
    nr: u32,
    pn: u32,
    // (their ratchet pub, message number, message key)
    skipped: Vec<([u8; 32], u32, [u8; 32])>,
}

impl RatchetState {
    /// Serialize the full session (fixint bincode). The output is SECRET — callers
    /// MUST store it encrypted at rest.
    pub fn serialize(&self) -> Vec<u8> {
        let wire = RatchetWire {
            dhs_secret: self.dhs_secret.to_bytes(),
            dhr: self.dhr.map(|p| p.to_bytes()),
            rk: self.rk,
            cks: self.cks,
            ckr: self.ckr,
            ns: self.ns,
            nr: self.nr,
            pn: self.pn,
            skipped: self
                .skipped
                .iter()
                .map(|((pub_, n), mk)| (*pub_, *n, *mk))
                .collect(),
        };
        bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .serialize(&wire)
            .expect("ratchet state serializes")
    }

    /// Reconstruct a session from [`serialize`] output. `None` if malformed.
    pub fn deserialize(bytes: &[u8]) -> Option<RatchetState> {
        let wire: RatchetWire = bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .reject_trailing_bytes()
            .deserialize(bytes)
            .ok()?;
        let dhs_secret = StaticSecret::from(wire.dhs_secret);
        let dhs_public = PublicKey::from(&dhs_secret);
        let skipped = wire
            .skipped
            .into_iter()
            .map(|(pub_, n, mk)| ((pub_, n), mk))
            .collect();
        Some(RatchetState {
            dhs_secret,
            dhs_public,
            dhr: wire.dhr.map(PublicKey::from),
            rk: wire.rk,
            cks: wire.cks,
            ckr: wire.ckr,
            ns: wire.ns,
            nr: wire.nr,
            pn: wire.pn,
            skipped,
            // (if Task 2's `skipped_order` field is added, initialize it from `skipped` keys here)
        })
    }
}
```
Add `pub use state::RatchetState;` already exists; ensure `serialize`/`deserialize` are reachable (they're `pub` methods).

- [ ] **Step 2: test — a serialized session resumes losslessly**

Append in `state.rs` tests:
```rust
    #[test]
    fn a_serialized_session_resumes_losslessly() {
        let (mut alice, mut bob) = pair();
        // Exchange a few messages (advance both chains + a DH ratchet).
        let (h1, c1) = alice.ratchet_encrypt(b"a1").unwrap();
        bob.ratchet_decrypt(&h1, &c1).unwrap();
        let (hb, cb) = bob.ratchet_encrypt(b"b1").unwrap();
        alice.ratchet_decrypt(&hb, &cb).unwrap();
        // Create a skipped key on Bob's side (Alice sends 2, Bob will get #1 later).
        let (h2, c2) = alice.ratchet_encrypt(b"a2").unwrap();
        let (h3, c3) = alice.ratchet_encrypt(b"a3").unwrap();
        bob.ratchet_decrypt(&h3, &c3).unwrap(); // stores skipped key for a2

        // Serialize BOTH, drop, and reload.
        let alice2 = RatchetState::deserialize(&alice.serialize()).unwrap();
        let mut bob2 = RatchetState::deserialize(&bob.serialize()).unwrap();
        let mut alice2 = alice2;

        // Bob reloads and can still open the skipped a2; Alice reloads and keeps sending.
        assert_eq!(bob2.ratchet_decrypt(&h2, &c2).unwrap(), b"a2");
        let (h4, c4) = alice2.ratchet_encrypt(b"a4").unwrap();
        assert_eq!(bob2.ratchet_decrypt(&h4, &c4).unwrap(), b"a4");

        // Malformed input is rejected.
        assert!(RatchetState::deserialize(b"junk").is_none());
    }
```

- [ ] **Step 3: build, test, commit**
```bash
cd src-tauri && nice -n 10 cargo test --lib ratchet -- --test-threads=2
cd src-tauri && nice -n 10 cargo build --lib && nice -n 10 cargo clippy --lib -- -D warnings 2>&1 | tail -20
cd src-tauri && nice -n 10 cargo fmt
cd /Users/kingsonwu/programming/rust-src/mesh-talk
git add src-tauri/src/ratchet/state.rs src-tauri/src/ratchet/mod.rs
git commit -m "feat(ratchet): lossless serialize/deserialize of a live session"
git status | head -1
```

---

### Task 2: bound skipped keys (FIFO) + zeroize on drop

**Files:** Modify `src-tauri/src/ratchet/state.rs`; `src-tauri/Cargo.toml` (add `zeroize`).

- [ ] **Step 1: add the `zeroize` dependency**

In `src-tauri/Cargo.toml` `[dependencies]`, add (it's already in the lock tree, so no new compile cost):
```toml
zeroize = "1"
```

- [ ] **Step 2: FIFO-bounded skipped keys**

Add a const + an insertion-order queue to bound total skipped entries (review item 6). Add the field to `RatchetState`:
```rust
    skipped_order: std::collections::VecDeque<([u8; 32], u32)>,
```
Add a const near `MAX_SKIP`:
```rust
/// Hard cap on TOTAL buffered skipped message keys (across all chains/steps) — a
/// memory-DoS backstop. Oldest are evicted; an extremely-late message older than
/// this bound won't decrypt (acceptable, matches Signal's bounded buffer).
const MAX_SKIPPED_TOTAL: usize = 2000;
```
Initialize `skipped_order: VecDeque::new()` in `init_alice`, `init_bob`, AND `deserialize` (in `deserialize`, rebuild it from the loaded `skipped` keys: `wire.skipped.iter().map(|(p,n,_)| (*p,*n)).collect()` — capture before the `into_iter().collect()` for the map, or rebuild from the assembled map's keys).
In `skip_message_keys`, after `self.skipped.insert((dhr, self.nr), mk);` add the order push + eviction:
```rust
            self.skipped_order.push_back((dhr, self.nr));
            while self.skipped.len() > MAX_SKIPPED_TOTAL {
                if let Some(old) = self.skipped_order.pop_front() {
                    self.skipped.remove(&old);
                } else {
                    break;
                }
            }
```
(When `ratchet_decrypt` consumes a skipped key via `self.skipped.remove(...)`, leave the stale `skipped_order` entry; the eviction loop's `self.skipped.remove(&old)` is a harmless no-op for an already-consumed key, and the `len()` guard still bounds memory.)

- [ ] **Step 3: zeroize raw key material on drop**

Add `use zeroize::Zeroize;` and a `Drop` impl for `RatchetState`:
```rust
impl Drop for RatchetState {
    fn drop(&mut self) {
        self.rk.zeroize();
        if let Some(ck) = self.cks.as_mut() {
            ck.zeroize();
        }
        if let Some(ck) = self.ckr.as_mut() {
            ck.zeroize();
        }
        for mk in self.skipped.values_mut() {
            mk.zeroize();
        }
        // dhs_secret (StaticSecret) zeroizes itself on drop.
    }
}
```

- [ ] **Step 4: test — the skipped buffer is bounded**

Append in tests:
```rust
    #[test]
    fn skipped_buffer_is_bounded() {
        let (mut alice, mut bob) = pair();
        // Drive many small skipped-key steps across DH ratchets so the TOTAL would
        // exceed the cap without eviction. Each round: Alice sends N then bob jumps.
        for _ in 0..6 {
            // advance alice's chain with a fresh DH ratchet each round
            let (hb, cb) = {
                let (h1, c1) = alice.ratchet_encrypt(b"x").unwrap();
                bob.ratchet_decrypt(&h1, &c1).unwrap();
                bob.ratchet_encrypt(b"y").unwrap()
            };
            alice.ratchet_decrypt(&hb, &cb).unwrap();
            // Alice sends 500, bob skips to the last → 499 skipped keys this step.
            let mut last = None;
            for i in 0..500u32 {
                last = Some(alice.ratchet_encrypt(format!("m{i}").as_bytes()).unwrap());
            }
            let (h, c) = last.unwrap();
            bob.ratchet_decrypt(&h, &c).unwrap();
        }
        // Total buffered skipped keys never exceeds the cap.
        assert!(bob.skipped_len() <= MAX_SKIPPED_TOTAL);
    }
```
Add a test-visibility accessor on `RatchetState` (a `#[cfg(test)] pub fn skipped_len(&self) -> usize { self.skipped.len() }`, or a `pub(crate)` accessor).

- [ ] **Step 5: build, test, clippy, fmt, commit**
```bash
cd src-tauri && nice -n 10 cargo test --lib ratchet -- --test-threads=2
cd src-tauri && nice -n 10 cargo build --lib && nice -n 10 cargo clippy --lib -- -D warnings 2>&1 | tail -20
cd src-tauri && nice -n 10 cargo fmt
cd /Users/kingsonwu/programming/rust-src/mesh-talk
git add src-tauri/src/ratchet/state.rs src-tauri/Cargo.toml src-tauri/Cargo.lock
git commit -m "feat(ratchet): bound skipped-key buffer (FIFO) + zeroize key material on drop"
git status | head -1
```
Expected: all ratchet tests pass (serialization round-trip, bounded buffer, + the Plan-1 suite); clippy clean.

---

## Notes for the reviewer

- **Delivered:** lossless session serialization (so the next plan can persist it encrypted), a FIFO-bounded skipped-key buffer (closes the review's memory-DoS item), and zeroization of raw key arrays on drop (the review's hygiene item).
- **Reviewer checks:** serialize→deserialize preserves enough to (a) open a previously-skipped key and (b) continue sending/receiving (the round-trip test); the bound never exceeds `MAX_SKIPPED_TOTAL`; eviction is FIFO (oldest first) and a consumed-key stale entry is harmless; `Drop` zeroizes `rk`/`cks`/`ckr`/skipped MKs; `RatchetState` still derives no `Debug`/`Serialize` (only the private `RatchetWire` is `Serialize`, used solely inside `serialize`); the serialized blob is documented as secret (must be encrypted at rest by the caller).
- **Deferred to Plan 3 (Node integration):** the encrypted on-disk session store + received-plaintext store (using `storage::EncryptionKey` like `SentLog`); session establishment (X3DH bootstrap from identity DH); seal/open DM Message events via the ratchet; serve history from the plaintext stores; coexistence with the current DM box; loopback rig.
