/**
 * Test doubles for the browser globals the web client touches.
 *
 * The client is a full client-side bundle, so its logic reaches for `indexedDB`, `localStorage`,
 * `document.cookie`, `window.location`, and `navigator` directly. Node provides none of the first
 * three, so a test that exercises persistence or navigation has to supply them. These doubles are
 * deliberately observable: the IndexedDB fake keeps its bytes in a plain Map a test can read back,
 * and the Web Storage / cookie fakes record every access and throw on any write — that is what lets a
 * test prove the strongest audit rule the client has, that private key material reaches IndexedDB and
 * nothing else.
 */

/** A recorded access to a storage surface that must never be written. */
export interface StorageAccess {
  surface: 'localStorage' | 'sessionStorage' | 'cookie';
  op: 'read' | 'write';
  key?: string;
  value?: string;
}

interface Restorable {
  restore: () => void;
}

/** A minimal request object mirroring the IDBRequest surface `idb.ts` uses. */
interface FakeRequest {
  result: unknown;
  error: unknown;
  onsuccess: (() => void) | null;
  onerror: (() => void) | null;
  onupgradeneeded: (() => void) | null;
}

interface FakeTx {
  error: unknown;
  oncomplete: (() => void) | null;
  onabort: (() => void) | null;
  objectStore: (name: string) => FakeStore;
}

interface FakeStore {
  get: (key: string) => FakeRequest;
  put: (value: unknown, key: string) => FakeRequest;
  delete: (key: string) => FakeRequest;
}

function newRequest(): FakeRequest {
  return { result: undefined, error: null, onsuccess: null, onerror: null, onupgradeneeded: null };
}

/**
 * An in-memory IndexedDB sufficient for `idb.ts`: one database, string keys, structured-clone values.
 *
 * Requests complete on a microtask, matching the real API's asynchrony — `idb.ts` assigns its
 * `onsuccess`/`onerror` handlers synchronously after issuing each request, so firing them any sooner
 * than the next microtask would miss them.
 */
export interface FakeIndexedDb extends Restorable {
  /** The backing bytes for the `kv` store, keyed exactly as the client stored them. */
  store: Map<string, unknown>;
}

export function installFakeIndexedDb(): FakeIndexedDb {
  const stores = new Map<string, Map<string, unknown>>();

  function makeStore(data: Map<string, unknown>, tx: FakeTx | null): FakeStore {
    const complete = (req: FakeRequest, apply: () => unknown): FakeRequest => {
      queueMicrotask(() => {
        req.result = apply();
        req.onsuccess?.();
        // A real transaction completes after its request; `idb.ts` closes the db on this.
        queueMicrotask(() => tx?.oncomplete?.());
      });
      return req;
    };
    return {
      // Real IndexedDB structured-clones on both write and read, so the fake does too: it makes the
      // round-trip lossy in exactly the ways the real store is (a returned value is a fresh clone, not
      // the stored reference) and rejects a value that is not structured-cloneable, as the store would.
      get: (key) => complete(newRequest(), () => structuredClone(data.get(key))),
      put: (value, key) =>
        complete(newRequest(), () => {
          data.set(key, structuredClone(value));
          return undefined;
        }),
      delete: (key) =>
        complete(newRequest(), () => {
          data.delete(key);
          return undefined;
        }),
    };
  }

  function makeDb(): unknown {
    return {
      objectStoreNames: { contains: (name: string) => stores.has(name) },
      createObjectStore: (name: string) => {
        const data = new Map<string, unknown>();
        stores.set(name, data);
        return makeStore(data, null);
      },
      transaction: (_name: string, _mode: string): FakeTx => {
        const tx: FakeTx = {
          error: null,
          oncomplete: null,
          onabort: null,
          objectStore: (storeName) =>
            makeStore(stores.get(storeName) ?? new Map<string, unknown>(), tx),
        };
        return tx;
      },
      close: () => undefined,
    };
  }

  const factory = {
    open: (_name: string, _version?: number) => {
      const req = newRequest();
      queueMicrotask(() => {
        const created = !stores.has('kv');
        req.result = makeDb();
        // The upgrade handler runs against the open request's `result` and creates the store.
        if (created) {
          req.onupgradeneeded?.();
        }
        req.onsuccess?.();
      });
      return req;
    },
  };

  const previous = Object.getOwnPropertyDescriptor(globalThis, 'indexedDB');
  Object.defineProperty(globalThis, 'indexedDB', {
    configurable: true,
    value: factory,
  });

  const ensured = new Map<string, unknown>();
  stores.set('kv', ensured);

  return {
    store: ensured,
    restore: () => {
      if (previous) {
        Object.defineProperty(globalThis, 'indexedDB', previous);
      } else {
        Reflect.deleteProperty(globalThis, 'indexedDB');
      }
    },
  };
}

