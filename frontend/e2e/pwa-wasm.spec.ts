import { test, expect } from "@playwright/test";

// The PWA / browser path (NO Tauri mock): prove the protocol core, compiled to wasm, actually
// loads and runs in a real browser — the foundation of mobile-PWA support. Generates two device
// fingerprints via the wasm module and asserts they're well-formed, non-empty, and distinct
// (the getrandom/js RNG path produces fresh keys).
test("wasm protocol core runs in the browser", async ({ page }) => {
  await page.goto("/");
  const result = await page.evaluate(async () => {
    // Runtime browser path (Vite serves /src in dev) — built from parts so it isn't resolved
    // as a TS module / statically analyzed by the bundler.
    const path = "/src/wasm/" + "mesh_talk_wasm.js";
    const m = await import(/* @vite-ignore */ path);
    await m.default();
    return [
      m.generate_identity_fingerprint() as string,
      m.generate_identity_fingerprint() as string,
    ];
  });
  const [a, b] = result;
  expect(a).toMatch(/^[0-9a-f]+$/);
  expect(a.length).toBeGreaterThan(8);
  expect(b).toMatch(/^[0-9a-f]+$/);
  expect(a).not.toBe(b);
});

// The installable-PWA surface is present and served.
test("manifest + service worker are served", async ({ page }) => {
  const manifest = await page.request.get("/manifest.webmanifest");
  expect(manifest.ok()).toBeTruthy();
  expect((await manifest.json()).name).toBe("Mesh-Talk");
  const sw = await page.request.get("/sw.js");
  expect(sw.ok()).toBeTruthy();
});
