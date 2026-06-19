# Device Linking + UI Implementation Plan (Multi-Device Plan 4)

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:subagent-driven-development. SECURITY-CRITICAL (transfers the account secret between devices). Checkbox steps; full code inline.

**Goal:** Let an existing authorized device provision a new device into the same account: the existing device shows a one-time **pairing code**; the new device connects over the existing LAN **Noise** secure channel, proves knowledge of the code, and receives the **account secret** + a **device certificate**. After linking (adopted on next login) the new device advertises under the shared account, so fan-out + self-sync (Plan 3) light up for real. Plus a minimal **"Link a device"** UI and account id surfacing.

**Architecture:** A pure `node::pairing` module: a 128-bit `PairingCode`, and a SHA-256 secret-prefix authenticator binding the code to BOTH device keys (so a code-proof can't be replayed for another device), with a constant-time check. The pairing exchange rides the existing `SecureChannel` (Noise, mutually device-authenticated): the new device (joiner) dials the existing device (linker) and sends a magic-framed `PairingRequest{ joiner, tag }`; `serve_connection` detects the pairing magic on the first frame and runs `serve_pairing`, which checks the code + that the request binds the authenticated channel peer, then replies `PairingResponse{ account_secret, account_ed25519_pub, cert }`. The account secret is confidential under Noise (encrypted to the joiner's pinned key) and authorized by the code.

**Tech Stack:** Rust; `sha2`, `rand`, `hex`, existing `transport`/`SecureChannel`, `identity::account`. Frontend: Vue 3 (`RedesignChatView.vue`).

## Global Constraints

- **CPU/test discipline:** `nice -n 10`; `-- --test-threads=2`; scope to touched modules/tests.
- **Branch:** `feat/redesign-phase0`. Full pre-commit health gate (whole suite) on every commit.
- **No secret in logs.** Constant-time compare for the authenticator. The code is single-use: linking mode clears it on success.
- **Backward compatible:** the pairing wire has its own magic; a non-pairing first frame is handled by the existing sync path unchanged.

---

### Task 1: Pure pairing crypto — code, authenticator, wire messages

**Files:**
- Create: `src-tauri/src/node/pairing.rs`
- Modify: `src-tauri/src/node/mod.rs` (register + re-export)

**Interfaces:**
- Produces: `PairingCode` (`generate`, `as_hex`, `from_hex`, `authenticator(linker_ed, joiner_ed) -> [u8;32]`, `verify(linker_ed, joiner_ed, tag) -> bool`); `PairingRequest { joiner: PublicIdentity, tag: [u8;32] }` + encode/decode; `PairingResponse { account_secret: [u8;32], account_ed25519_pub: [u8;32], cert: DeviceCertificate }` + encode/decode.

- [ ] **Step 1: write `node/pairing.rs`** (full content below)

