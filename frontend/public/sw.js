// Mesh-Talk PWA service worker — an app-shell offline cache.
//
// Navigations (the HTML page) are NETWORK-FIRST so a new deploy is picked up immediately: the fresh
// index.html references the current content-hashed assets, which are then fetched fresh. Hashed
// assets are cache-first (a new build = new URLs, so that's safe + fast + offline). Falls back to
// the cached shell when offline. It does NOT cache or proxy any protocol traffic — messaging is
// end-to-end encrypted through the wasm core, never the SW. Bump CACHE on shell-format changes.
const CACHE = "mesh-talk-shell-v2";
// Base path the SW is served under ("/" at the root, or e.g. "/mesh-talk/" on a GitHub Pages
// project site) — derived from the SW's own URL so the shell precache is correct either way.
const BASE = self.location.pathname.replace(/sw\.js$/, "");
const SHELL = [BASE, BASE + "index.html", BASE + "manifest.webmanifest"];

self.addEventListener("install", (event) => {
  event.waitUntil(
    caches
      .open(CACHE)
      .then((c) => c.addAll(SHELL))
      .then(() => self.skipWaiting()),
  );
});

self.addEventListener("activate", (event) => {
  event.waitUntil(
    caches
      .keys()
      .then((keys) =>
        Promise.all(keys.filter((k) => k !== CACHE).map((k) => caches.delete(k))),
      )
      .then(() => self.clients.claim()),
  );
});

// Cache a successful same-origin response, then return it.
function cachePut(req, res) {
  if (res.ok && res.type === "basic") {
    const copy = res.clone();
    caches.open(CACHE).then((c) => c.put(req, copy));
  }
  return res;
}

self.addEventListener("fetch", (event) => {
  const req = event.request;
  // Only handle same-origin GETs; let everything else hit the network untouched.
  if (req.method !== "GET" || new URL(req.url).origin !== self.location.origin) {
    return;
  }
  // App shell / navigations: NETWORK-FIRST, so a fresh deploy lands without a manual cache clear.
  // Offline → the cached page (or the precached index.html).
  if (req.mode === "navigate") {
    event.respondWith(
      fetch(req)
        .then((res) => cachePut(req, res))
        .catch(() =>
          caches.match(req).then((hit) => hit || caches.match(BASE + "index.html")),
        ),
    );
    return;
  }
  // Hashed assets (JS/wasm/css): cache-first — content-addressed, so a new build = new URLs.
  event.respondWith(
    caches.match(req).then(
      (hit) =>
        hit ||
        fetch(req)
          .then((res) => cachePut(req, res))
          .catch(() => caches.match(BASE + "index.html")),
    ),
  );
});
