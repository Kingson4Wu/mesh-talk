# Implementation Status

The legacy server/relay stack has been **retired**; the serverless, end-to-end-encrypted
stack is the entire product and is feature-complete:

- **Phase 0** — identity/keystore, Noise transport, content-addressed event log + sync,
  X3DH DM crypto, persistent history, post office (store-and-forward). ✅
- **Phase 1** — LAN discovery + node wiring, group **channels**. ✅
- **Phase 2** — file sharing, reactions, replies/threads, @mentions, search. ✅
- **Phase 3** — forward secrecy (DM **Double Ratchet** + channel **sender-key** ratchet),
  **multi-device** (account identity, device linking, account-addressed fan-out +
  self-sync, history backfill, re-key). ✅

Architecture: **[`docs/ARCHITECTURE.md`](../docs/ARCHITECTURE.md)**. Design specs +
per-phase implementation plans: **`docs/superpowers/specs/`** and
**`docs/superpowers/plans/`**. Detailed task history lives in git.

## Known follow-ups (not blocking)
- Device linking has no SAS/key-pinning (LAN MITM window); backfill history travels as
  plaintext over the Noise channel.
- `node/node.rs` could be split further by domain (the test module is already extracted).
- A `glib 0.20` bump is gated on tauri's gtk stack (auto-watched by
  `.github/workflows/glib-0.20-watch.yml`).
