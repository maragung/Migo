'use client';

import { useEffect } from 'react';

/**
 * Registers the service worker in production only.
 *
 * The worker (see public/sw.js) caches the app shell and static assets for offline loads; it never touches
 * the API or gateway. In development it is intentionally skipped so code changes are never served stale.
 */
export function SwRegister(): null {
  useEffect(() => {
    if (process.env.NODE_ENV !== 'production' || !('serviceWorker' in navigator)) {
      return;
    }
    const register = (): void => {
      navigator.serviceWorker.register('/sw.js').catch(() => {
        // A failed registration only means no offline caching; the app still works online.
      });
    };
    window.addEventListener('load', register);
    return () => window.removeEventListener('load', register);
  }, []);

  return null;
}