```rust
//! Device-linking pairing crypto + wire (multi-device). An existing device (the
//! "linker") shows a one-time [`PairingCode`]; a new device (the "joiner") proves it
//! over the LAN Noise channel and receives the account secret. The authenticator is a
//! SHA-256 secret-prefix MAC over the code and BOTH device keys, so a captured proof
//! cannot be replayed to enrol a different device. Confidentiality of the transferred
//! secret is provided by the Noise channel (encrypted to the joiner's pinned key);
//! the code provides authorization (user consent carried out-of-band).

use crate::identity::account::DeviceCertificate;
use crate::identity::device::PublicIdentity;
use bincode::Options;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const PAIR_DOMAIN: &[u8] = b"mesh-talk-pairing-v1";
const REQ_MAGIC: &[u8] = b"MTPQ1";
const RESP_MAGIC: &[u8] = b"MTPS1";

/// A one-time, high-entropy linking code shown by the linker and entered on the joiner.
#[derive(Clone)]
pub struct PairingCode([u8; 16]);

impl PairingCode {
    /// A fresh random 128-bit code.
    pub fn generate() -> Self {
        use rand::RngCore;
        let mut bytes = [0u8; 16];
        rand::rngs::OsRng.fill_bytes(&mut bytes);
        PairingCode(bytes)
    }

    /// 32 lowercase hex chars (what the user reads / types).
    pub fn as_hex(&self) -> String {
        hex::encode(self.0)
    }

    /// Parse a code from its hex form (case-insensitive). `None` if malformed.
    pub fn from_hex(s: &str) -> Option<Self> {
        let bytes = hex::decode(s.trim()).ok()?;
        let arr: [u8; 16] = bytes.try_into().ok()?;
        Some(PairingCode(arr))
    }

    /// Authenticator = SHA-256(DOMAIN ‖ code ‖ linker_ed ‖ joiner_ed). Binding both
    /// device keys stops a captured tag being replayed to enrol another device.
    pub fn authenticator(&self, linker_ed: &[u8; 32], joiner_ed: &[u8; 32]) -> [u8; 32] {
        let mut h = Sha256::new();
        h.update(PAIR_DOMAIN);
        h.update(self.0);
        h.update(linker_ed);
        h.update(joiner_ed);
        h.finalize().into()
    }

    /// Constant-time check that `tag` is the expected authenticator.
    pub fn verify(&self, linker_ed: &[u8; 32], joiner_ed: &[u8; 32], tag: &[u8; 32]) -> bool {
        ct_eq(&self.authenticator(linker_ed, joiner_ed), tag)
    }
}

/// Constant-time 32-byte equality (no early return on first mismatch).
fn ct_eq(a: &[u8; 32], b: &[u8; 32]) -> bool {
    let mut diff = 0u8;
    for i in 0..32 {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}

/// Joiner → linker: "I hold the code; here is my device identity." Magic-framed so the
/// responder can distinguish it from a sync wire on the first frame of a connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairingRequest {
    pub joiner: PublicIdentity,
    pub tag: [u8; 32],
}

impl PairingRequest {
    pub fn encode(&self) -> Vec<u8> {
        frame(REQ_MAGIC, self)
    }
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        unframe(REQ_MAGIC, bytes)
    }
}

/// Linker → joiner: the account secret + the account public key + a certificate
/// binding the joiner's device key to the account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairingResponse {
    pub account_secret: [u8; 32],
    pub account_ed25519_pub: [u8; 32],
    pub cert: DeviceCertificate,
}

impl PairingResponse {
    pub fn encode(&self) -> Vec<u8> {
        frame(RESP_MAGIC, self)
    }
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        unframe(RESP_MAGIC, bytes)
    }
}

fn frame<T: Serialize>(magic: &[u8], v: &T) -> Vec<u8> {
    let mut out = Vec::with_capacity(magic.len() + 64);
    out.extend_from_slice(magic);
    out.extend_from_slice(
        &bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .serialize(v)
            .expect("pairing message serializes"),
    );
    out
}

fn unframe<T: for<'de> Deserialize<'de>>(magic: &[u8], bytes: &[u8]) -> Option<T> {
    let rest = bytes.strip_prefix(magic)?;
    bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .reject_trailing_bytes()
        .deserialize::<T>(rest)
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::account::Account;
    use crate::identity::device::DeviceIdentity;

    #[test]
    fn code_hex_round_trips() {
        let c = PairingCode::generate();
        assert_eq!(PairingCode::from_hex(&c.as_hex()).unwrap().0, c.0);
        assert!(PairingCode::from_hex("nothex").is_none());
        assert!(PairingCode::from_hex("ab").is_none()); // wrong length
    }

    #[test]
    fn authenticator_verifies_and_binds_both_keys() {
        let code = PairingCode::generate();
        let linker = DeviceIdentity::generate();
        let joiner = DeviceIdentity::generate();
        let l = linker.public().ed25519_pub;
        let j = joiner.public().ed25519_pub;
        let tag = code.authenticator(&l, &j);
        assert!(code.verify(&l, &j, &tag));
        // Wrong code, wrong joiner, wrong linker all fail.
        assert!(!PairingCode::generate().verify(&l, &j, &tag));
        let other = DeviceIdentity::generate().public().ed25519_pub;
        assert!(!code.verify(&l, &other, &tag));
        assert!(!code.verify(&other, &j, &tag));
    }

    #[test]
    fn request_and_response_round_trip() {
        let joiner = DeviceIdentity::generate();
        let code = PairingCode::generate();
        let req = PairingRequest {
            joiner: joiner.public(),
            tag: code.authenticator(&[1u8; 32], &joiner.public().ed25519_pub),
        };
        assert_eq!(PairingRequest::decode(&req.encode()), Some(req));
        assert!(PairingRequest::decode(b"not a pairing frame").is_none());

        let account = Account::generate();
        let cert = account.certify(&joiner.public().ed25519_pub);
        let resp = PairingResponse {
            account_secret: account.secret_bytes(),
            account_ed25519_pub: account.public().ed25519_pub,
            cert,
        };
        assert_eq!(PairingResponse::decode(&resp.encode()), Some(resp));
    }

    #[test]
    fn a_sync_like_frame_is_not_a_pairing_request() {
        // Bytes without the magic must not decode as a pairing request.
        assert!(PairingRequest::decode(&[0u8, 1, 2, 3, 4, 5]).is_none());
    }
}
```

