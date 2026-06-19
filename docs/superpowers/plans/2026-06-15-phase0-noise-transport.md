# Noise Secure Transport Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add an authenticated, end-to-end-encrypted peer channel over TCP — a Noise XX handshake keyed by the device's X25519 identity key, bound to the Ed25519 identity, replacing today's plaintext newline-JSON transport.

**Architecture:** A self-contained `transport` module with four layers, each independently testable: (1) `handshake` wraps the `snow` Noise XX state machine and produces a transport `Session`; (2) `session` encrypts/decrypts individual messages in Noise transport mode; (3) `auth` binds the Noise X25519 static key to the long-term Ed25519 identity via a signature over the handshake hash; (4) `channel` is the async `SecureChannel<S>` that drives handshake → identity-auth → length-prefixed framed `send`/`recv` over any `AsyncRead + AsyncWrite` stream (in-memory duplex for unit tests, real `TcpStream` for integration). This phase delivers the transport primitive in isolation; wiring it into `node_service`/discovery is a later step.

**Tech Stack:** Rust, `snow` 0.9 (pure-Rust Noise, `Noise_XX_25519_ChaChaPoly_BLAKE2s`), `tokio` (async I/O), the existing `identity::DeviceIdentity` (Ed25519 + X25519), `bincode`/`serde` for the auth payload.

---

## Background the implementer needs

**Identity API (already built, `src-tauri/src/identity/device.rs`):**

```rust
pub struct PublicIdentity { pub ed25519_pub: [u8; 32], pub x25519_pub: [u8; 32] }
impl PublicIdentity {
    pub fn user_id(&self) -> String;                    // hex fingerprint of ed25519_pub
    pub fn user_id_from(ed25519_pub: &[u8; 32]) -> String;
}
// PublicIdentity derives Clone, Debug, PartialEq, Eq, Serialize, Deserialize.

pub struct DeviceIdentity { /* signing_key + dh_secret, private */ }
impl DeviceIdentity {
    pub fn generate() -> Self;
    pub fn from_secret_bytes(ed25519_secret: [u8;32], x25519_secret: [u8;32]) -> Self;
    pub fn secret_bytes(&self) -> ([u8;32], [u8;32]);   // (ed25519_secret, x25519_secret)
    pub fn public(&self) -> PublicIdentity;
    pub fn user_id(&self) -> String;
    pub fn sign(&self, message: &[u8]) -> [u8; 64];
    pub fn verify(ed25519_pub: &[u8;32], message: &[u8], signature: &[u8;64]) -> bool; // associated fn
}
```

The **Noise static key is the device's X25519 key**: `identity.secret_bytes().1` is the 32-byte X25519 secret; `identity.public().x25519_pub` is its public half. The handshake authenticates possession of that X25519 key; the `auth` step then proves the X25519 key belongs to the device's Ed25519 identity (so `user_id` is meaningful over the channel).

**Why XX, and why an extra auth step.** Noise XX gives mutual authentication of the two **X25519** static keys without either side needing the other's key in advance (good for first contact). But our identity/`user_id` is anchored on the **Ed25519** key. So after the Noise handshake we run a short mutual auth exchange *inside the encrypted channel*: each side sends its `PublicIdentity` plus an Ed25519 signature over `AUTH_DOMAIN || handshake_hash`. The receiver checks (a) the signature verifies against the sender's `ed25519_pub`, and (b) the sender's advertised `x25519_pub` equals the X25519 static key Noise actually authenticated (`get_remote_static`). Signing the per-session handshake hash binds the signature to *this* session, preventing replay.

**CPU/test discipline (mandatory — this machine has had CPU spikes):** never run a bare full `cargo test`. Always `nice` + throttle threads, and scope to the module:

```bash
cd src-tauri && nice -n 10 cargo test --lib transport:: -- --test-threads=2
```

For fmt/clippy: `nice -n 10 cargo fmt` and `nice -n 10 cargo clippy --lib -- -D warnings`. The repo's pre-commit hook also runs full health checks (fmt/clippy/test/build) on commit — let it run.

**Conventions to match:** look at `src-tauri/src/identity/keystore.rs` for error/style conventions (hand-written error enums, no `thiserror`; tests use `tempfile`, `#[test]`/`#[tokio::test]`, `.expect("...")` in test code is fine). The crate has no `thiserror` dependency — write manual `Display`/`Error`/`From` impls.

---

### Task 1: Add `snow` dependency and the `transport` module skeleton

**Files:**
- Modify: `src-tauri/Cargo.toml` (`[dependencies]`)
- Create: `src-tauri/src/transport/mod.rs`
- Modify: `src-tauri/src/lib.rs` (module declarations)

- [ ] **Step 1: Add the `snow` dependency**

In `src-tauri/Cargo.toml`, under `[dependencies]`, add after the `rsa` line (keep the existing `# For RSA encryption test` block intact):

```toml
# Noise secure transport
snow = "0.9"
```

`snow` 0.9 defaults to the pure-Rust resolver (no C/`ring` system deps) and supports `Noise_XX_25519_ChaChaPoly_BLAKE2s`. Its license is `Unlicense OR MIT`, both already in `deny.toml`'s allowlist.

