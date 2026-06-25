// Browser / PWA device-identity keystore.
//
// The analog of mesh-talk-core's native file keystore: the wasm core seals a generated device
// identity under the user's password (PBKDF2 → AES-256-GCM), and the resulting opaque blob is
// persisted in IndexedDB. Plaintext keys exist only inside wasm — IndexedDB only ever holds
// ciphertext. See docs/superpowers/specs/2026-06-23-mobile-pwa-design.md.

import { loadWasm } from "./wasm";
import { idbGet, idbSet, idbDelete } from "./idb";

const KEYSTORE_KEY = "device-keystore";

/** True once an identity keystore has been persisted in this browser. */
export async function hasIdentity(): Promise<boolean> {
  return (await idbGet(KEYSTORE_KEY)) !== null;
}

/**
 * Load the existing device identity, or create + persist a new one. Returns the device
 * fingerprint (user id). Rejects if a stored keystore exists but the password is wrong.
 */
export async function loadOrCreateIdentity(password: string): Promise<string> {
  const m = await loadWasm();
  const existing = await idbGet(KEYSTORE_KEY);
  if (existing) {
    return m.open_identity_keystore(existing, password);
  }
  const blob = m.create_identity_keystore(password);
  await idbSet(KEYSTORE_KEY, blob);
  return m.open_identity_keystore(blob, password);
}

/** Forget the stored identity (e.g. "sign out / reset this device"). */
export async function clearIdentity(): Promise<void> {
  await idbDelete(KEYSTORE_KEY);
}
