# mesh-talk-core

The UI-free protocol core of [Mesh-Talk](https://github.com/Kingson4Wu/mesh-talk) — a
**serverless, end-to-end-encrypted, LAN-first messenger**. This crate is the foundation an
SDK is built on: it has **no UI and no Tauri dependency**, so it can be embedded in a CLI,
a daemon, a desktop shell (as the Mesh-Talk app does), or any other Rust program.

There are no servers. Peers discover each other on the local network, talk directly over an
authenticated encrypted channel, and converge a signed, content-addressed event log. An
elected "post office" peer store-and-forwards ciphertext so a message still reaches a
recipient who was offline.

## What's inside

| Module | Responsibility |
| --- | --- |
| [`identity`] | Ed25519/X25519 device identities, cross-device accounts, and device certificates. |
| [`eventlog`] | Content-addressed, hash-linked, encrypted event log + peer reconciliation (sync). |
| [`ratchet`] | Double Ratchet — per-message forward secrecy for DMs. |
| [`channel`] | Sender-key group ratchet — forward secrecy for group channels. |
| [`dm`] | Sealed direct-message envelopes. |
| [`transport`] | A Noise (`snow`) secure channel with identity binding. |
| [`discovery`] | Signed-announce LAN peer discovery + roster. |
| [`postoffice`] | A durable store-and-forward relay for offline delivery (holds ciphertext only). |
| [`file`] | Chunked, encrypted file transfer. |
| [`node`] | The runtime that wires the above into a working peer (DMs, channels, files, history, sync). |

## Use it

This crate is not published to crates.io; depend on it by git (or path, within the workspace):

```toml
[dependencies]
mesh-talk-core = { git = "https://github.com/Kingson4Wu/mesh-talk" }
tokio = { version = "1", features = ["full"] }
```

### Example (abridged)

Load a persistent identity, open a node, and send a direct message. This is the shape of a
node; for a complete, runnable program see [`src/bin/mesh-talk-node.rs`](src/bin/mesh-talk-node.rs),
the reference consumer this crate ships.

```rust,ignore
use std::sync::{Arc, Mutex};
use mesh_talk_core::discovery::Roster;
use mesh_talk_core::identity::{keystore, account_keystore};
use mesh_talk_core::node::{Node, ReceivedDm};
use tokio::sync::mpsc;

// Persistent, password-encrypted device + account keystores (created on first run).
let identity = keystore::load_or_create("device.keystore".as_ref(), "pw")?;
let account = account_keystore::load_or_create("account.keystore".as_ref(), "pw")?;

// Discovery writes the roster; the node reads it. Inbound events arrive on channels.
let roster = Arc::new(Mutex::new(Roster::default()));
let (dm_tx, mut dm_rx) = mpsc::unbounded_channel::<ReceivedDm>();
let (chan_tx, _chan_rx) = mpsc::unbounded_channel();
let (file_tx, _file_rx) = mpsc::unbounded_channel();

let node = Node::open_with_account(
    identity, account, Arc::clone(&roster),
    dm_tx, chan_tx, file_tx,
    "messages.log".as_ref(), "sent.log".as_ref(), "pw",
)?;

// `node.send_dm(&recipient_user_id, b"hello").await?;` once a peer is in the roster.
// Received DMs surface on `dm_rx`; run discovery + the accept loop to populate the roster.
// (See the CLI for the discovery/listener/drain wiring.)
```

The crate is async and built on Tokio; `node` spawns the accept loop, discovery, and the
post-office drain as tasks.

## CLI: `mesh-talk-node`

A headless node ships with the crate as a reference consumer:

```bash
# A normal node: discovers peers on the LAN and runs a 1:1 DM REPL.
cargo run --bin mesh-talk-node -- \
    --keystore ./alice/device.keystore --password pw --name Alice

# A post office: a durable store-and-forward relay (no REPL, relays ciphertext).
cargo run --bin mesh-talk-node -- \
    --keystore ./relay/device.keystore --password pw --name relay --post-office
```

REPL commands: `/peers`, `/msg <user_id-prefix> <text>`, `/history <user_id-prefix> [n]`, `/quit`.

> A post office cold-starts slowly (~15 s on first run): it does two password-KDF unlocks
> back to back (the keystore and the relay log). Wait for its `post-office … listening` line
> rather than a fixed sleep.

## Security model (summary)

- **End-to-end encryption.** Message content is encrypted to the recipient(s); relays and the
  network never see plaintext and hold no conversation keys.
- **Forward secrecy.** DMs use a Double Ratchet; group channels use a sender-key ratchet with
  epoch rotation on membership change.
- **Authenticated transport.** Sessions run over Noise with the peer's device identity bound
  into the handshake (canonical, `verify_strict` Ed25519 signatures).
- **Integrity.** The event log is content-addressed and hash-linked; the at-rest log is
  AEAD-authenticated; a tampered or reordered local cache is self-healing via re-sync.
- **Known, by-design limits:** a relay inevitably sees an event's author and can derive the
  participant pair from the conversation id (content stays encrypted); device linking relies
  on the one-time pairing code as its authenticator (no separate SAS UX). See
  [`specifications/TODO.md`](../../specifications/TODO.md).

This is research-grade software; it has not had an independent security audit.

## License

MIT. Part of the [Mesh-Talk](https://github.com/Kingson4Wu/mesh-talk) workspace; see
[`docs/ARCHITECTURE.md`](../../docs/ARCHITECTURE.md) for the full design.

[`identity`]: src/identity
[`eventlog`]: src/eventlog
[`ratchet`]: src/ratchet
[`channel`]: src/channel
[`dm`]: src/dm.rs
[`transport`]: src/transport
[`discovery`]: src/discovery
[`postoffice`]: src/postoffice
[`file`]: src/file
[`node`]: src/node
