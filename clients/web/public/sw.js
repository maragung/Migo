/*
 * Migo web client service worker.
 *
 * Scope is deliberately narrow: it makes the app shell available offline and speeds repeat loads of
 * static assets. It NEVER touches the realtime path. The gateway is a WebSocket, which a service worker
 * does not intercept; REST calls to the API origin are cross-origin here and are passed straight
 * through. Only same-origin GET requests are considered, so no request carrying a token or ciphertext
 * is ever read or cached by this worker.
 */

const CACHE = 'migo-web-v1';
const PRECACHE = ['/', '/manifest.webmanifest', '/icons/icon.svg', '/icons/maskable.svg'];

self.addEventListener('install', (event) => {
  event.waitUntil(
    caches
      .open(CACHE)
      // Precache best-effort: a single missing entry must not fail the whole install.
      .then((cache) => Promise.allSettled(PRECACHE.map((url) => cache.add(url))))
      .then(() => self.skipWaiting()),
  );
});

self.addEventListener('activate', (event) => {
  event.waitUntil(
    caches
      .keys()
      .then((keys) =>
        Promise.all(keys.filter((key) => key !== CACHE).map((key) => caches.delete(key))),
      )
      .then(() => self.clients.claim()),
  );
});

self.addEventListener('fetch', (event) => {
  const request = event.request;
  if (request.method !== 'GET') {
    return;
  }
  const url = new URL(request.url);
  // Same-origin only: the API and gateway live elsewhere and must never be served from cache.
  if (url.origin !== self.location.origin) {
    return;
  }

  // Navigations: network-first, falling back to the cached shell when offline.
  if (request.mode === 'navigate') {
    event.respondWith(
      fetch(request)
        .then((response) => {
          const copy = response.clone();
          void caches.open(CACHE).then((cache) => cache.put(request, copy));
          return response;
        })
        .catch(() => caches.match(request).then((cached) => cached || caches.match('/'))),
    );
    return;
  }

  // Static assets (the Next build output and icons): cache-first, then fill the cache on a miss.
  event.respondWith(
    caches.match(request).then((cached) => {
      if (cached) {
        return cached;
      }
      return fetch(request).then((response) => {
        if (
          response.ok &&
          (url.pathname.startsWith('/_next/') || url.pathname.startsWith('/icons/'))
        ) {
          const copy = response.clone();
          void caches.open(CACHE).then((cache) => cache.put(request, copy));
        }
        return response;
      });
    }),
  );
});