- [ ] **Step 2: Register the module in `lib.rs`**

In `src-tauri/src/lib.rs`, add `pub mod transport;` in alphabetical position — **between** `pub mod storage;` and `pub mod tray;** (rustfmt's `reorder_modules` sorts these; `transport` sorts before `tray`):

```rust
pub mod storage;
pub mod transport;
pub mod tray;
```

- [ ] **Step 3: Write the module skeleton with the failing test**

Create `src-tauri/src/transport/mod.rs`:

```rust
//! Authenticated, end-to-end-encrypted peer transport.
//!
//! A Noise XX handshake (`Noise_XX_25519_ChaChaPoly_BLAKE2s`) keyed by the
//! device's X25519 identity key establishes a [`session::Session`]; a short
//! identity-auth exchange ([`auth`]) then binds the Noise static key to the
//! device's Ed25519 identity. [`channel::SecureChannel`] drives the whole
//! thing over any async stream with length-prefixed framing.

pub mod auth;
pub mod channel;
pub mod handshake;
pub mod session;

pub use channel::SecureChannel;
pub use session::Session;

/// Noise pattern: XX (mutual static-key auth), X25519 DH, ChaChaPoly AEAD,
/// BLAKE2s hash.
pub const NOISE_PARAMS: &str = "Noise_XX_25519_ChaChaPoly_BLAKE2s";

/// Maximum size of a single Noise message on the wire (Noise spec limit).
pub const MAX_FRAME: usize = 65535;

/// Maximum plaintext per frame = `MAX_FRAME` minus the 16-byte AEAD tag.
pub const MAX_PLAINTEXT: usize = MAX_FRAME - 16;

/// Errors from the transport layer.
#[derive(Debug)]
pub enum TransportError {
    /// Underlying Noise/`snow` failure (handshake or AEAD).
    Noise(String),
    /// I/O failure on the byte stream.
    Io(std::io::Error),
    /// A declared frame length exceeded [`MAX_FRAME`].
    FrameTooLarge(usize),
    /// Plaintext exceeded [`MAX_PLAINTEXT`] for a single frame.
    PlaintextTooLarge(usize),
    /// Handshake ended without a peer static key.
    MissingRemoteStatic,
    /// Identity auth failed (bad signature or static-key mismatch).
    IdentityMismatch,
    /// Authenticated peer was not the one the caller expected.
    UnexpectedPeer,
    /// (De)serialization of the auth payload failed.
    Serialization(String),
}

impl std::fmt::Display for TransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TransportError::Noise(m) => write!(f, "noise error: {m}"),
            TransportError::Io(e) => write!(f, "io error: {e}"),
            TransportError::FrameTooLarge(n) => write!(f, "frame too large: {n} bytes"),
            TransportError::PlaintextTooLarge(n) => write!(f, "plaintext too large: {n} bytes"),
            TransportError::MissingRemoteStatic => write!(f, "handshake produced no remote static key"),
            TransportError::IdentityMismatch => write!(f, "peer identity authentication failed"),
            TransportError::UnexpectedPeer => write!(f, "authenticated peer was not the expected one"),
            TransportError::Serialization(m) => write!(f, "serialization error: {m}"),
        }
    }
}

impl std::error::Error for TransportError {}

impl From<std::io::Error> for TransportError {
    fn from(e: std::io::Error) -> Self {
        TransportError::Io(e)
    }
}

