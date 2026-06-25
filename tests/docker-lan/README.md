# Docker LAN simulation — phone sees a LAN PC via a hub, across separate hosts

The full reported flow, with each node on its **own container IP** (real cross-host UDP discovery
over a `/24` bridge — not loopback), run end to end without any human in the loop:

- **relay** — `mesh-talk-signal` (also serves the built PWA over http for the phone)
- **PC2** — a plain desktop node (`mesh-talk-runtime-node`, the real `NodeRuntime`)
- **PC1** — a desktop node on the same LAN that ALSO hosts the gateway hub
- **phone** — a real headless Chromium (Playwright) in its own container, loading the PWA + the
  real wasm, connecting only to the relay

It asserts two things:
1. **PHASE 1** — PC1's gossiped presence directory `[7;32]` picks up PC2 (cross-host LAN discovery +
   directory gossip).
2. **PHASE 2** — the phone's sidebar shows PC2, a LAN PC it never connected to, bridged through PC1.

## Run it

```sh
# 1. build the PWA (so the relay can serve it)
( cd frontend && npm run build:wasm && npm run build )   # build:wasm needs the rustup toolchain on PATH

# 2. build the node + relay image (Linux binaries)
docker build -f tests/docker-lan/Dockerfile -t mesh-lan:latest .

# 3. run the full flow (PHASE1_ONLY=1 skips the phone)
bash tests/docker-lan/run.sh
```

Expected tail: `ALL OK: the phone saw PC2 (a LAN PC it never connected to) via the hub`.

The desktop nodes run the actual `NodeRuntime` (`mesh-talk-runtime-node`) — the same code the Tauri
app runs (discovery, presence gossip incl. gossip-on-newcomer, `run_gateway_hub`). The only thing
this can't reproduce is real Wi-Fi radio; the cross-host networking is real.
