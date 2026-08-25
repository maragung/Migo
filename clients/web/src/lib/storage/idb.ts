/**
 * A tiny promise-based wrapper over a single IndexedDB key/value store.
 *
 * IndexedDB is chosen deliberately over Web Storage. The key-store snapshot this app persists contains
 * this device's private key material as raw byte arrays, and the audit rules forbid a private key from
 * ever touching localStorage, sessionStorage, or a cookie. IndexedDB is the sanctioned store: its
 * structured-clone serialization round-trips `Uint8Array` and `bigint` losslessly, so seeds and the
 * grant's capability bitset survive without a fragile hand-rolled JSON encoding.
 *
 * All access is guarded for a non-browser context (SSR, prerender): the helpers reject cleanly when
 * `indexedDB` is absent, and every caller runs them only from client-side effects.
 */

const DB_NAME = 'migo';
const DB_VERSION = 1;
const STORE = 'kv';

function hasIndexedDb(): boolean {
  return typeof indexedDB !== 'undefined';
}

function openDb(): Promise<IDBDatabase> {
  return new Promise((resolve, reject) => {
    if (!hasIndexedDb()) {
      reject(new Error('indexedDB is unavailable in this environment'));
      return;
    }
    const request = indexedDB.open(DB_NAME, DB_VERSION);
    request.onupgradeneeded = () => {
      const db = request.result;
      if (!db.objectStoreNames.contains(STORE)) {
        db.createObjectStore(STORE);
      }
    };
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => reject(request.error ?? new Error('failed to open IndexedDB'));
  });
}

function withStore<T>(
  mode: IDBTransactionMode,
  run: (store: IDBObjectStore) => IDBRequest<T>,
): Promise<T> {
  return openDb().then(
    (db) =>
      new Promise<T>((resolve, reject) => {
        const tx = db.transaction(STORE, mode);
        const request = run(tx.objectStore(STORE));
        request.onsuccess = () => resolve(request.result);
        request.onerror = () => reject(request.error ?? new Error('IndexedDB request failed'));
        tx.oncomplete = () => db.close();
        tx.onabort = () => reject(tx.error ?? new Error('IndexedDB transaction aborted'));
      }),
  );
}

/** Reads a value, or `undefined` when the key is absent. */
export async function idbGet<T>(key: string): Promise<T | undefined> {
  return withStore<T | undefined>(
    'readonly',
    (store) => store.get(key) as IDBRequest<T | undefined>,
  );
}

/** Writes a value, replacing any existing one. */
export async function idbSet<T>(key: string, value: T): Promise<void> {
  await withStore('readwrite', (store) => store.put(value as unknown as never, key));
}

/** Deletes a value; a no-op when the key is absent. */
export async function idbDelete(key: string): Promise<void> {
  await withStore('readwrite', (store) => store.delete(key));
}
