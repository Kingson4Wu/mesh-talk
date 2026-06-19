# Post-Office Replication + Drain + Offline Rig Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Complete the offline-delivery slice: `send_dm` always replicates to the elected post office (and tolerates an offline recipient), a periodic drain pulls held DMs back from the post office, and a 3-process rig proves a DM sent while the recipient is offline is delivered after it returns.

**Architecture:** `Node::send_dm` keeps appending locally, then does best-effort direct delivery AND always replicates to `elected_post_office` (Plan A) — succeeding if either path lands. `Node::drain_from_post_office` dials the elected PO once and runs a sync round per known peer's DM conversation, surfacing anything new through the existing emit path. The binary spawns a periodic drain task on normal nodes. The headline `#[ignore]`d rig spawns a `--post-office` node + Alice + Bob; Bob exits, Alice messages him (direct fails → PO holds it), Bob returns and his drain prints the decrypted DM.

**Tech Stack:** Rust; the redesign `node` (Plan A's `node::postbox::elected_post_office`, `node::session`, `node::transport`, `node::conversation`), `discovery`, `postoffice`; `tokio`; `std::process` + `tempfile` for the rig. No new dependencies.

---

## Background the implementer needs

**This is Plan B (of 2) of the "offline post-office delivery" slice** (spec: `docs/superpowers/specs/2026-06-16-offline-post-office-delivery-design.md`). Plan A made the post office discoverable and runnable: a signed `post_office` announce flag, `Roster::post_offices`, `node::postbox::elected_post_office(roster) -> Option<PeerRecord>`, a relay serve loop, and a `--post-office` binary mode. THIS plan adds the sender/recipient logic that USES the elected PO, plus the integration rig.

**Exact APIs you build on (all implemented):**
```rust
// node::postbox (Plan A)
crate::node::postbox::elected_post_office(roster: &Roster) -> Option<PeerRecord>; // lowest-fingerprint PO, or None
crate::node::postbox::run_relay_accept_loop(identity: DeviceIdentity, listener: TcpListener, store: Arc<Mutex<PostOffice>>); // async
// node internals you are extending (src-tauri/src/node/node.rs)
//   Node { identity: DeviceIdentity, log: Mutex<EventLog>, roster: Arc<Mutex<Roster>>, incoming, emitted }
//   fn emit_new_messages(&self, conv: ConversationId)  // private; decrypts + sends ReceivedDm on the mpsc
//   fn now_millis() -> u64
crate::node::conversation::dm_conversation_id(a: &PublicIdentity, b: &PublicIdentity) -> ConversationId; // SYMMETRIC
crate::node::conversation::build_dm_event(...);
crate::node::session::{request_round, SessionError}; // request_round(&mut SecureChannel<IO>, &Mutex<S: SyncStore>, ConversationId) -> Result<ApplyReport, SessionError>
crate::node::transport::dial(addr: SocketAddr, identity: &DeviceIdentity, expected_peer: Option<&PublicIdentity>) -> Result<SecureChannel<TcpStream>, TransportError>;
// discovery
crate::discovery::roster::{PeerRecord, Roster}; // PeerRecord { public: PublicIdentity, addr: SocketAddr, name, post_office, last_seen }; Roster::peers() -> Vec<PeerRecord>; Roster::get/update
crate::discovery::announce::Announce::{new, new_post_office};
// postoffice
crate::postoffice::PostOffice::open(&Path, &str, DeviceIdentity) -> Result<PostOffice, LogError>; // PostOffice: SyncStore
// identity
DeviceIdentity::{generate, public, secret_bytes() -> ([u8;32],[u8;32]), from_secret_bytes(ed, x)};
```

**Key facts:**
- `dm_conversation_id` is SYMMETRIC: Alice's send uses `dm_conversation_id(alice, bob)`; Bob's drain uses `dm_conversation_id(bob, alice)` — the SAME id, so the PO holds Alice's event under the id Bob queries.
- `Node::new` consumes `identity` by value; in tests, seed rosters (which borrow `&DeviceIdentity`) BEFORE moving the identity into `Node::new`.
- A `request_round` returns once this side has SENT its Followup — the relay ingests it asynchronously on its own task, so a drain immediately after a replicate can race; tests retry the drain in a bounded loop.
- Locks are `std::sync::Mutex`; never hold a guard across `.await` (collect/clone under the lock, drop the guard, then dial/round).
- **The post office is slow to cold-start (~15-25s: two password-KDF opens).** The rig must POLL for the relay's startup line, never fixed-sleep a short interval.

**CPU/test discipline (MANDATORY — this machine has had CPU spikes):** never run a bare `cargo test`/`build`. Always `nice` + throttle + scope:
```bash
cd src-tauri && nice -n 10 cargo test --lib node::node -- --test-threads=2
cd src-tauri && nice -n 10 cargo build --bin mesh-talk-node
cd src-tauri && nice -n 10 cargo clippy --lib --bin mesh-talk-node -- -D warnings 2>&1 | tail -20
cd src-tauri && nice -n 10 cargo fmt
```
The pre-commit hook runs the full health check — let it run. The Task-3 rig is `#[ignore]`d (3 real processes incl. a slow PO) — run it explicitly with a GENEROUS timeout (up to ~180s).

**Conventions:** hand-written errors; `#[cfg(test)] mod tests`; match the existing `node.rs`/`two_node_cli.rs` style.

---

### Task 1: `send_dm` replication + `drain_from_post_office`

**Files:**
- Modify: `src-tauri/src/node/node.rs`

- [ ] **Step 1: Extend the imports**

In `src-tauri/src/node/node.rs`, change the roster import to add `PeerRecord`, and add the `elected_post_office` import:
```rust
use crate::discovery::roster::{PeerRecord, Roster, UserId};
```
and (add directly below the existing `use crate::node::conversation::...;` line):
```rust
use crate::node::postbox::elected_post_office;
```

- [ ] **Step 2: Replace `send_dm` and add the two helpers**

Replace the entire existing `send_dm` method with this version (it appends locally exactly as before, then does best-effort direct + always-replicate, with the new success semantics), and add the two private helpers immediately after it:
```rust
    /// Send a DM to `recipient` (a known peer): seal it, append the Message event
    /// locally, then deliver it. Delivery is best-effort DIRECT (the recipient may
    /// be offline) plus ALWAYS replicating to the elected post office (so an
    /// offline recipient can retrieve it later). Succeeds if EITHER the direct
    /// delivery or a post office accepted the event; errors only if the recipient
    /// is unreachable and no post office is available.
    pub async fn send_dm(&self, recipient: &str, text: &[u8]) -> Result<(), NodeError> {
        let peer = self
            .roster
            .lock()
            .expect("roster mutex not poisoned")
            .get(recipient)
            .cloned()
            .ok_or_else(|| NodeError::UnknownPeer(recipient.to_string()))?;

        let conv = dm_conversation_id(&self.identity.public(), &peer.public);
        let self_author = Author::from_ed25519(self.identity.public().ed25519_pub);

        {
            let mut log = self.log.lock().expect("log mutex not poisoned");
            let (parents, lamport) = log.prepare(&conv);
            let seq = log
                .version_vector(&conv)
                .get(&self_author)
                .copied()
                .unwrap_or(0)
                + 1;
            let event = build_dm_event(
                &self.identity,
                &peer.public,
                conv,
                seq,
                parents,
                lamport,
                now_millis(),
                text,
            )
            .map_err(NodeError::Seal)?;
            log.append(event).map_err(NodeError::Log)?;
        }

        // Best-effort direct delivery (the recipient may be offline) plus always
        // replicating to the elected post office (store-and-forward).
        let direct = self.deliver_direct(&peer, conv).await;
        let replicated = self.replicate_to_post_office(conv).await;
        // Either round may also have pulled events back to us.
        self.emit_new_messages(conv);

        match (direct, replicated) {
            (Ok(()), _) => Ok(()),               // delivered directly
            (Err(_), Ok(true)) => Ok(()),        // a post office holds it
            (Err(e), Ok(false)) => Err(NodeError::Session(e)), // offline peer, no PO
            (Err(_), Err(e)) => Err(NodeError::Session(e)),    // both paths failed
        }
    }

    /// Dial `peer` directly and run one sync round for `conv`. Best-effort: the
    /// peer may be offline, in which case the dial fails.
    async fn deliver_direct(
        &self,
        peer: &PeerRecord,
        conv: ConversationId,
    ) -> Result<(), SessionError> {
        let mut channel = dial(peer.addr, &self.identity, Some(&peer.public))
            .await
            .map_err(SessionError::Transport)?;
        request_round(&mut channel, &self.log, conv).await.map(|_| ())
    }

    /// Replicate `conv` to the elected post office, if one is known. Returns
    /// `Ok(true)` if a post office accepted a round, `Ok(false)` if no post office
    /// is known.
    async fn replicate_to_post_office(
        &self,
        conv: ConversationId,
    ) -> Result<bool, SessionError> {
        let po = {
            let roster = self.roster.lock().expect("roster mutex not poisoned");
            elected_post_office(&roster)
        };
        let Some(po) = po else {
            return Ok(false);
        };
        let mut channel = dial(po.addr, &self.identity, Some(&po.public))
            .await
            .map_err(SessionError::Transport)?;
        request_round(&mut channel, &self.log, conv).await?;
        Ok(true)
    }
```

- [ ] **Step 3: Add `drain_from_post_office`**

Add this public method to the `impl Node` block (place it right after `serve_connection`):
```rust
    /// Pull any DMs held for this node from the elected post office: dial it once,
    /// then run a sync round for each known peer's DM conversation, surfacing
    /// anything new. A no-op if no post office is known. Best-effort and
    /// fail-soft — a dial/round error just ends this drain; the next one retries.
    pub async fn drain_from_post_office(&self) {
        let (post_office, peers) = {
            let roster = self.roster.lock().expect("roster mutex not poisoned");
            (elected_post_office(&roster), roster.peers())
        };
        let Some(po) = post_office else {
            return;
        };
        let mut channel = match dial(po.addr, &self.identity, Some(&po.public)).await {
            Ok(c) => c,
            Err(_) => return,
        };
        for peer in peers {
            let conv = dm_conversation_id(&self.identity.public(), &peer.public);
            if request_round(&mut channel, &self.log, conv).await.is_err() {
                return; // channel broke; the next drain re-dials
            }
            self.emit_new_messages(conv);
        }
    }
```

- [ ] **Step 4: Add the in-process offline-delivery test (over loopback TCP)**

Append this test inside the existing `#[cfg(test)] mod tests` block in `node.rs`. Add these imports at the top of the `mod tests` block (next to the existing `use crate::discovery::announce::Announce;` etc.):
```rust
    use crate::node::postbox::run_relay_accept_loop;
    use crate::postoffice::PostOffice;
    use std::net::SocketAddr;
```
Then the test:
```rust
    #[tokio::test]
    async fn offline_dm_delivered_via_post_office_over_loopback() {
        // Alice sends Bob a DM while Bob is offline; a post office holds it; Bob
        // drains it and decrypts — all in-process over loopback TCP.
        let dir = tempfile::tempdir().unwrap();
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let po_id = DeviceIdentity::generate();
        let alice_uid = alice.public().user_id();
        let bob_uid = bob.public().user_id();

        // Post office relay on loopback. Keep extra identity copies (open() moves one).
        let po_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let po_addr = po_listener.local_addr().unwrap();
        let (po_ed, po_x) = po_id.secret_bytes();
        let po_transport = DeviceIdentity::from_secret_bytes(po_ed, po_x);
        let po_seed = DeviceIdentity::from_secret_bytes(po_ed, po_x);
        let po_store = Arc::new(Mutex::new(
            PostOffice::open(&dir.path().join("po.log"), "pw", po_id).unwrap(),
        ));
        tokio::spawn(run_relay_accept_loop(po_transport, po_listener, Arc::clone(&po_store)));

        // A closed port stands in for offline Bob — Alice's direct dial fails fast.
        let dead = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let bob_dead_addr = dead.local_addr().unwrap();
        drop(dead);

        // Seed a roster entry whose addr/keys/role we control (addr = (ip, port)).
        fn seed(
            roster: &Arc<Mutex<Roster>>,
            id: &DeviceIdentity,
            name: &str,
            addr: SocketAddr,
            post_office: bool,
            self_uid: &str,
        ) {
            let announce = if post_office {
                Announce::new_post_office(id, name, addr.port())
            } else {
                Announce::new(id, name, addr.port())
            };
            roster.lock().unwrap().update(&announce, addr.ip(), self_uid);
        }

        // Alice knows offline-Bob (dead addr) + the PO (real addr).
        let alice_roster = Arc::new(Mutex::new(Roster::default()));
        seed(&alice_roster, &bob, "Bob", bob_dead_addr, false, &alice_uid);
        seed(&alice_roster, &po_seed, "PO", po_addr, true, &alice_uid);
        // Bob knows Alice (real keys, for decryption) + the PO (to drain).
        let bob_roster = Arc::new(Mutex::new(Roster::default()));
        seed(&bob_roster, &alice, "Alice", "127.0.0.1:4000".parse().unwrap(), false, &bob_uid);
        seed(&bob_roster, &po_seed, "PO", po_addr, true, &bob_uid);

        let (alice_tx, _alice_rx) = mpsc::unbounded_channel();
        let (bob_tx, mut bob_rx) = mpsc::unbounded_channel();
        let alice_node = Node::new(alice, alice_roster, alice_tx);
        let bob_node = Node::new(bob, bob_roster, bob_tx);

        // Alice sends while Bob is offline: direct fails, PO replication succeeds.
        alice_node
            .send_dm(&bob_uid, b"held-hello")
            .await
            .expect("send_dm succeeds via the post office despite offline recipient");

        // Bob drains from the PO (retry to absorb the relay's async ingest).
        let mut received = None;
        for _ in 0..25 {
            bob_node.drain_from_post_office().await;
            if let Ok(dm) =
                tokio::time::timeout(Duration::from_millis(200), bob_rx.recv()).await
            {
                received = dm;
                break;
            }
        }
        let received = received.expect("Bob received the held DM from the post office");
        assert_eq!(received.from, alice_uid);
        assert_eq!(received.from_name, "Alice");
        assert_eq!(received.text, b"held-hello");
    }
```

- [ ] **Step 5: Run the node tests**
```bash
cd src-tauri && nice -n 10 cargo test --lib node::node -- --test-threads=2
```
Expected: PASS — the existing node tests plus `offline_dm_delivered_via_post_office_over_loopback`.

- [ ] **Step 6: fmt + clippy, then commit**
```bash
cd src-tauri && nice -n 10 cargo fmt && nice -n 10 cargo clippy --lib -- -D warnings 2>&1 | tail -20
cd /Users/kingsonwu/programming/rust-src/mesh-talk
git add src-tauri/src/node/node.rs
git commit -m "feat(node): replicate DMs to elected post office; drain held DMs

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 2: Periodic drain task on normal nodes

**Files:**
- Modify: `src-tauri/src/bin/mesh-talk-node.rs`

- [ ] **Step 1: Add the drain-interval constant**

In `src-tauri/src/bin/mesh-talk-node.rs`, add below the `DEFAULT_DISCOVERY_PORT` const:
```rust
/// How often a normal node drains held DMs from the elected post office.
const DRAIN_INTERVAL_SECS: u64 = 3;
```

- [ ] **Step 2: Spawn the drain task in the normal-node `main` path**

In `main`, after the spawned inbound-DM printer task and before the startup `emit(...)` line, add the periodic drain (it runs once immediately at startup, then every interval):
```rust
    // Periodically pull DMs held for us by the elected post office (delivered
    // while we were offline / unreachable). Runs once now, then every interval.
    {
        let node = Arc::clone(&node);
        tokio::spawn(async move {
            loop {
                node.drain_from_post_office().await;
                tokio::time::sleep(Duration::from_secs(DRAIN_INTERVAL_SECS)).await;
            }
        });
    }
```
(Do NOT add a drain task to the `run_post_office` relay path — a post office does not drain.)

- [ ] **Step 3: Build + clippy + bin tests**
```bash
cd src-tauri && nice -n 10 cargo build --bin mesh-talk-node
cd src-tauri && nice -n 10 cargo clippy --bin mesh-talk-node -- -D warnings 2>&1 | tail -20
cd src-tauri && nice -n 10 cargo test --bin mesh-talk-node -- --test-threads=2
```
Expected: build OK; clippy clean; the 5 resolver tests still pass.

- [ ] **Step 4: fmt + commit**
```bash
cd src-tauri && nice -n 10 cargo fmt
cd /Users/kingsonwu/programming/rust-src/mesh-talk
git add src-tauri/src/bin/mesh-talk-node.rs
git commit -m "feat(node-cli): periodic drain of held DMs from the post office

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 3: Three-process offline integration rig

**Files:**
- Create: `src-tauri/tests/post_office_offline.rs`

This is the headline acceptance test: a `--post-office` node + Alice + Bob; Bob exits; Alice DMs offline Bob (direct fails → PO holds it); Bob returns and his drain prints the decrypted DM. `#[ignore]`d (three real processes incl. a slow-starting post office + UDP broadcast).

- [ ] **Step 1: Create `src-tauri/tests/post_office_offline.rs`**

```rust
//! Three-process offline-delivery rig for the post-office slice: a `--post-office`
//! relay + Alice + Bob. Bob goes offline; Alice DMs him (direct fails, the relay
//! holds the sealed event); Bob returns and his drain pulls and decrypts it.
//! `#[ignore]`d — three real processes, a slow-starting post office (~15-25s cold
//! start: two KDFs), and UDP broadcast. Run explicitly:
//!   cd src-tauri && nice -n 10 cargo test --test post_office_offline -- --ignored --test-threads=2

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

/// A spawned `mesh-talk-node` (normal or `--post-office`) with a line view of stdout.
struct CliNode {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
}

impl CliNode {
    fn spawn(keystore: &std::path::Path, name: &str, discovery_port: u16, post_office: bool) -> CliNode {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_mesh-talk-node"));
        cmd.arg("--keystore")
            .arg(keystore)
            .arg("--password")
            .arg("pw")
            .arg("--name")
            .arg(name)
            .arg("--port")
            .arg("0")
            .arg("--discovery-port")
            .arg(discovery_port.to_string());
        if post_office {
            cmd.arg("--post-office");
        }
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn mesh-talk-node");
        let stdin = child.stdin.take().expect("child stdin");
        let stdout = child.stdout.take().expect("child stdout");
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                match line {
                    Ok(l) => {
                        if tx.send(l).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });
        CliNode { child, stdin, lines: rx }
    }

    fn node(keystore: &std::path::Path, name: &str, dp: u16) -> CliNode {
        Self::spawn(keystore, name, dp, false)
    }

    fn post_office(keystore: &std::path::Path, name: &str, dp: u16) -> CliNode {
        Self::spawn(keystore, name, dp, true)
    }

    fn send(&mut self, line: &str) {
        writeln!(self.stdin, "{line}").expect("write to child stdin");
        self.stdin.flush().expect("flush child stdin");
    }

    fn wait_for(&self, pred: impl Fn(&str) -> bool, timeout: Duration) -> Option<String> {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.checked_duration_since(Instant::now())?;
            match self.lines.recv_timeout(remaining) {
                Ok(l) => {
                    if pred(&l) {
                        return Some(l);
                    }
                }
                Err(_) => return None,
            }
        }
    }
}