/** Recording, write-refusing doubles for the three storage surfaces private keys must never touch. */
export interface RecordingWebStorage extends Restorable {
  accesses: StorageAccess[];
  /** Every write attempt across all three surfaces, for a single assertion that none occurred. */
  writes: () => StorageAccess[];
}

export function installRecordingWebStorage(): RecordingWebStorage {
  const accesses: StorageAccess[] = [];

  const makeStorage = (surface: 'localStorage' | 'sessionStorage'): Storage => {
    const throwOnWrite = (op: string): never => {
      throw new Error(`${surface}.${op} must never be called by the web client`);
    };
    return {
      getItem: (key: string) => {
        accesses.push({ surface, op: 'read', key });
        return null;
      },
      setItem: (key: string, value: string) => {
        accesses.push({ surface, op: 'write', key, value });
        throwOnWrite('setItem');
      },
      removeItem: (key: string) => {
        accesses.push({ surface, op: 'write', key });
        throwOnWrite('removeItem');
      },
      clear: () => {
        accesses.push({ surface, op: 'write' });
        throwOnWrite('clear');
      },
      key: () => null,
      length: 0,
    };
  };

  const define = (name: string, value: unknown): PropertyDescriptor | undefined => {
    const previous = Object.getOwnPropertyDescriptor(globalThis, name);
    Object.defineProperty(globalThis, name, { configurable: true, value });
    return previous;
  };

  const prevLocal = define('localStorage', makeStorage('localStorage'));
  const prevSession = define('sessionStorage', makeStorage('sessionStorage'));

  const fakeDocument = {
    get cookie(): string {
      accesses.push({ surface: 'cookie', op: 'read' });
      return '';
    },
    set cookie(value: string) {
      accesses.push({ surface: 'cookie', op: 'write', value });
      throw new Error('document.cookie must never be written by the web client');
    },
  };
  const prevDocument = Object.getOwnPropertyDescriptor(globalThis, 'document');
  Object.defineProperty(globalThis, 'document', {
    configurable: true,
    value: fakeDocument,
  });

  const restoreOne = (name: string, previous: PropertyDescriptor | undefined): void => {
    if (previous) {
      Object.defineProperty(globalThis, name, previous);
    } else {
      Reflect.deleteProperty(globalThis, name);
    }
  };

  return {
    accesses,
    writes: () => accesses.filter((access) => access.op === 'write'),
    restore: () => {
      restoreOne('localStorage', prevLocal);
      restoreOne('sessionStorage', prevSession);
      restoreOne('document', prevDocument);
    },
  };
}

/** A location whose `hash` normalises the way a browser's does (a leading `#`, or empty). */
interface FakeLocation {
  hash: string;
  pathname: string;
  search: string;
}

/** An EventTarget-backed `window`, so `addEventListener`/`dispatchEvent` behave for real. */
export interface FakeWindow extends Restorable {
  location: FakeLocation;
}

export function installFakeWindow(pathname = '/chat/', search = ''): FakeWindow {
  let hashValue = '';
  const location: FakeLocation = {
    get hash(): string {
      return hashValue;
    },
    set hash(value: string) {
      hashValue = value === '' ? '' : value.startsWith('#') ? value : `#${value}`;
    },
    pathname,
    search,
  };

  const target = new EventTarget();
  const win = {
    location,
    history: {
      replaceState: (_state: unknown, _title: string, url?: string | null): void => {
        // Mirrors the browser: a fragment-less URL clears the fragment.
        const raw = url ?? '';
        const hashIndex = raw.indexOf('#');
        location.hash = hashIndex >= 0 ? raw.slice(hashIndex) : '';
      },
    },
    addEventListener: target.addEventListener.bind(target),
    removeEventListener: target.removeEventListener.bind(target),
    dispatchEvent: target.dispatchEvent.bind(target),
  };

  const previous = Object.getOwnPropertyDescriptor(globalThis, 'window');
  Object.defineProperty(globalThis, 'window', {
    configurable: true,
    value: win,
  });

  return {
    location,
    restore: () => {
      if (previous) {
        Object.defineProperty(globalThis, 'window', previous);
      } else {
        Reflect.deleteProperty(globalThis, 'window');
      }
    },
  };
}

/** Overrides `navigator.language`/`userAgent` so locale and device-name derivation are deterministic. */
export function installNavigator(overrides: { language?: string; userAgent?: string }): Restorable {
  const previous = Object.getOwnPropertyDescriptor(globalThis, 'navigator');
  Object.defineProperty(globalThis, 'navigator', {
    configurable: true,
    value: {
      language: overrides.language,
      userAgent: overrides.userAgent ?? '',
    },
  });
  return {
    restore: () => {
      if (previous) {
        Object.defineProperty(globalThis, 'navigator', previous);
      } else {
        Reflect.deleteProperty(globalThis, 'navigator');
      }
    },
  };
}
