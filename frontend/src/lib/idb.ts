// Minimal IndexedDB key→bytes store for the browser / installed PWA backend.
//
// The desktop app persists encrypted state on the filesystem (mesh-talk-core's record log); the
// browser has no filesystem, so it persists the same kind of opaque, already-encrypted blobs
// here. Dependency-free (raw IndexedDB) and self-contained on purpose. See
// docs/superpowers/specs/2026-06-23-mobile-pwa-design.md.

const DB_NAME = "mesh-talk";
const DB_VERSION = 1;
const STORE = "kv";

let dbPromise: Promise<IDBDatabase> | null = null;

function openDb(): Promise<IDBDatabase> {
  if (!dbPromise) {
    dbPromise = new Promise((resolve, reject) => {
      const req = indexedDB.open(DB_NAME, DB_VERSION);
      req.onupgradeneeded = () => {
        if (!req.result.objectStoreNames.contains(STORE)) {
          req.result.createObjectStore(STORE);
        }
      };
      req.onsuccess = () => resolve(req.result);
      req.onerror = () => reject(req.error);
    });
  }
  return dbPromise;
}

function tx<T>(
  mode: IDBTransactionMode,
  run: (store: IDBObjectStore) => IDBRequest<T>,
): Promise<T> {
  return openDb().then(
    (db) =>
      new Promise<T>((resolve, reject) => {
        const req = run(db.transaction(STORE, mode).objectStore(STORE));
        req.onsuccess = () => resolve(req.result);
        req.onerror = () => reject(req.error);
      }),
  );
}

/** Read the bytes stored at `key`, or null if absent. */
export async function idbGet(key: string): Promise<Uint8Array | null> {
  const v = await tx<unknown>("readonly", (s) => s.get(key));
  if (v == null) return null;
  return v instanceof Uint8Array ? v : new Uint8Array(v as ArrayBuffer);
}

/** Store `value` bytes at `key` (overwrites). */
export async function idbSet(key: string, value: Uint8Array): Promise<void> {
  await tx("readwrite", (s) => s.put(value, key));
}

/** Delete whatever is stored at `key` (no-op if absent). */
export async function idbDelete(key: string): Promise<void> {
  await tx("readwrite", (s) => s.delete(key));
}
