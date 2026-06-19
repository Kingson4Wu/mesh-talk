# Mesh-Talk Project Structure

```
mesh-talk/
├── src-tauri/                # Rust backend (Tauri shell + the serverless node)
│   ├── src/
│   │   ├── main.rs           # `mesh-talk` desktop binary → lib::run_tauri
│   │   ├── lib.rs            # app entry: Tauri setup + IPC handler registration
│   │   ├── commands.rs       # auth IPC (login/logout/register) + redesign-node bridge
│   │   ├── redesign_commands.rs # all messaging IPC (redesign_*)
│   │   ├── events.rs         # Tauri events emitted to the UI (redesign-*)
│   │   ├── state.rs          # AppState: auth service + in-memory session
│   │   ├── tray.rs           # system tray (show/hide/quit)
│   │   ├── services/         # auth_service + user + common (the only stateful service)
│   │   ├── identity/         # device + account keypairs, keystores, certificates
│   │   ├── transport/        # Noise_XX secure channel + identity auth
│   │   ├── discovery/        # signed UDP announce + roster
│   │   ├── eventlog/         # content-addressed event DAG, persistence, sync protocol
│   │   ├── ratchet/          # Double Ratchet (DM forward secrecy)
│   │   ├── dm.rs             # X3DH sealed-box (channel-key + file-manifest sealing)
│   │   ├── channel/          # group sender-key ratchet + membership/epochs
│   │   ├── file/             # chunked file crypto + manifest
│   │   ├── postoffice/       # store-and-forward relay + election
│   │   ├── node/             # the Node: wires it all together (+ runtime, pairing, …)
│   │   ├── storage/          # at-rest encryption (PBKDF2 + AES-GCM), file manager
│   │   ├── logger/ perf/ error.rs   # cross-cutting
│   │   └── bin/mesh-talk-node.rs    # headless node CLI (+ --post-office relay mode)
│   └── Cargo.toml
├── frontend/                 # Vue 3 + Pinia + Vite
│   └── src/
│       ├── views/redesign/RedesignChatView.vue  # the app UI ("/")
│       ├── views/auth/LoginView.vue
│       ├── stores/appStore.js                   # auth/session store
│       └── services/api.js                      # authAPI + redesignAPI (Tauri invoke)
├── docs/ARCHITECTURE.md      # ← authoritative architecture reference
├── docs/superpowers/         # design specs + implementation plans (history)
├── specifications/           # this dir: overview + process/convention docs
└── scripts/                  # check-health.sh (gate), setup-hooks.sh, …
```

> Module responsibilities and how they interact are described in
> **[`docs/ARCHITECTURE.md`](../docs/ARCHITECTURE.md)**.
