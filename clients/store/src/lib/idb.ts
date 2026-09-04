/**
 * A tiny promise-based wrapper over a single IndexedDB key/value store.
 *
 * Verbatim contract with the web client's `lib/storage/idb.ts`: the same `migo` database, the
 * same `kv` store, the same keys. That sameness is the whole point — the store app is served
 * from the same origin as the web client, so the two share one IndexedDB, and the session the
 * web client persisted is the session the store resumes. No second sign-in, no credential
 * hand-off, no token in a URL.
 *
 * IndexedDB rather than Web Storage because the values include the device's private key
 * material as raw byte arrays; structured-clone round-trips `Uint8Array` and `bigint`
 * losslessly, and the audit rules forbid a private key from ever touching localStorage.
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
