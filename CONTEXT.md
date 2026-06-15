# Mesh-Talk Domain

## Core job

Let users on the same local network chat directly — no central server. Each node
discovers peers via UDP broadcast and exchanges messages over direct TCP
connections. The desktop app is a Tauri (Rust) backend with a Vue frontend; a
headless CLI (`mesh-talk-cli`) drives the same core for testing.

## Layers (frontend → commands → services → domain/network/storage)

| Layer | Path (`src-tauri/src/`) | Responsibility |
|-------|-------------------------|----------------|
| **Frontend** | `frontend/src/` (Vue) | UI, talks to the backend only through Tauri `commands`. |
| **Commands** | `commands.rs` | Tauri command handlers; thin glue that validates input and calls services. |
| **Services** | `services/` | Business logic: `auth_service`, `contact_service`, `message_service`, `node_service`, `file_transfer`, `notification_service`. |
| **Domain** | `domain/` | Core models: `node`, `node_registry`, `message`, `models`. |
| **Network** | `network/` | Transport: `udp` (discovery/heartbeat), `tcp` (chat), `reconnection`, `runtime`. |
| **Storage** | `storage/`, `contacts/`, `identity/`, `crypto/` | Encrypted-at-rest persistence and identity/keys. |

## Entities

| Entity | Description |
|--------|-------------|
| **Node** | This running instance: identity, listen port, peer state behind `Arc<Mutex<_>>`. |
| **Peer / DiscoveredNode** | Another node found via UDP broadcast; tracked in the shared `NodeRegistry` (keyed by address, indexed by `user_id`). |
| **Contact** | A saved peer, persisted in the per-user contacts store (encrypted with the user's RSA key). |
| **Message** | `Discovery`, `Heartbeat`, `Chat`, `ContactRequest/Response`, and `File*` variants, serialized as JSON over TCP. |
| **Transfer** | A resumable, chunked, SHA-256-verified file transfer with a per-transfer manifest. |

## Networking

- **Discovery**: UDP broadcast (default port 9999) advertises name/username/port/`user_id`; received peers go into the shared `NodeRegistry`. A self-filter (by `user_id`, falling back to IP+port) keeps a node from discovering itself.
- **Chat**: peers connect over TCP and exchange newline-delimited JSON `Message`s. Outbound connections are established lazily (on send) and by the `ReconnectionManager`.
- **Liveness**: heartbeats refresh registry timestamps; a cleanup task marks timed-out nodes offline so reconnection can act on them.

## Key invariants

- One shared `NodeRegistry` is the single source of truth for peer state (discovery, cleanup, and reconnection all act on it).
- A node never registers itself as a peer.
- Per-user secrets are encrypted at rest; the contacts RSA key is unlocked once at login into an in-memory keyring and evicted at logout.
- File transfers are accepted only if the received bytes match the sender-advertised checksum.

## What Mesh-Talk does NOT own

- **No central server / directory** — discovery is broadcast-only, scoped to the LAN.
- **No message relay** — messages go peer-to-peer; offline peers are not buffered by a third party.
