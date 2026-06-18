# File Sharing (Phase 2) Design

**Status:** Derived from the master redesign spec (`2026-06-15-mesh-talk-redesign-design.md` §5 "Files", §9 Phase 2 "files-over-event-log"). Authored autonomously against that approved north-star per the standing directive.

## Goal

Let a user send a file into a DM or channel; recipients receive it E2E-encrypted and can save it locally. Offline recipients get it later via the post office. Reuse the existing event-log + sync + distribution stack — **no new wire protocol**.

## Core idea: files-over-event-log

A file is split into fixed-size chunks. Each chunk is encrypted with a per-file symmetric key and carried as an **event** in a dedicated per-file conversation. A `FileManifest` event in the *original* conversation (DM or channel) announces the file and carries the (sealed) file key + the file-conversation id. Because chunks are ordinary events, they distribute and resync through the exact machinery channels/DMs already use — directly to online members and via the post office for offline ones. Content-addressing + the event log's id-set reconcile give resumable, dedup'd, integrity-checked transfer for free.

### Crypto

- **File key:** a fresh 256-bit key per file (`OsRng`). Chunks are sealed with raw-key AES-256-GCM (fresh 96-bit nonce prepended per chunk) — identical primitive to `channel::crypto::seal_channel_message`.
- **Content addressing:** each chunk's id is `BLAKE3/SHA-256(ciphertext)` (the event id already is the content hash of the event, so the chunk event's own id *is* its content address — no separate hash needed). The manifest records `chunk_count` + an overall plaintext `checksum` (SHA-256 of the whole file) for end-to-end verification after reassembly.
- **Manifest confidentiality:** the `FileManifest` payload (name, size, mime, checksum, file_key, file_conv id, chunk_count) is sealed exactly like a message — DM sealed-box for a DM, the channel group key for a channel — so only conversation members learn the file key. The post office relays both the manifest and the chunk ciphertext, learning neither the key nor the plaintext.

### Data model

- New `EventKind::FileManifest = 7` (already reserved in the master spec) carries the sealed manifest in the *original* conversation.
- The per-file conversation id (`file_conv`) is a fresh random `ConversationId` (like `new_channel_id`). Its events are chunk ciphertexts; ordering is by per-author seq (sender is the sole author), so reassembly is seq-ordered.
- `FileManifest { name: String, size: u64, mime: String, checksum: [u8;32], file_key: [u8;32], file_conv: ConversationId, chunk_count: u32 }` — fail-closed bincode encode/decode.

### Flows

**Send** (`send_file(conv, path)`): read file → SHA-256 checksum → gen file_key + a random `file_conv` → split into `CHUNK_SIZE` chunks → seal each chunk, append as an event in `file_conv` → build + seal the `FileManifest`, append the FileManifest event in `conv` → distribute BOTH conversations (dial members + replicate to the post office), reusing the channel/DM distribution path.

**Receive:** a peer syncs `conv`, processes the FileManifest event → opens the sealed manifest → surfaces a `ReceivedFile { conv, from, name, size, file_conv, ... }` on a stream + records the manifest. Chunks arrive eagerly (the sender distributed `file_conv` too) or on the next sync.

**Save** (`save_file(file_conv, dest)`): gather the `file_conv` chunk events in seq order → decrypt each with the file key → concatenate → verify SHA-256 == manifest.checksum → write to `dest`. Errors if any chunk is missing (not yet synced) or verification fails.

### Scope (MVP) and deferrals

- **In:** send/receive/save for DMs and channels; eager chunk distribution (chunks replicate with the manifest, so an offline recipient retrieves them from the post office); end-to-end checksum verification; a file "card" in the UI with a Save action.
- **Deferred (Phase 2.x):** on-demand chunk fetch (vs eager push) to avoid replicating large files to every member + PO; resumable progress UI; drag-and-drop (start with a file picker); thumbnails/previews; chunk garbage collection. Note the eager-push tradeoff in code so it isn't mistaken for the final design.
- **Limits:** cap file size (e.g. 50 MB) so eager event-log replication stays bounded; `CHUNK_SIZE` = 256 KiB.

## Decomposition (one plan each)

1. **File crypto + manifest model** (pure, `file/` module): chunking, per-chunk seal/open, `FileManifest` encode/decode + seal/open, whole-file checksum + reassembly/verify. Fully unit-tested, no I/O or networking.
2. **Node integration**: `send_file`/`save_file` + FileManifest event processing (a `FileBook`-style replay surfacing `ReceivedFile`) + chunk events in `file_conv` + distribution of both conversations + a `received_files` stream. Loopback-TCP rig (two nodes send+receive+save a file).
3. **App + UI**: IPC commands (`redesign_send_file`/`redesign_list_files`/`redesign_save_file`) + a `redesign-file-received` event + a file card with a file-picker send + Save in the `/redesign` view.

Each plan produces working, tested software on its own. Plan 1 is the pure foundation and lands first.