impl From<snow::Error> for TransportError {
    fn from(e: snow::Error) -> Self {
        TransportError::Noise(e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noise_params_are_valid() {
        // Proves snow is linked and our pattern string parses.
        let parsed: Result<snow::params::NoiseParams, _> = NOISE_PARAMS.parse();
        assert!(parsed.is_ok(), "NOISE_PARAMS must be a valid Noise pattern");
    }
}
```

> The four `pub mod` lines reference files created in later tasks. Create empty placeholder files now so the crate compiles: `src-tauri/src/transport/handshake.rs`, `session.rs`, `auth.rs`, `channel.rs`, each containing only a `//! placeholder` doc-comment line. Each later task replaces its placeholder.

- [ ] **Step 4: Create the placeholder submodule files**

Create each of these with a single line so `mod.rs` compiles:

`src-tauri/src/transport/handshake.rs`:
```rust
//! Noise XX handshake — implemented in Task 2.
```
`src-tauri/src/transport/session.rs`:
```rust
//! Noise transport session — implemented in Task 2.
```
`src-tauri/src/transport/auth.rs`:
```rust
//! Identity-binding auth exchange — implemented in Task 3.
```
`src-tauri/src/transport/channel.rs`:
```rust
//! Async SecureChannel — implemented in Task 4.
```

- [ ] **Step 5: Verify it compiles and the test passes**

Run:
```bash
cd src-tauri && nice -n 10 cargo test --lib transport::tests::noise_params_are_valid -- --test-threads=2
```
Expected: PASS (1 test). If `snow` fails to resolve, confirm the `[dependencies]` edit and network access for the first build.

- [ ] **Step 6: Verify cargo-deny still passes (license gate)**

Run:
```bash
cd /Users/kingsonwu/programming/rust-src/mesh-talk && nice -n 10 cargo deny check licenses 2>&1 | tail -20
```
Expected: no license errors (snow is `Unlicense OR MIT`, both allowlisted). If a *transitive* dep of snow trips an unlisted license, add that license id to the `allow` list in `deny.toml` and note it in the commit message.

- [ ] **Step 7: Commit**

```bash
cd /Users/kingsonwu/programming/rust-src/mesh-talk
git add src-tauri/Cargo.toml src-tauri/Cargo.lock src-tauri/src/lib.rs src-tauri/src/transport/
git commit -m "feat(transport): add snow dep and transport module skeleton

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 2: Noise XX handshake and transport session

**Files:**
- Modify (replace placeholder): `src-tauri/src/transport/session.rs`
- Modify (replace placeholder): `src-tauri/src/transport/handshake.rs`

This task implements both files together because a `Session` can only be constructed by completing a handshake, so the handshake test is what exercises the session.

- [ ] **Step 1: Implement `session.rs`**

Replace `src-tauri/src/transport/session.rs` with:

```rust
//! A Noise transport-mode session: encrypts/decrypts individual messages
//! once the handshake has completed. One [`Session`] per peer connection.

use crate::transport::{TransportError, MAX_PLAINTEXT};

/// Encrypts and decrypts messages over an established Noise channel.
pub struct Session {
    transport: snow::TransportState,
}

impl Session {
    /// Wrap a completed Noise transport state.
    pub(crate) fn new(transport: snow::TransportState) -> Self {
        Self { transport }
    }

    /// Encrypt one message, returning the Noise ciphertext blob (<= `MAX_FRAME`).
    pub fn encrypt(&mut self, plaintext: &[u8]) -> Result<Vec<u8>, TransportError> {
        if plaintext.len() > MAX_PLAINTEXT {
            return Err(TransportError::PlaintextTooLarge(plaintext.len()));
        }
        // ChaChaPoly adds a 16-byte tag; size the buffer exactly.
        let mut buf = vec![0u8; plaintext.len() + 16];
        let len = self.transport.write_message(plaintext, &mut buf)?;
        buf.truncate(len);
        Ok(buf)
    }

    /// Decrypt one Noise ciphertext blob back into plaintext.
    pub fn decrypt(&mut self, ciphertext: &[u8]) -> Result<Vec<u8>, TransportError> {
        // Plaintext is never larger than the ciphertext; size the buffer to it.
        let mut buf = vec![0u8; ciphertext.len()];
        let len = self.transport.read_message(ciphertext, &mut buf)?;
        buf.truncate(len);
        Ok(buf)
    }
}
```

- [ ] **Step 2: Write the failing handshake test**

Replace `src-tauri/src/transport/handshake.rs` with the implementation below **and** its tests. (We write impl + tests together because the test cannot exist without the impl types; run it next to confirm it passes.)

```rust
//! The Noise XX handshake state machine, wrapping `snow`. Drives the three
//! XX messages and, on completion, yields a [`Session`] plus the peer's
//! authenticated X25519 static key and the channel-binding handshake hash.

use crate::transport::session::Session;
use crate::transport::{TransportError, MAX_FRAME, NOISE_PARAMS};

/// One side of an in-progress Noise XX handshake.
pub struct Handshake {
    state: snow::HandshakeState,
}

/// The result of a completed handshake.
pub struct HandshakeOutput {
    /// The transport session for `send`/`recv`.
    pub session: Session,
    /// The peer's X25519 static public key (what Noise authenticated).
    pub remote_static: [u8; 32],
    /// The Noise channel-binding hash — identical on both ends, unique per
    /// session. Signed during identity auth.
    pub handshake_hash: Vec<u8>,
}

impl Handshake {
    /// Start the handshake as the initiator (dialing side).
    pub fn initiator(x25519_secret: &[u8; 32]) -> Result<Self, TransportError> {
        Self::build(x25519_secret, true)
    }

    /// Start the handshake as the responder (listening side).
    pub fn responder(x25519_secret: &[u8; 32]) -> Result<Self, TransportError> {
        Self::build(x25519_secret, false)
    }

    fn build(x25519_secret: &[u8; 32], initiator: bool) -> Result<Self, TransportError> {
        let params: snow::params::NoiseParams = NOISE_PARAMS
            .parse()
            .map_err(|_| TransportError::Noise("invalid noise params".into()))?;
        let builder = snow::Builder::new(params).local_private_key(x25519_secret);
        let state = if initiator {
            builder.build_initiator()?
        } else {
            builder.build_responder()?
        };
        Ok(Self { state })
    }

    /// Produce the next handshake message (payload is empty for our use).
    pub fn write_message(&mut self) -> Result<Vec<u8>, TransportError> {
        let mut buf = vec![0u8; MAX_FRAME];
        let len = self.state.write_message(&[], &mut buf)?;
        buf.truncate(len);
        Ok(buf)
    }

    /// Consume an incoming handshake message.
    pub fn read_message(&mut self, message: &[u8]) -> Result<(), TransportError> {
        let mut buf = vec![0u8; MAX_FRAME];
        self.state.read_message(message, &mut buf)?;
        Ok(())
    }

    /// Whether the handshake has completed.
    pub fn is_finished(&self) -> bool {
        self.state.is_handshake_finished()
    }

    /// Finish: capture the binding hash and peer static key, then switch to
    /// transport mode.
    pub fn into_session(self) -> Result<HandshakeOutput, TransportError> {
        let handshake_hash = self.state.get_handshake_hash().to_vec();
        let remote_static: [u8; 32] = self
            .state
            .get_remote_static()
            .ok_or(TransportError::MissingRemoteStatic)?
            .try_into()
            .map_err(|_| TransportError::MissingRemoteStatic)?;
        let transport = self.state.into_transport_mode()?;
        Ok(HandshakeOutput {
            session: Session::new(transport),
            remote_static,
            handshake_hash,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::device::DeviceIdentity;

    /// Drive the full XX handshake in memory and return both completed outputs.
    fn run_handshake(
        initiator_secret: &[u8; 32],
        responder_secret: &[u8; 32],
    ) -> (HandshakeOutput, HandshakeOutput) {
        let mut ini = Handshake::initiator(initiator_secret).expect("initiator");
        let mut res = Handshake::responder(responder_secret).expect("responder");

        // XX: -> e ; <- e, ee, s, es ; -> s, se
        let m1 = ini.write_message().expect("m1");
        res.read_message(&m1).expect("read m1");
        let m2 = res.write_message().expect("m2");
        ini.read_message(&m2).expect("read m2");
        let m3 = ini.write_message().expect("m3");
        res.read_message(&m3).expect("read m3");

        assert!(ini.is_finished() && res.is_finished());
        (ini.into_session().expect("ini session"), res.into_session().expect("res session"))
    }

    #[test]
    fn handshake_binds_static_keys_and_agrees_on_hash() {
        let a = DeviceIdentity::generate();
        let b = DeviceIdentity::generate();
        let (out_a, out_b) = run_handshake(&a.secret_bytes().1, &b.secret_bytes().1);

        // Both ends derive the identical channel-binding hash.
        assert_eq!(out_a.handshake_hash, out_b.handshake_hash);
        assert!(!out_a.handshake_hash.is_empty());

        // Each side authenticated the other's real X25519 static key.
        assert_eq!(out_a.remote_static, b.public().x25519_pub);
        assert_eq!(out_b.remote_static, a.public().x25519_pub);
    }

    #[test]
    fn sessions_round_trip_messages_both_directions() {
        let a = DeviceIdentity::generate();
        let b = DeviceIdentity::generate();
        let (mut out_a, mut out_b) = run_handshake(&a.secret_bytes().1, &b.secret_bytes().1);

        let ct = out_a.session.encrypt(b"hello from a").expect("encrypt a->b");
        assert_eq!(out_b.session.decrypt(&ct).expect("decrypt"), b"hello from a");

        let ct2 = out_b.session.encrypt(b"hello from b").expect("encrypt b->a");
        assert_eq!(out_a.session.decrypt(&ct2).expect("decrypt"), b"hello from b");
    }

    #[test]
    fn tampered_ciphertext_fails_to_decrypt() {
        let a = DeviceIdentity::generate();
        let b = DeviceIdentity::generate();
        let (mut out_a, mut out_b) = run_handshake(&a.secret_bytes().1, &b.secret_bytes().1);

        let mut ct = out_a.session.encrypt(b"secret").expect("encrypt");
        ct[0] ^= 0xFF; // flip a byte -> AEAD tag check must fail
        assert!(out_b.session.decrypt(&ct).is_err());
    }

    #[test]
    fn oversized_plaintext_is_rejected() {
        let a = DeviceIdentity::generate();
        let b = DeviceIdentity::generate();
        let (mut out_a, _out_b) = run_handshake(&a.secret_bytes().1, &b.secret_bytes().1);

        let big = vec![0u8; crate::transport::MAX_PLAINTEXT + 1];
        assert!(matches!(
            out_a.session.encrypt(&big),
            Err(TransportError::PlaintextTooLarge(_))
        ));
    }
}
```

- [ ] **Step 3: Run the tests to verify they pass**

Run:
```bash
cd src-tauri && nice -n 10 cargo test --lib transport::handshake -- --test-threads=2
```
Expected: PASS — 4 tests (`handshake_binds_static_keys_and_agrees_on_hash`, `sessions_round_trip_messages_both_directions`, `tampered_ciphertext_fails_to_decrypt`, `oversized_plaintext_is_rejected`).

- [ ] **Step 4: fmt + clippy clean**

Run:
```bash
cd src-tauri && nice -n 10 cargo fmt && nice -n 10 cargo clippy --lib -- -D warnings 2>&1 | tail -20
```
Expected: no warnings in `transport`.

- [ ] **Step 5: Commit**

```bash
cd /Users/kingsonwu/programming/rust-src/mesh-talk
git add src-tauri/src/transport/handshake.rs src-tauri/src/transport/session.rs
git commit -m "feat(transport): Noise XX handshake and transport session

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 3: Identity-binding auth exchange

**Files:**
- Modify (replace placeholder): `src-tauri/src/transport/auth.rs`

Binds the Noise X25519 static key to the long-term Ed25519 identity. Pure logic (no I/O), fully unit-testable.

- [ ] **Step 1: Implement `auth.rs` with the failing tests**

Replace `src-tauri/src/transport/auth.rs` with:

```rust
//! Identity-binding auth exchange. After the Noise handshake, each side proves
//! its X25519 static key belongs to its Ed25519 identity by signing the
//! per-session handshake hash. This makes the channel's `user_id` meaningful.

use crate::identity::device::{DeviceIdentity, PublicIdentity};
use crate::transport::TransportError;
use serde::{Deserialize, Serialize};

/// Domain separator so these signatures can never be mistaken for any other
/// signature the identity key produces.
pub const AUTH_DOMAIN: &[u8] = b"mesh-talk-transport-auth-v1";

/// What each side sends (encrypted) during the auth exchange.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthMessage {
    /// The sender's public identity (Ed25519 + X25519 keys).
    pub public: PublicIdentity,
    /// Ed25519 signature over `AUTH_DOMAIN || handshake_hash`. 64 bytes; held
    /// as a `Vec<u8>` because serde does not derive for `[u8; 64]`.
    pub signature: Vec<u8>,
}

fn signing_input(handshake_hash: &[u8]) -> Vec<u8> {
    let mut input = Vec::with_capacity(AUTH_DOMAIN.len() + handshake_hash.len());
    input.extend_from_slice(AUTH_DOMAIN);
    input.extend_from_slice(handshake_hash);
    input
}

/// Build our auth message for a completed handshake.
pub fn build_auth(identity: &DeviceIdentity, handshake_hash: &[u8]) -> AuthMessage {
    let signature = identity.sign(&signing_input(handshake_hash)).to_vec();
    AuthMessage {
        public: identity.public(),
        signature,
    }
}

/// Verify a peer's auth message against this session's handshake hash and the
/// X25519 static key Noise authenticated. Returns the verified [`PublicIdentity`].
pub fn verify_auth(
    msg: &AuthMessage,
    handshake_hash: &[u8],
    remote_static: &[u8; 32],
) -> Result<PublicIdentity, TransportError> {
    // The advertised X25519 key must be the one Noise actually authenticated.
    if &msg.public.x25519_pub != remote_static {
        return Err(TransportError::IdentityMismatch);
    }
    // The signature must be 64 bytes and verify against the advertised Ed25519 key.
    let signature: [u8; 64] = msg
        .signature
        .as_slice()
        .try_into()
        .map_err(|_| TransportError::IdentityMismatch)?;
    if !DeviceIdentity::verify(&msg.public.ed25519_pub, &signing_input(handshake_hash), &signature) {
        return Err(TransportError::IdentityMismatch);
    }
    Ok(msg.public.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_then_verify_round_trips() {
        let id = DeviceIdentity::generate();
        let hh = [42u8; 32];
        let msg = build_auth(&id, &hh);

        let verified = verify_auth(&msg, &hh, &id.public().x25519_pub).expect("verify");
        assert_eq!(verified, id.public());
        assert_eq!(verified.user_id(), id.user_id());
    }

    #[test]
    fn verify_rejects_different_handshake_hash() {
        // A signature captured from one session must not validate in another.
        let id = DeviceIdentity::generate();
        let msg = build_auth(&id, &[1u8; 32]);
        assert!(matches!(
            verify_auth(&msg, &[2u8; 32], &id.public().x25519_pub),
            Err(TransportError::IdentityMismatch)
        ));
    }

    #[test]
    fn verify_rejects_static_key_mismatch() {
        // Advertised x25519 key differs from the Noise-authenticated static key.
        let id = DeviceIdentity::generate();
        let hh = [7u8; 32];
        let msg = build_auth(&id, &hh);
        let wrong_static = [0u8; 32];
        assert!(matches!(
            verify_auth(&msg, &hh, &wrong_static),
            Err(TransportError::IdentityMismatch)
        ));
    }

    #[test]
    fn verify_rejects_tampered_signature() {
        let id = DeviceIdentity::generate();
        let hh = [9u8; 32];
        let mut msg = build_auth(&id, &hh);
        msg.signature[0] ^= 0xFF;
        assert!(matches!(
            verify_auth(&msg, &hh, &id.public().x25519_pub),
            Err(TransportError::IdentityMismatch)
        ));
    }

    #[test]
    fn verify_rejects_wrong_length_signature() {
        let id = DeviceIdentity::generate();
        let hh = [3u8; 32];
        let mut msg = build_auth(&id, &hh);
        msg.signature.truncate(10);
        assert!(matches!(
            verify_auth(&msg, &hh, &id.public().x25519_pub),
            Err(TransportError::IdentityMismatch)
        ));
    }
}
```

- [ ] **Step 2: Run the tests to verify they pass**

Run:
```bash
cd src-tauri && nice -n 10 cargo test --lib transport::auth -- --test-threads=2
```
Expected: PASS — 5 tests.

- [ ] **Step 3: fmt + clippy clean, then commit**

```bash
cd src-tauri && nice -n 10 cargo fmt && nice -n 10 cargo clippy --lib -- -D warnings 2>&1 | tail -20
cd /Users/kingsonwu/programming/rust-src/mesh-talk
git add src-tauri/src/transport/auth.rs
git commit -m "feat(transport): identity-binding auth over the handshake hash

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 4: Async `SecureChannel` with framing (over in-memory duplex)

**Files:**
- Modify (replace placeholder): `src-tauri/src/transport/channel.rs`

Orchestrates handshake → identity auth → length-prefixed framed `send`/`recv` over any async stream. Generic over `S: AsyncRead + AsyncWrite + Unpin` so the same code is exercised by an in-memory duplex (this task) and a real `TcpStream` (Task 5).

> **Half-duplex note:** `send`/`recv` take `&mut self` (no concurrent read+write on one channel). That is sufficient for Phase 0's request/response integration rig. Full-duplex split is deferred to the node-service wiring step.

- [ ] **Step 1: Implement `channel.rs` with its tests**

Replace `src-tauri/src/transport/channel.rs` with:

```rust
//! Async authenticated, encrypted channel over any byte stream.
//!
//! Wire format per message: a 4-byte big-endian length prefix followed by the
//! Noise blob. The same framing carries handshake messages, the encrypted auth
//! exchange, and application messages.

use crate::identity::device::{DeviceIdentity, PublicIdentity};
use crate::transport::auth::{build_auth, verify_auth, AuthMessage};
use crate::transport::handshake::{Handshake, HandshakeOutput};
use crate::transport::session::Session;
use crate::transport::{TransportError, MAX_FRAME};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// An established, authenticated, encrypted channel to one peer.
pub struct SecureChannel<S> {
    stream: S,
    session: Session,
    peer: PublicIdentity,
}

impl<S: AsyncRead + AsyncWrite + Unpin> SecureChannel<S> {
    /// Dial: perform the XX handshake as initiator, then authenticate.
    /// If `expected_peer` is set, the authenticated identity must match it.
    pub async fn connect(
        mut stream: S,
        identity: &DeviceIdentity,
        expected_peer: Option<&PublicIdentity>,
    ) -> Result<Self, TransportError> {
        let x_secret = identity.secret_bytes().1;
        let mut hs = Handshake::initiator(&x_secret)?;

        // XX: -> e ; <- e, ee, s, es ; -> s, se
        let m1 = hs.write_message()?;
        write_frame(&mut stream, &m1).await?;
        let m2 = read_frame(&mut stream).await?;
        hs.read_message(&m2)?;
        let m3 = hs.write_message()?;
        write_frame(&mut stream, &m3).await?;

        let out = hs.into_session()?;
        // Initiator authenticates first, then reads the peer's auth.
        let (session, peer) =
            auth_exchange(&mut stream, identity, out, AuthOrder::SendFirst).await?;

        if let Some(expected) = expected_peer {
            if expected != &peer {
                return Err(TransportError::UnexpectedPeer);
            }
        }
        Ok(Self { stream, session, peer })
    }

    /// Accept: perform the XX handshake as responder, then authenticate.
    pub async fn accept(mut stream: S, identity: &DeviceIdentity) -> Result<Self, TransportError> {
        let x_secret = identity.secret_bytes().1;
        let mut hs = Handshake::responder(&x_secret)?;

        let m1 = read_frame(&mut stream).await?;
        hs.read_message(&m1)?;
        let m2 = hs.write_message()?;
        write_frame(&mut stream, &m2).await?;
        let m3 = read_frame(&mut stream).await?;
        hs.read_message(&m3)?;

        let out = hs.into_session()?;
        // Responder reads the peer's auth first, then sends its own.
        let (session, peer) =
            auth_exchange(&mut stream, identity, out, AuthOrder::ReceiveFirst).await?;

        Ok(Self { stream, session, peer })
    }

    /// The cryptographically authenticated identity of the peer.
    pub fn peer_identity(&self) -> &PublicIdentity {
        &self.peer
    }

    /// Encrypt and send one application message.
    pub async fn send(&mut self, plaintext: &[u8]) -> Result<(), TransportError> {
        let ct = self.session.encrypt(plaintext)?;
        write_frame(&mut self.stream, &ct).await
    }

    /// Receive and decrypt one application message.
    pub async fn recv(&mut self) -> Result<Vec<u8>, TransportError> {
        let ct = read_frame(&mut self.stream).await?;
        self.session.decrypt(&ct)
    }
}

/// Whether this side sends or receives its auth message first (prevents deadlock).
enum AuthOrder {
    SendFirst,
    ReceiveFirst,
}

/// Exchange and verify identity-auth messages over the freshly-established
/// session. Returns the session and the verified peer identity.
async fn auth_exchange<S: AsyncRead + AsyncWrite + Unpin>(
    stream: &mut S,
    identity: &DeviceIdentity,
    out: HandshakeOutput,
    order: AuthOrder,
) -> Result<(Session, PublicIdentity), TransportError> {
    let HandshakeOutput { mut session, remote_static, handshake_hash } = out;

    let send = |session: &mut Session| -> Result<Vec<u8>, TransportError> {
        let auth = build_auth(identity, &handshake_hash);
        let bytes = bincode::serialize(&auth)
            .map_err(|e| TransportError::Serialization(e.to_string()))?;
        session.encrypt(&bytes)
    };

    let peer = match order {
        AuthOrder::SendFirst => {
            let ct = send(&mut session)?;
            write_frame(stream, &ct).await?;
            let peer_ct = read_frame(stream).await?;
            verify_peer(&mut session, &peer_ct, &handshake_hash, &remote_static)?
        }
        AuthOrder::ReceiveFirst => {
            let peer_ct = read_frame(stream).await?;
            let peer = verify_peer(&mut session, &peer_ct, &handshake_hash, &remote_static)?;
            let ct = send(&mut session)?;
            write_frame(stream, &ct).await?;
            peer
        }
    };
    Ok((session, peer))
}

fn verify_peer(
    session: &mut Session,
    peer_ct: &[u8],
    handshake_hash: &[u8],
    remote_static: &[u8; 32],
) -> Result<PublicIdentity, TransportError> {
    let bytes = session.decrypt(peer_ct)?;
    let auth: AuthMessage = bincode::deserialize(&bytes)
        .map_err(|e| TransportError::Serialization(e.to_string()))?;
    verify_auth(&auth, handshake_hash, remote_static)
}

/// Write a length-prefixed frame (4-byte big-endian length + blob) and flush.
pub(crate) async fn write_frame<W: AsyncWrite + Unpin>(
    w: &mut W,
    blob: &[u8],
) -> Result<(), TransportError> {
    let len = blob.len() as u32;
    w.write_all(&len.to_be_bytes()).await?;
    w.write_all(blob).await?;
    w.flush().await?;
    Ok(())
}

/// Read one length-prefixed frame, rejecting absurd lengths before allocating.
pub(crate) async fn read_frame<R: AsyncRead + Unpin>(
    r: &mut R,
) -> Result<Vec<u8>, TransportError> {
    let mut len_buf = [0u8; 4];
    r.read_exact(&mut len_buf).await?;
    let len = u32::from_be_bytes(len_buf) as usize;
    if len > MAX_FRAME {
        return Err(TransportError::FrameTooLarge(len));
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf).await?;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::device::DeviceIdentity;

    #[tokio::test]
    async fn channel_handshakes_authenticates_and_exchanges_messages() {
        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        let a = DeviceIdentity::generate();
        let b = DeviceIdentity::generate();
        let (a_pub, b_pub) = (a.public(), b.public());

        let server = tokio::spawn(async move {
            let mut ch = SecureChannel::accept(server_io, &b).await.expect("accept");
            // Server confirms it is talking to A.
            assert_eq!(ch.peer_identity(), &a_pub);
            let got = ch.recv().await.expect("recv");
            assert_eq!(got, b"ping");
            ch.send(b"pong").await.expect("send");
        });

        let mut ch = SecureChannel::connect(client_io, &a, Some(&b_pub))
            .await
            .expect("connect");
        // Client confirms it is talking to B.
        assert_eq!(ch.peer_identity(), &b_pub);
        ch.send(b"ping").await.expect("send");
        let got = ch.recv().await.expect("recv");
        assert_eq!(got, b"pong");

        server.await.expect("server task");
    }

    #[tokio::test]
    async fn connect_rejects_unexpected_peer() {
        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        let a = DeviceIdentity::generate();
        let b = DeviceIdentity::generate();
        let wrong = DeviceIdentity::generate().public();

        // Responder completes its side normally.
        let server = tokio::spawn(async move {
            let _ = SecureChannel::accept(server_io, &b).await;
        });

        // Initiator expects `wrong` but actually talks to B -> UnexpectedPeer.
        let result = SecureChannel::connect(client_io, &a, Some(&wrong)).await;
        assert!(matches!(result, Err(TransportError::UnexpectedPeer)));
        let _ = server.await;
    }

    #[tokio::test]
    async fn read_frame_rejects_oversized_length() {
        let (mut a, mut b) = tokio::io::duplex(64);
        // Write a length prefix far above MAX_FRAME, no body.
        let bogus = (MAX_FRAME as u32 + 1).to_be_bytes();
        a.write_all(&bogus).await.expect("write len");
        a.flush().await.expect("flush");
        let result = read_frame(&mut b).await;
        assert!(matches!(result, Err(TransportError::FrameTooLarge(_))));
    }
}
```

- [ ] **Step 2: Run the tests to verify they pass**

Run:
```bash
cd src-tauri && nice -n 10 cargo test --lib transport::channel -- --test-threads=2
```
Expected: PASS — 3 tests (`channel_handshakes_authenticates_and_exchanges_messages`, `connect_rejects_unexpected_peer`, `read_frame_rejects_oversized_length`).

- [ ] **Step 3: fmt + clippy clean, then commit**

```bash
cd src-tauri && nice -n 10 cargo fmt && nice -n 10 cargo clippy --lib -- -D warnings 2>&1 | tail -20
cd /Users/kingsonwu/programming/rust-src/mesh-talk
git add src-tauri/src/transport/channel.rs
git commit -m "feat(transport): async SecureChannel with length-prefixed framing

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 5: Real-TCP integration test

**Files:**
- Create: `src-tauri/src/transport/tests.rs`
- Modify: `src-tauri/src/transport/mod.rs` (add `#[cfg(test)] mod tests;`)

Proves the channel works over a real `TcpListener`/`TcpStream` on localhost — the foundation for Phase 0's two-CLI integration rig.

- [ ] **Step 1: Convert the inline test module to a file-backed one**

`mod.rs` currently ends with an inline `#[cfg(test)] mod tests { ... }` block (the `noise_params_are_valid` test from Task 1). Replace that inline block with a file-backed module declaration, and the test itself moves into `tests.rs` (Step 2). There must be only one module named `tests`.

1. Delete the entire inline `#[cfg(test)] mod tests { ... }` block at the bottom of `mod.rs`.
2. Add this in its place at the bottom of `mod.rs`:

```rust
#[cfg(test)]
mod tests;
```

- [ ] **Step 2: Create the integration test file**

Create `src-tauri/src/transport/tests.rs`:

```rust
//! Module-level transport tests, including a real-TCP integration test.

use crate::identity::device::DeviceIdentity;
use crate::transport::{SecureChannel, NOISE_PARAMS};
use tokio::net::{TcpListener, TcpStream};

#[test]
fn noise_params_are_valid() {
    let parsed: Result<snow::params::NoiseParams, _> = NOISE_PARAMS.parse();
    assert!(parsed.is_ok(), "NOISE_PARAMS must be a valid Noise pattern");
}

#[tokio::test]
async fn secure_channel_over_real_tcp() {
    let a = DeviceIdentity::generate();
    let b = DeviceIdentity::generate();
    let (a_pub, b_pub) = (a.public(), b.public());

    // Bind an ephemeral localhost port for the responder.
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local_addr");

    let server = tokio::spawn(async move {
        let (sock, _peer_addr) = listener.accept().await.expect("accept tcp");
        let mut ch = SecureChannel::accept(sock, &b).await.expect("secure accept");
        assert_eq!(ch.peer_identity(), &a_pub);
        let msg = ch.recv().await.expect("recv");
        assert_eq!(msg, b"hello over tcp");
        ch.send(b"ack").await.expect("send");
    });

    let sock = TcpStream::connect(addr).await.expect("connect tcp");
    let mut ch = SecureChannel::connect(sock, &a, Some(&b_pub))
        .await
        .expect("secure connect");
    assert_eq!(ch.peer_identity(), &b_pub);
    ch.send(b"hello over tcp").await.expect("send");
    assert_eq!(ch.recv().await.expect("recv"), b"ack");

    server.await.expect("server task");
}
```

- [ ] **Step 3: Run the transport test suite (all of it)**

Run:
```bash
cd src-tauri && nice -n 10 cargo test --lib transport:: -- --test-threads=2
```
Expected: PASS — all transport tests (`mod::tests::noise_params_are_valid`, the 4 handshake tests, the 5 auth tests, the 3 channel tests, and `secure_channel_over_real_tcp`). Count ~14.

- [ ] **Step 4: Full fmt + clippy gate**

Run:
```bash
cd src-tauri && nice -n 10 cargo fmt && nice -n 10 cargo clippy --lib -- -D warnings 2>&1 | tail -30
```
Expected: clean. Confirm `cargo fmt --check` exits 0:
```bash
cd src-tauri && nice -n 10 cargo fmt --check; echo "EXIT: $?"
```
Expected: `EXIT: 0`.

- [ ] **Step 5: Commit**

```bash
cd /Users/kingsonwu/programming/rust-src/mesh-talk
git add src-tauri/src/transport/mod.rs src-tauri/src/transport/tests.rs
git commit -m "test(transport): real-TCP SecureChannel integration test

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Notes for the reviewer / next phase

- **What this delivers:** an authenticated, encrypted, framed channel primitive — `SecureChannel::connect`/`accept`, `send`/`recv`, `peer_identity()`. It is fully isolated; nothing in `node_service`, `network/tcp.rs`, or `commands.rs` is touched yet.
- **Deliberately deferred (do NOT add here):** wiring `SecureChannel` into the live node service / discovery, full-duplex read-write split, connection pooling, reconnection, and a typed message envelope on top of the byte channel. Those belong to a later Phase-0 plan (event-log + sync) or the node-service integration step.
- **Security properties:** mutual authentication of both X25519 static keys (Noise XX) plus an Ed25519 binding signature over the per-session handshake hash, so the channel's `peer_identity().user_id()` is cryptographically trustworthy. AEAD (ChaChaPoly) gives confidentiality + integrity; tampered frames fail to decrypt. Frame-length guard prevents allocation-DoS.
- **Accepted v1 limitations:** no forward secrecy beyond Noise's ephemeral DH (matches the sealed-box DM tradeoff in the design spec); half-duplex `&mut self` API.