impl Drop for CliNode {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Read the user_id from a startup line beginning with `prefix` (`node ` or
/// `post-office `): the token after the prefix.
fn read_user_id(node: &CliNode, prefix: &str, timeout: Duration) -> String {
    let line = node
        .wait_for(|l| l.starts_with(prefix), timeout)
        .unwrap_or_else(|| panic!("startup line starting with {prefix:?} within {timeout:?}"));
    line.split_whitespace()
        .nth(1)
        .expect("startup line has a user_id")
        .to_string()
}

/// Poll `/peers` until `want_uid` appears in `node`'s roster, or fail after 25s.
/// Paces the poll so a roster of non-matching lines can't busy-spin.
fn await_peer(node: &mut CliNode, want_uid: &str, who: &str) {
    let deadline = Instant::now() + Duration::from_secs(25);
    while Instant::now() < deadline {
        node.send("/peers");
        if node
            .wait_for(|l| l.contains(want_uid), Duration::from_secs(1))
            .is_some()
        {
            return;
        }
        std::thread::sleep(Duration::from_millis(300));
    }
    panic!("{who} never discovered {want_uid}");
}

#[test]
#[ignore = "three real processes incl. a slow post office + UDP broadcast; run with --ignored"]
fn offline_dm_delivered_via_post_office() {
    let dir = tempfile::tempdir().expect("tempdir");
    let dp = 47820;

    // Post office first (it is slow to cold-start: two KDFs). Wait for its line.
    let po = CliNode::post_office(&dir.path().join("po.keystore"), "Relay", dp);
    let po_uid = read_user_id(&po, "post-office ", Duration::from_secs(45));

    // Alice and Bob (normal nodes). Keep Bob's keystore path to restart him later.
    let mut alice = CliNode::node(&dir.path().join("alice.keystore"), "Alice", dp);
    let bob_keystore = dir.path().join("bob.keystore");
    let mut bob = CliNode::node(&bob_keystore, "Bob", dp);
    let alice_uid = read_user_id(&alice, "node ", Duration::from_secs(20));
    let bob_uid = read_user_id(&bob, "node ", Duration::from_secs(20));
    assert_ne!(alice_uid, bob_uid);

    // Everyone converges: Alice needs Bob (to address) + PO (to replicate); Bob
    // needs Alice (to decrypt later) + PO (to drain).
    await_peer(&mut alice, &bob_uid, "Alice→Bob");
    await_peer(&mut alice, &po_uid, "Alice→PO");
    await_peer(&mut bob, &alice_uid, "Bob→Alice");
    await_peer(&mut bob, &po_uid, "Bob→PO");

    // Bob goes offline.
    drop(bob);

    // Alice DMs offline Bob: direct dial fails, the event is replicated to the PO.
    alice.send(&format!("/msg {} held-hello", &bob_uid[..8]));
    std::thread::sleep(Duration::from_secs(3)); // let replication reach the PO

    // Bob comes back (same keystore → same identity); his drain pulls from the PO.
    let bob = CliNode::node(&bob_keystore, "Bob", dp);
    let _ = read_user_id(&bob, "node ", Duration::from_secs(20));
    let got = bob
        .wait_for(
            |l| l.starts_with(&format!("from {alice_uid}")) && l.contains("held-hello"),
            Duration::from_secs(30),
        )
        .expect("Bob received the held DM from the post office after coming back online");
    assert!(got.contains("(Alice)"), "expected sender name Alice, got: {got}");
}
```

- [ ] **Step 2: Build the binary, then run the rig explicitly (generous timeout)**
```bash
cd src-tauri && nice -n 10 cargo build --bin mesh-talk-node
cd src-tauri && nice -n 10 cargo test --test post_office_offline -- --ignored --test-threads=2
```
Expected: `test offline_dm_delivered_via_post_office ... ok` (1 passed). This run can take ~60-120s (PO cold start + discovery + restart). If it fails:
- at `read_user_id(... "post-office" ... 45s)`: the PO didn't finish its two KDFs — raise to 60s and retry once.
- at an `await_peer`: same-host UDP broadcast didn't converge (rare here) — retry once.
- at the final `wait_for`: capture which stage; do NOT just bump timeouts or weaken asserts — report the real cause.

- [ ] **Step 3: Confirm the rig is excluded from the normal sweep**
```bash
cd src-tauri && nice -n 10 cargo test --test post_office_offline -- --test-threads=2
```
Expected: `0 passed; 0 failed; 1 ignored`.

- [ ] **Step 4: Commit**
```bash
cd /Users/kingsonwu/programming/rust-src/mesh-talk
git add src-tauri/tests/post_office_offline.rs
git commit -m "test(node-cli): 3-process offline rig — DM held by PO reaches returning peer

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Notes for the reviewer

- **What this delivers (completing the slice):** `send_dm` now appends locally, attempts best-effort direct delivery, and ALWAYS replicates to the elected post office — succeeding if either lands (so messaging an offline peer succeeds once the PO holds the event). `drain_from_post_office` dials the elected PO once and pulls each known peer's DM conversation, surfacing new DMs through the existing decrypt/emit path. The binary drains every 3s on normal nodes. The `#[ignore]`d 3-process rig proves the canonical scenario: a DM sent while the recipient is offline is delivered (and decrypted) after the recipient returns.
- **Why the in-process test is strong:** `offline_dm_delivered_via_post_office_over_loopback` exercises the real path (seal → append → direct-dial-fails → replicate-to-PO over loopback TCP+Noise → drain → decrypt) deterministically and fast, with a closed port standing in for the offline recipient. The 3-process rig then validates it across real processes + discovery + a restart.
- **Correctness:** `dm_conversation_id` symmetry means the PO holds Alice's event under exactly the id Bob drains; the new `send_dm` success semantics are explicit in the `match`; no lock is held across `.await` (roster/PO are snapshotted under the guard, which is dropped before dialing); the drain reuses one PO connection for all conversations and fails soft.
- **Accepted limitations (from the slice spec):** offline delivery still requires the recipient to re-discover the SENDER for identity/decryption (the rig keeps Alice online when Bob returns); "sender also offline at delivery," identity persistence, and a PO identity directory are deferred. Single elected PO; one DM conversation per known peer per drain; a fresh dial per send/drain (no pooling); LAN broadcast only.
