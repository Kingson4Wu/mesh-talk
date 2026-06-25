import { test, expect } from "@playwright/test";

// Step 4 of the mobile-PWA plan: encrypted persistence in the browser. The wasm core seals a
// device identity under a password; the JS keystore persists the opaque blob in IndexedDB. This
// proves the identity survives a full page reload (new wasm instance) and that a wrong password
// is rejected — i.e. browser persistence works the same way the desktop file keystore does.

// PBKDF2 (600k rounds) runs several times in wasm here; give it room.
test.setTimeout(60_000);

// Built from parts so TS / the bundler don't statically resolve this runtime browser path.
const KEYSTORE = "/src/lib/" + "browserKeystore.ts";

test("identity keystore persists across reloads (IndexedDB + wasm)", async ({
  page,
}) => {
  await page.goto("/");

  // First visit: no identity yet → create + persist one.
  const first = await page.evaluate(async (mod) => {
    const k = await import(/* @vite-ignore */ mod);
    const had = await k.hasIdentity();
    const fp = await k.loadOrCreateIdentity("correct horse battery");
    return { had, fp };
  }, KEYSTORE);
  expect(first.had).toBe(false);
  expect(first.fp).toMatch(/^[0-9a-f]+$/);

  // Reload → a fresh wasm instance + page. The same identity must load from IndexedDB.
  await page.reload();
  const second = await page.evaluate(async (mod) => {
    const k = await import(/* @vite-ignore */ mod);
    const had = await k.hasIdentity();
    const fp = await k.loadOrCreateIdentity("correct horse battery");
    return { had, fp };
  }, KEYSTORE);
  expect(second.had).toBe(true);
  expect(second.fp).toBe(first.fp);

  // Wrong password must be rejected (AEAD fails).
  const rejected = await page.evaluate(async (mod) => {
    const k = await import(/* @vite-ignore */ mod);
    try {
      await k.loadOrCreateIdentity("wrong password");
      return false;
    } catch {
      return true;
    }
  }, KEYSTORE);
  expect(rejected).toBe(true);
});
