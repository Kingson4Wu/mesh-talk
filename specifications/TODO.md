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
  plaintext over the Noise channel. Also: the pairing code has no TTL and a failed attempt
  doesn't invalidate it (defense-in-depth only — the code is a full 128-bit secret, so
  brute force is infeasible; add a short TTL + attempt ceiling anyway). Backfill records
  from the linker are imported without re-verification (linker is in the trust boundary).
- **Post office** (`postoffice/`, `node/postbox.rs`): the relay serve loop has no per-
  connection round cap / idle timeout, and storage has no quota/retention — an
  authenticated peer can spin it or fill it (DoS). Also a confirmed metadata leak: the
  relay sees cleartext `author` + can derive the participant pair from `conversation_id`
  (content stays encrypted). Needs rate-limit + quota/GC; metadata leak is inherent to a
  relay but should be documented.
- **Channel membership no-op edges** (`node/channels.rs`): `add_channel_member` of an
  existing member and `remove_channel_member` of a non-member still bump the epoch +
  re-key (wasteful); removing the last member yields an unreadable channel. Add guards.
- **Sync `have` id-set is not frame-bounded** (`eventlog/sync.rs`): the 3-message
  reconciliation sends each side's full event-id set in one frame. A single conversation
  past ~2040 events (32 B/id vs `MAX_PLAINTEXT` 65519) overflows the frame →
  `PlaintextTooLarge` → that conversation can no longer sync. Only the *events* are
  budgeted today, not the `have` set. Needs a protocol change (chunk/page the id-set, or
  range/IBLT reconciliation) — a focused design, not a hasty patch.
- **Sync `have` id-set is not frame-bounded** (`eventlog/sync.rs`): the 3-message
  reconciliation sends each side's full event-id set in one frame. A single conversation
  past ~2040 events (32 B/id vs `MAX_PLAINTEXT` 65519) overflows the frame →
  `PlaintextTooLarge` → that conversation can no longer sync. Only the *events* are
  budgeted today, not the `have` set. Needs a protocol change (chunk/page the id-set, or
  range/IBLT reconciliation) — a focused design, not a hasty patch.
- A `glib 0.20` bump is gated on tauri's gtk stack (auto-watched by
  `.github/workflows/glib-0.20-watch.yml`).