- [ ] **Step 2: register (`node/mod.rs`)**

Add `pub mod pairing;` and `pub use pairing::{PairingCode, PairingRequest, PairingResponse};`.

- [ ] **Step 3: test, clippy, fmt, commit**
```bash
cd src-tauri && nice -n 10 cargo test --lib -- --test-threads=2 node::pairing
cd src-tauri && nice -n 10 cargo clippy --lib -- -D warnings 2>&1 | tail -20
cd src-tauri && nice -n 10 cargo fmt
cd /Users/kingsonwu/programming/rust-src/mesh-talk
git add src-tauri/src/node/pairing.rs src-tauri/src/node/mod.rs docs/superpowers/plans/2026-06-19-device-linking.md
git commit -m "feat(node): device-linking pairing crypto + wire (multi-device plan 4)"
```

---

### Task 2: The node pairing exchange (Node holds the Account; serve + initiate)

**Files:**
- Modify: `src-tauri/src/node/node.rs` (Account field; `open_with_account` takes `Account`; pending-code state; `start_linking`/`stop_linking`/`link_to_device`; `serve_pairing`; first-frame dispatch in `serve_connection`)
- Modify: `src-tauri/src/node/session.rs` (extract `serve_wire_bytes` so an already-read frame can be served)
- Modify: `src-tauri/src/node/runtime.rs` + `src-tauri/src/bin/mesh-talk-node.rs` (pass `Account` instead of `account_id`)

**Interfaces:**
- Consumes: `PairingCode/Request/Response` (Task 1); `Account`; `SecureChannel::{send,recv,peer_identity}`; `dial`.
- Produces: `Node::open_with_account(identity, account: Account, ...)`; `Node::{start_linking()-> String, stop_linking(), link_to_device(addr, peer_public, code_hex) -> Result<LinkedAccount, NodeError>}`; `LinkedAccount { secret: [u8;32], account_id: String }`.

- [ ] **Step 1: `session.rs` — extract `serve_wire_bytes`**

Refactor `serve_one` so the post-`recv` body becomes a public helper that serves an already-read frame (lets `serve_connection` peek the first frame and still serve it as sync):
```rust
/// Serve one already-read sync wire frame (the body of [`serve_one`] after the recv).
pub async fn serve_wire_bytes<S, IO>(
    channel: &mut SecureChannel<IO>,
    store: &Mutex<S>,
    bytes: &[u8],
) -> Result<Served, SessionError>
where
    S: SyncStore,
    IO: AsyncRead + AsyncWrite + Unpin,
{
    match decode(bytes)? {
        SyncWire::Request(request) => {
            let conversation = request.conversation;
            let response = {
                let store = store.lock().expect("store mutex not poisoned");
                handle_request_bounded(&*store, &request, MAX_PLAINTEXT)
            };
            channel
                .send(&encode(&SyncWire::Response(response))?)
                .await?;
            Ok(Served::Handled(conversation))
        }
        SyncWire::Followup(followup) => {
            let conversation = followup.conversation;
            {
                let mut store = store.lock().expect("store mutex not poisoned");
                handle_followup(&mut *store, &followup);
            }
            Ok(Served::Handled(conversation))
        }
        SyncWire::Response(_) => Err(SessionError::UnexpectedMessage),
    }
}
```
Then make `serve_one` delegate:
```rust
pub async fn serve_one<S, IO>(
    channel: &mut SecureChannel<IO>,
    store: &Mutex<S>,
) -> Result<Served, SessionError>
where
    S: SyncStore,
    IO: AsyncRead + AsyncWrite + Unpin,
{
    let bytes = match channel.recv().await {
        Ok(b) => b,
        Err(_) => return Ok(Served::Closed),
    };
    serve_wire_bytes(channel, store, &bytes).await
}
```

- [ ] **Step 2: `node.rs` — Account field + `open_with_account(Account)` + pending code**

