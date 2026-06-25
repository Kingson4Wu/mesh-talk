// Persistent PWA node: a WasmNode with a durable identity (a password-sealed keystore), a durable
// event log, and durable DM state (ratchet sessions + decrypted plaintext + peer roster), all in
// IndexedDB — so identity, messages, and encrypted conversations survive a reload (the browser
// analog of the desktop node's on-disk keystore + log). See
// docs/superpowers/specs/2026-06-23-mobile-pwa-design.md.

import { loadWasm } from "./wasm";
import { idbGet, idbSet, idbDelete } from "./idb";

const KEYSTORE_KEY = "node-keystore";
const LOG_KEY = "node-log";
const DM_STATE_KEY = "node-dm-state";
const SESSION_KEY = "saved-session";

// "Stay signed in" for the PWA. The desktop keeps the resume secret in the OS keychain; a browser
// has no such store, so we persist it in IndexedDB — the same place the (password-sealed) keystore
// already lives, so it's the same device-level trust boundary, traded for never re-entering the
// password. Cleared on explicit sign-out.
export async function saveSession(
  username: string,
  password: string,
): Promise<void> {
  const bytes = new TextEncoder().encode(
    JSON.stringify({ username, password }),
  );
  await idbSet(SESSION_KEY, bytes);
}
export async function loadSession(): Promise<{
  username: string;
  password: string;
} | null> {
  const bytes = await idbGet(SESSION_KEY);
  if (!bytes) return null;
  try {
    return JSON.parse(new TextDecoder().decode(bytes));
  } catch {
    return null;
  }
}
export async function clearSession(): Promise<void> {
  await idbDelete(SESSION_KEY);
}

// Held for the session so persistNode can re-seal the DM state (the keystore password). The
// identity is already unsealed in memory, so this is no weaker than holding the node itself.
let currentPassword = "";

/**
 * Load the node: open its identity from the IndexedDB keystore (creating + persisting one on
 * first run), then restore its event log and DM state. The same `password` yields the same
 * identity across reloads.
 */
export async function loadNode(password: string) {
  currentPassword = password;
  const m = await loadWasm();
  let keystore = await idbGet(KEYSTORE_KEY);
  if (!keystore) {
    keystore = m.create_identity_keystore(password);
    await idbSet(KEYSTORE_KEY, keystore);
  }
  const node = m.WasmNode.from_keystore(keystore, password);
  const log = await idbGet(LOG_KEY);
  if (log) node.restore(log);
  const dmState = await idbGet(DM_STATE_KEY);
  if (dmState) {
    try {
      node.restore_dm_state(dmState, password);
    } catch {
      // Wrong password or a format change — start with empty DM state rather than fail to load.
    }
  }
  return node;
}

/** Persist a node's event log + DM state to IndexedDB (call after send/sync). */
export async function persistNode(node: {
  snapshot(): Uint8Array;
  dm_state_snapshot(password: string): Uint8Array;
}): Promise<void> {
  await idbSet(LOG_KEY, node.snapshot());
  if (currentPassword)
    await idbSet(DM_STATE_KEY, node.dm_state_snapshot(currentPassword));
}
