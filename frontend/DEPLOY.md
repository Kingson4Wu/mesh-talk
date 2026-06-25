# Deploying the Mesh-Talk PWA

The browser/mobile build is a plain static site (`frontend/dist`: HTML + JS + the ~470 KB wasm).
It is just the **one-time app download** — once a phone loads it (and "Add to Home Screen" on
iOS), the service worker caches it and the app runs locally. After that, messaging is
peer-to-peer over **WebRTC**, end-to-end encrypted; the host is never in the message path.

## Recommended: one-command pure-LAN hub (no TLS, no GitHub Pages)

A single command hosts the app **and** runs the signaling relay on the LAN:

```sh
# build the PWA once
cd frontend && npm run build && cd ..
# host it + run the relay (any machine on the Wi-Fi / hotspot)
cargo run -p mesh-talk-signal --release -- \
  --serve-dir frontend/dist --port 47480 --http-port 8080
```

It prints a ready-to-use join URL with your LAN IP, e.g.:

```
→ open or QR this on a phone (same Wi-Fi): http://192.168.1.10:8080/?relay=ws://192.168.1.10:47480&room=mesh-talk
```

On each phone: open that URL in the browser (turn it into a QR with any generator, or scan one
shown by the host) → the app downloads, **Add to Home Screen**, and the `?relay=…&room=…` in the
URL auto-configures the gateway, so it connects with no typing. Two phones on the same room then
discover each other and exchange encrypted DMs.

**Why this avoids certificates:** the app is served over `http://` (not HTTPS), so the browser is
allowed to open the plain `ws://` relay directly. WebRTC itself never needed a certificate; the
only thing that *would* require one is connecting a `ws://` relay from an **HTTPS** page (mixed
content) — which only happens if you host the app on HTTPS GitHub Pages (see below). Pure LAN over
http has no such issue.

## Alternative: GitHub Pages (install from anywhere)

If you want to install the app from the public internet rather than a LAN host:

1. Repo → Settings → Pages → Source: **GitHub Actions** (one time).
2. Run the **"Deploy PWA to GitHub Pages"** workflow → publishes to `https://<owner>.github.io/mesh-talk/`.

Because Pages is HTTPS, the app then needs a **`wss://`** relay (a plain `ws://` LAN relay is
blocked as mixed content). Put `mesh-talk-signal` behind a TLS reverse proxy (e.g. Caddy, which
gets a cert automatically) and point Settings → Gateway at `wss://your-host`. This only affects
peer **discovery** — not the download or the encrypted WebRTC traffic.

## iOS notes

- Install is Safari → Add to Home Screen (no App Store, no packaging, no Apple developer account).
- iOS may evict a PWA's IndexedDB after ~7 days unused; the identity is recoverable by signing in
  again with the password, but un-synced local history could be lost.
- iOS PWAs have no true background execution — the background sync loop runs only while the app is
  open/foreground.