Change the `account_id: String` field to `account: Account` and add a pending-code holder. (Imports: `use crate::identity::account::Account;`, `use crate::node::pairing::{PairingCode, PairingRequest, PairingResponse};`, `use crate::node::session::serve_wire_bytes;`, `use crate::node::transport::dial;` — `dial` already imported.)
```rust
    account: Account,
    /// One-time pairing code while in "link a device" mode (linker side). Set by
    /// [`Node::start_linking`], cleared on success or [`Node::stop_linking`].
    pending_link: Mutex<Option<PairingCode>>,
```
`open` builds a deterministic per-device account from the device's ed25519 secret (each device = its own account until linked):
```rust
    pub fn open(/* unchanged params */) -> Result<Arc<Self>, LogError> {
        let account = crate::identity::account::Account::from_secret_bytes(identity.secret_bytes().0);
        Self::open_with_account(identity, account, roster, incoming, channel_incoming, file_incoming, log_path, sent_path, password)
    }
```
`open_with_account` takes `account: Account` (2nd param) instead of `account_id: String`; in the `Node { .. }` literal set `account,` and `pending_link: Mutex::new(None),`. Everywhere the code reads `self.account_id` (Plan 3: `send_to_account`, `emit_new_messages`, `account_history`), replace with `self.account.account_id()` (bind to a local `let my_account = self.account.account_id();` once per fn). Update the accessor:
```rust
    pub fn account_id(&self) -> String {
        self.account.account_id()
    }
```
(Note: accessor now returns `String`; update its two callers in `runtime.rs`/`redesign_commands.rs` if they relied on `&str` — `.to_string()` already applied there, and `rt.account_id()` in runtime returns `&str`; keep runtime's accessor returning `String` too — see Step 5.)

- [ ] **Step 3: `node.rs` — linking methods + `serve_pairing`**

```rust
    /// Material a newly-linked device receives.
    pub struct LinkedAccount { /* defined at module level below */ }

    /// Enter "link a device" mode: generate a one-time code to display. The next
    /// valid pairing request from a device proving this code is served the account.
    pub fn start_linking(&self) -> String {
        let code = PairingCode::generate();
        let hex = code.as_hex();
        *self.pending_link.lock().expect("pending_link mutex") = Some(code);
        hex
    }

    /// Leave linking mode (clear any pending code).
    pub fn stop_linking(&self) {
        *self.pending_link.lock().expect("pending_link mutex") = None;
    }

    /// Joiner side: dial `addr` (the linker, pinned to `peer_public`), prove `code_hex`,
    /// and receive the account secret + a certificate for this device.
    pub async fn link_to_device(
        &self,
        addr: std::net::SocketAddr,
        peer_public: &PublicIdentity,
        code_hex: &str,
    ) -> Result<LinkedAccount, NodeError> {
        let code = PairingCode::from_hex(code_hex)
            .ok_or_else(|| NodeError::Channel("invalid pairing code".into()))?;
        let mut channel = dial(addr, &self.identity, Some(peer_public))
            .await
            .map_err(|e| NodeError::Session(SessionError::Transport(e)))?;
        let tag = code.authenticator(&peer_public.ed25519_pub, &self.identity.public().ed25519_pub);
        let req = PairingRequest { joiner: self.identity.public(), tag };
        channel
            .send(&req.encode())
            .await
            .map_err(|e| NodeError::Session(SessionError::Transport(e)))?;
        let resp_bytes = channel
            .recv()
            .await
            .map_err(|e| NodeError::Session(SessionError::Transport(e)))?;
        let resp = PairingResponse::decode(&resp_bytes)
            .ok_or_else(|| NodeError::Channel("pairing rejected".into()))?;
        // Verify the returned cert really binds THIS device to THAT account.
        if !resp.cert.verify()
            || resp.cert.device_ed25519_pub != self.identity.public().ed25519_pub
            || resp.cert.account_ed25519_pub != resp.account_ed25519_pub
        {
            return Err(NodeError::Channel("pairing cert invalid".into()));
        }
        let account = crate::identity::account::Account::from_secret_bytes(resp.account_secret);
        Ok(LinkedAccount { secret: resp.account_secret, account_id: account.account_id() })
    }

    /// Linker side: handle a pairing request on an inbound (authenticated) channel.
    /// Releases the account secret only if a code is pending AND the request both
    /// proves it and binds the authenticated channel peer. Clears the code on success.
    async fn serve_pairing(&self, channel: &mut SecureChannel<TcpStream>, req: PairingRequest) {
        // Bind the proof to the Noise-authenticated peer.
        if req.joiner.ed25519_pub != channel.peer_identity().ed25519_pub {
            return;
        }
        let code = { self.pending_link.lock().expect("pending_link mutex").clone() };
        let Some(code) = code else { return };
        let my_ed = self.identity.public().ed25519_pub;
        if !code.verify(&my_ed, &req.joiner.ed25519_pub, &req.tag) {
            return;
        }
        let cert = self.account.certify(&req.joiner.ed25519_pub);
        let resp = PairingResponse {
            account_secret: self.account.secret_bytes(),
            account_ed25519_pub: self.account.public().ed25519_pub,
            cert,
        };
        if channel.send(&resp.encode()).await.is_ok() {
            self.stop_linking(); // single-use
        }
    }
```
Add the `LinkedAccount` struct at module level (near `ReceivedDm`):
```rust
/// Account material a device receives when it links to an existing device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkedAccount {
    pub secret: [u8; 32],
    pub account_id: String,
}
```
(Remove the inline `pub struct LinkedAccount {}` placeholder shown in the methods block above — it lives at module level, not inside `impl`.)

- [ ] **Step 4: `node.rs` — dispatch pairing on the first frame (`serve_connection`)**

Replace `serve_connection`:
```rust
    pub async fn serve_connection(&self, mut channel: SecureChannel<TcpStream>) {
        // Peek the first frame: a pairing request gets the linking handler; anything
        // else is a sync wire and is served normally (then the loop continues).
        let first = match channel.recv().await {
            Ok(b) => b,
            Err(_) => return,
        };
        if let Some(req) = PairingRequest::decode(&first) {
            self.serve_pairing(&mut channel, req).await;
            return;
        }
        match serve_wire_bytes(&mut channel, &self.log, &first).await {
            Ok(Served::Handled(conv)) => {
                self.emit_new_messages(conv);
                self.process_channel(conv);
                self.process_file_events(conv);
            }
            _ => return,
        }
        while let Ok(Served::Handled(conv)) = serve_one(&mut channel, &self.log).await {
            self.emit_new_messages(conv);
            self.process_channel(conv);
            self.process_file_events(conv);
        }
    }
```

- [ ] **Step 5: update `open_with_account` call sites (runtime + bin + Plan 3 tests)**

`runtime.rs`: `Node::open_with_account(identity, account, ...)` — pass the loaded `Account` (move it; derive `account_id` BEFORE the move: it already does `let account_id = account.account_id();` from Plan 2, so keep that line ABOVE the open call, then move `account`). The runtime's announce is built from `&account` earlier — reorder so the announce is built before the node consumes `account`, OR clone via `Account::from_secret_bytes(account.secret_bytes())` for the node. Cleanest: build the announce first (already does), then `Node::open_with_account(identity, account, ...)`.
`bin/mesh-talk-node.rs`: same — the announce is built before; pass the `account` into `open_with_account` (move). The post-office path doesn't open a Node.
Plan 3 tests in `node.rs` (`send_to_account_*`): they call `Node::open_with_account(dev, acct.account_id(), ...)`. Change to pass an `Account`: capture `let acct_id = acct.account_id();` first, then pass `acct` (move). Where the SAME account is needed for two nodes (self-sync test), construct each node's account from the shared secret: `Account::from_secret_bytes(secret)`.

- [ ] **Step 6: integration tests (append to `node.rs` tests)**
```rust
    #[tokio::test]
    async fn linking_transfers_the_account_secret_with_a_valid_code() {
        use crate::identity::account::Account;
        let linker_id = DeviceIdentity::generate();
        let linker_account = Account::generate();
        let joiner_id = DeviceIdentity::generate();
        let dir = tempfile::tempdir().unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let linker_addr = listener.local_addr().unwrap();

        let (d, _dr) = mpsc::unbounded_channel();
        let (c, _cr) = mpsc::unbounded_channel();
        let (f, _fr) = mpsc::unbounded_channel();
        let linker_public = linker_id.public();
        let linker = Node::open_with_account(
            linker_id,
            Account::from_secret_bytes(linker_account.secret_bytes()),
            Arc::new(Mutex::new(Roster::default())),
            d, c, f,
            &dir.path().join("l.log"), &dir.path().join("l-sent.log"), "pw",
        ).unwrap();

        let (d2, _d2r) = mpsc::unbounded_channel();
        let (c2, _c2r) = mpsc::unbounded_channel();
        let (f2, _f2r) = mpsc::unbounded_channel();
        let joiner = Node::open_with_account(
            joiner_id,
            Account::generate(), // its own throwaway account before linking
            Arc::new(Mutex::new(Roster::default())),
            d2, c2, f2,
            &dir.path().join("j.log"), &dir.path().join("j-sent.log"), "pw",
        ).unwrap();

        let code = linker.start_linking();
        tokio::spawn(Arc::clone(&linker).run_accept_loop(listener));

        let linked = joiner
            .link_to_device(linker_addr, &linker_public, &code)
            .await
            .expect("linking succeeds");
        assert_eq!(linked.secret, linker_account.secret_bytes());
        assert_eq!(linked.account_id, linker_account.account_id());
        // Code is single-use: a second attempt is refused.
        let again = joiner.link_to_device(linker_addr, &linker_public, &code).await;
        assert!(again.is_err(), "code consumed after first successful link");
    }

    #[tokio::test]
    async fn linking_with_a_wrong_code_is_refused() {
        use crate::identity::account::Account;
        let linker_id = DeviceIdentity::generate();
        let joiner_id = DeviceIdentity::generate();
        let dir = tempfile::tempdir().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let linker_addr = listener.local_addr().unwrap();
        let linker_public = linker_id.public();

        let (d, _dr) = mpsc::unbounded_channel();
        let (c, _cr) = mpsc::unbounded_channel();
        let (f, _fr) = mpsc::unbounded_channel();
        let linker = Node::open_with_account(
            linker_id, Account::generate(),
            Arc::new(Mutex::new(Roster::default())),
            d, c, f, &dir.path().join("l.log"), &dir.path().join("l-sent.log"), "pw",
        ).unwrap();
        let (d2, _d2r) = mpsc::unbounded_channel();
        let (c2, _c2r) = mpsc::unbounded_channel();
        let (f2, _f2r) = mpsc::unbounded_channel();
        let joiner = Node::open_with_account(
            joiner_id, Account::generate(),
            Arc::new(Mutex::new(Roster::default())),
            d2, c2, f2, &dir.path().join("j.log"), &dir.path().join("j-sent.log"), "pw",
        ).unwrap();

        let _real = linker.start_linking();
        tokio::spawn(Arc::clone(&linker).run_accept_loop(listener));
        let wrong = PairingCode::generate().as_hex();
        let res = joiner.link_to_device(linker_addr, &linker_public, &wrong).await;
        assert!(res.is_err(), "a wrong code must not yield the account secret");
    }
```
(Add `use crate::node::pairing::PairingCode;` inside the test that needs it, or fully-qualify.)

- [ ] **Step 7: build, test, clippy, fmt, commit**
```bash
cd src-tauri && nice -n 10 cargo build --lib --bins 2>&1 | tail -15
cd src-tauri && nice -n 10 cargo test --lib -- --test-threads=2 node::node::tests::linking node::node::tests::send_to_account
cd src-tauri && nice -n 10 cargo clippy --lib --bins -- -D warnings 2>&1 | tail -20
cd src-tauri && nice -n 10 cargo fmt
cd /Users/kingsonwu/programming/rust-src/mesh-talk
git add -A && git commit -m "feat(node): device-linking exchange over the Noise channel (multi-device plan 4)"
```

---

### Task 3: Runtime + IPC for linking and account grouping

**Files:**
- Modify: `src-tauri/src/node/runtime.rs` (store account path; `start_linking`/`stop_linking`/`link_device`; `account_id` returns `String`)
- Modify: `src-tauri/src/redesign_commands.rs` (IPC commands + an account-grouped peer list)
- Modify: `src-tauri/src/lib.rs` (register commands)

**Interfaces:**
- Produces: `RedesignRuntime::{start_linking()->String, stop_linking(), link_device(peer_user_id, code)->Result<String,NodeError>}` (returns adopted account_id, persisted to disk); IPC `redesign_start_linking`, `redesign_stop_linking`, `redesign_link_device`, `redesign_list_accounts`.

- [ ] **Step 1: runtime (`runtime.rs`)**

Store the account keystore path so a link can persist the adopted secret. Add field `account_path: std::path::PathBuf` to `RedesignRuntime`, set in `start` to `dir.join("account.keystore")`. Add:
```rust
    /// Enter "link a device" mode; returns the one-time code to display.
    pub fn start_linking(&self) -> String {
        self.node.start_linking()
    }
    pub fn stop_linking(&self) {
        self.node.stop_linking()
    }

    /// Link THIS device to an existing device `peer_user_id` using `code`. On success
    /// persists the adopted account secret to disk (effective next login) and returns
    /// the adopted account id.
    pub async fn link_device(&self, peer_user_id: &str, code: &str) -> Result<String, NodeError> {
        let peer = {
            let roster = self.roster.lock().expect("roster mutex not poisoned");
            roster
                .get(peer_user_id)
                .cloned()
                .ok_or_else(|| NodeError::UnknownPeer(peer_user_id.to_string()))?
        };
        let linked = self
            .node
            .link_to_device(peer.addr, &peer.public, code)
            .await?;
        let account = crate::identity::account::Account::from_secret_bytes(linked.secret);
        crate::identity::account_keystore::save(&self.account_path, &self.password, &account)
            .map_err(|e| NodeError::Channel(format!("persist linked account: {e}")))?;
        Ok(linked.account_id)
    }
```
This needs the password — store `password: String` on `RedesignRuntime` too (set in `start`). (If storing the password in memory is undesirable, persist the account during `start` is not possible; storing it for the session mirrors how the node already holds derived keys in memory.)

- [ ] **Step 2: IPC (`redesign_commands.rs`)**
```rust
#[tauri::command]
pub async fn redesign_start_linking(
    state: tauri::State<'_, RedesignState>,
) -> Result<String, String> {
    let guard = state.0.lock().await;
    let rt = guard.as_ref().ok_or_else(|| NOT_STARTED.to_string())?;
    Ok(rt.start_linking())
}

#[tauri::command]
pub async fn redesign_stop_linking(state: tauri::State<'_, RedesignState>) -> Result<(), String> {
    let guard = state.0.lock().await;
    let rt = guard.as_ref().ok_or_else(|| NOT_STARTED.to_string())?;
    rt.stop_linking();
    Ok(())
}

#[tauri::command]
pub async fn redesign_link_device(
    state: tauri::State<'_, RedesignState>,
    peer: String,
    code: String,
) -> Result<String, String> {
    let rt_handle = {
        let guard = state.0.lock().await;
        // We need the runtime across an await; clone the node handle + resolve peer here.
        let rt = guard.as_ref().ok_or_else(|| NOT_STARTED.to_string())?;
        rt.link_device(&peer, &code).await.map_err(|e| e.to_string())
    };
    rt_handle
}

/// An account (group of devices) as shown in the redesign UI.
#[derive(Serialize)]
pub struct AccountInfo {
    pub account_id: String,
    pub device_count: usize,
    pub names: Vec<String>,
}

#[tauri::command]
pub async fn redesign_list_accounts(
    state: tauri::State<'_, RedesignState>,
) -> Result<Vec<AccountInfo>, String> {
    use std::collections::BTreeMap;
    let guard = state.0.lock().await;
    let rt = guard.as_ref().ok_or_else(|| NOT_STARTED.to_string())?;
    let mut by_account: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for p in rt.peers() {
        if let Some(acct) = p.account_id {
            by_account.entry(acct).or_default().push(p.name);
        }
    }
    Ok(by_account
        .into_iter()
        .map(|(account_id, names)| AccountInfo {
            device_count: names.len(),
            names,
            account_id,
        })
        .collect())
}
```
(`PeerRecord.account_id` is `Option<String>` from Plan 2; `rt.peers()` returns `Vec<PeerRecord>`.)

- [ ] **Step 3: register (`lib.rs`)** — add `redesign_start_linking`, `redesign_stop_linking`, `redesign_link_device`, `redesign_list_accounts` to the handler list.

- [ ] **Step 4: build, clippy, fmt, commit**
```bash
cd src-tauri && nice -n 10 cargo build --lib --bins 2>&1 | tail -15
cd src-tauri && nice -n 10 cargo clippy --lib --bins -- -D warnings 2>&1 | tail -20
cd src-tauri && nice -n 10 cargo fmt
cd /Users/kingsonwu/programming/rust-src/mesh-talk
git add -A && git commit -m "feat(ipc): linking + account-grouping commands for the frontend (multi-device plan 4)"
```

---

### Task 4: "Link a device" UI + account surfacing

**Files:**
- Modify: `frontend/src/services/api.js` (add the new redesign calls)
- Modify: `frontend/src/views/redesign/RedesignChatView.vue` (a "Link a device" panel + show account id + accounts list)

**Interfaces:** consumes the Task 3 IPC commands.

- [ ] **Step 1: API bindings (`api.js`)**

In the `redesignAPI` object add:
```javascript
  accountId: () => invoke("redesign_account_id"),
  startLinking: () => invoke("redesign_start_linking"),
  stopLinking: () => invoke("redesign_stop_linking"),
  linkDevice: (peer, code) => invoke("redesign_link_device", { peer, code }),
  listAccounts: () => invoke("redesign_list_accounts"),
  sendToAccount: (account, text, replyTo = null) =>
    invoke("redesign_send_to_account", { account, text, replyTo }),
  accountHistory: (account, limit) => invoke("redesign_account_history", { account, limit }),
```

- [ ] **Step 2: a "Link a device" panel (`RedesignChatView.vue`)**

Add reactive state (in `<script setup>`): `const accountId = ref(""); const linkCode = ref(""); const linkOpen = ref(false); const joinPeer = ref(""); const joinCode = ref(""); const linkMsg = ref("");`. Load the account id in `onMounted` after `myId`:
```javascript
  try { accountId.value = await API.redesign.accountId(); } catch (_e) {}
```
Add methods:
```javascript
async function showLinkCode() {
  try { linkCode.value = await API.redesign.startLinking(); }
  catch (e) { linkMsg.value = String(e); }
}
async function doLink() {
  linkMsg.value = "";
  try {
    const adopted = await API.redesign.linkDevice(joinPeer.value.trim(), joinCode.value.trim());
    linkMsg.value = `Linked! Adopted account ${adopted.slice(0, 8)}… — restart the app to use it.`;
    joinCode.value = "";
  } catch (e) { linkMsg.value = `Link failed: ${e}`; }
}
```
Add a panel to the template (e.g. a collapsible block near the header `me` line). A minimal version:
```vue
<div class="link-panel">
  <button class="icon" @click="linkOpen = !linkOpen" title="Link a device">🔗</button>
  <div v-if="linkOpen" class="link-body">
    <p>Account: <code>{{ accountId ? accountId.slice(0, 8) + "…" : "—" }}</code></p>
    <div class="link-existing">
      <button @click="showLinkCode">Show pairing code (this device)</button>
      <code v-if="linkCode" class="pair-code">{{ linkCode }}</code>
    </div>
    <div class="link-new">
      <p>Have a code from your other device?</p>
      <select v-model="joinPeer">
        <option value="">Pick the device…</option>
        <option v-for="p in peers" :key="p.user_id" :value="p.user_id">
          {{ p.name || "(unnamed)" }} ({{ p.user_id.slice(0, 8) }})
        </option>
      </select>
      <input v-model="joinCode" placeholder="pairing code" />
      <button :disabled="!joinPeer || !joinCode.trim()" @click="doLink">Link this device</button>
    </div>
    <p v-if="linkMsg" class="link-msg">{{ linkMsg }}</p>
  </div>
</div>
```
Scope minimal CSS in the component's `<style>` (mirror existing class styling; functional, not pixel-perfect).

- [ ] **Step 3: verify frontend build + lint, fmt, commit**
```bash
cd /Users/kingsonwu/programming/rust-src/mesh-talk
# The pre-commit gate runs eslint + prettier + frontend build; rely on it, or pre-check:
( cd frontend && npm run build 2>&1 | tail -15 )
git add -A && git commit -m "feat(ui): link-a-device panel + account id surfacing in /redesign (multi-device plan 4)"
```

---

## Notes for the reviewer

- **Delivered:** device linking. An existing device shows a one-time code; a new device proves it over the mutually-authenticated Noise channel and receives the account secret + a device certificate. The code is a SHA-256 secret-prefix MAC bound to BOTH device keys, checked in constant time; the request is bound to the Noise-authenticated peer; the code is single-use. The UI gains a link panel + account-id display, and the backend exposes account-grouped peers.
- **Reviewer checks:**
  - The account secret only leaves the linker when (a) a code is pending, (b) the request's joiner key equals the authenticated channel peer, and (c) the authenticator verifies — see the wrong-code test. Single-use is asserted (second attempt fails).
  - The joiner validates the returned cert binds its own device key to the returned account key before adopting.
  - Pairing detection is by magic on the first frame; the sync path is unchanged for non-pairing connections (existing sync tests still pass).
  - Adopting the new account is effective on next login (the secret is persisted to `account.keystore`); hot-swapping a running Node's account is intentionally out of scope.
- **Multi-device now end-to-end:** after two devices link, they share an account; Plan 3's `send_to_account` fan-out + self-sync operate across them, and account history converges on each.
- **Deferred / v1 simplifications:** any linked device holds the account secret and can link/sign further devices (no primary-only model); re-keying/revoking a lost device; cross-device history backfill (a freshly-linked device sees messages from the link point forward); switching the *entire* conversation UI from device- to account-addressing (the link panel + account list are added; full conversation re-keying to accounts can follow once dogfooded).
