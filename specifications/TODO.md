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
- Device linking relies on the one-time code as its authenticator (no separate SAS/key-
  pinning UX); backfill records from the linker are imported without re-verification
  (the linker is inside the trust boundary — it's handing you the account secret).
- Post-office **metadata** exposure (by design, inherent to any relay): the relay sees
  each event's cleartext `author` and can derive the participant pair from the conversation
  id. Content stays encrypted (it has no key). Not fixable without onion-routing/mixing.
- **Reaction toggle ordering** (`node/reaction.rs`, `reactions.rs`): aggregation is
  last-writer-wins folded in `(lamport, id)` order, deterministic across replicas. Two edges
  are not "intent-perfect": (a) an add and its remove that end up at the SAME lamport are
  tie-broken by event-id hash, not by which the user did last; (b) own reactions are merged
  after log events, so in the rare multi-device same-account-same-target toggle case the
  result can invert. Both are consistent (no divergence) — fixing needs a per-author causal
  seq on reactions, deferred. Also: a removed channel member's past reactions stop rendering
  (implicit membership filter).
- A `glib 0.20` bump is gated on tauri's gtk stack (auto-watched by
  `.github/workflows/glib-0.20-watch.yml`).
