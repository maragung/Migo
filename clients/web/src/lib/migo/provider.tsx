'use client';

/**
 * The client lifecycle, owned by one React context.
 *
 * This provider is the single place a {@link MigoClient} is constructed, brought online, and torn down.
 * It handles the three ways a session begins — register a new account, log an existing one in, or
 * resume a persisted session on a return visit — and the persistence that makes resume possible:
 *
 *   - The key-store snapshot (this device's private identity) is written to IndexedDB after every
 *     operation that can mutate it, so a reload keeps the identity and its ability to read history.
 *   - The grant (the session tokens) is written to IndexedDB and, when its access token has expired by
 *     the time we return, refreshed over REST before the socket is opened.
 *
 * Everything realtime flows through the SDK's gateway WebSocket; this module never polls. The only
 * timers involved are the SDK's own heartbeat and reconnect backoff.
 */

import { createContext, useCallback, useEffect, useRef, useState } from 'react';
import type { ReactNode } from 'react';

import { BootstrapClient, KeyStore, MigoClient, PresenceState } from '@migo/sdk';
import type { ConnectionState, Grant, Id, RegisterParams } from '@migo/sdk';

import { config } from '@/lib/config.js';
import { friendlyError } from '@/lib/migo/errors.js';
import { deviceDisplayName, webHello } from '@/lib/migo/hello.js';
import {
  clearKeyStoreSnapshot,
  loadKeyStoreSnapshot,
  saveKeyStoreSnapshot,
} from '@/lib/storage/keystore-store.js';
import { clearSession, loadSession, saveSession } from '@/lib/storage/session-store.js';

/** The overall authentication lifecycle, distinct from the transport's {@link ConnectionState}. */
export type AuthStatus = 'initializing' | 'anonymous' | 'connecting' | 'ready';

/** The fields a registration form collects. */
export interface RegisterForm {
  username: string;
  password: string;
  email?: string;
  phone?: string;
  country?: string;
}

/** The fields a login form collects. */
export interface LoginForm {
  identifier: string;
  password: string;
}

/** What the rest of the app reads and calls. */
export interface MigoContextValue {
  status: AuthStatus;
  connectionState: ConnectionState;
  accountId: Id | null;
  deviceId: Id | null;
  error: string | null;
  /** Bumped whenever the session reset (a fresh, non-resumed reconnect), so views can resync. */
  resetNonce: number;
  /** The live client once {@link status} is `ready`, else `null`. */
  client: MigoClient | null;
  register: (form: RegisterForm) => Promise<void>;
  login: (form: LoginForm) => Promise<void>;
  logout: () => Promise<void>;
}

export const MigoContext = createContext<MigoContextValue | null>(null);

/** Refresh the access token if it expires within this window, to avoid a doomed first request. */
const REFRESH_SKEW_MS = 30_000;

export function MigoProvider({ children }: { children: ReactNode }): ReactNode {
  const [status, setStatus] = useState<AuthStatus>('initializing');
  const [connectionState, setConnectionState] = useState<ConnectionState>('closed');
  const [accountId, setAccountId] = useState<Id | null>(null);
  const [deviceId, setDeviceId] = useState<Id | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [resetNonce, setResetNonce] = useState(0);
  const [client, setClient] = useState<MigoClient | null>(null);

  const clientRef = useRef<MigoClient | null>(null);
  const inboundOffRef = useRef<(() => void) | null>(null);
  const persistScheduledRef = useRef(false);

  // --- key-store persistence (coalesced to at most once per microtask) ---

  const persistKeyStoreNow = useCallback(async (): Promise<void> => {
    const current = clientRef.current;
    if (current === null) {
      return;
    }
    try {
      await saveKeyStoreSnapshot(current.keyStore.snapshot());
    } catch {
      // A failed local persist is non-fatal: the in-memory key store stays authoritative for this run.
    }
  }, []);

  const scheduleKeyStorePersist = useCallback((): void => {
    if (persistScheduledRef.current) {
      return;
    }
    persistScheduledRef.current = true;
    queueMicrotask(() => {
      persistScheduledRef.current = false;
      void persistKeyStoreNow();
    });
  }, [persistKeyStoreNow]);

  // --- teardown ---

  const teardown = useCallback(async (): Promise<void> => {
    inboundOffRef.current?.();
    inboundOffRef.current = null;
    const current = clientRef.current;
    clientRef.current = null;
    setClient(null);
    if (current !== null) {
      try {
        await current.disconnect();
      } catch {
        // Disconnect is best-effort; the object graph is dropped regardless.
      }
    }
  }, []);

  // --- inbound wiring: keep the persisted key store current and top up prekeys ---

  const wireInbound = useCallback(
    (target: MigoClient): void => {
      inboundOffRef.current?.();
      // Receiving a first message from a new peer consumes one of our one-time prekeys, mutating the key
      // store; persist the new snapshot and replenish the pool if it has run low (which republishes).
      inboundOffRef.current = target.messaging.onMessage(() => {
        scheduleKeyStorePersist();
        void target
          .replenishPrekeys()
          .then((published) => {
            if (published) {
              scheduleKeyStorePersist();
            }
          })
          .catch(() => {
            // Replenishment is best-effort; a failure is retried on the next inbound message.
          });
      });
    },
    [scheduleKeyStorePersist],
  );

  // --- client construction ---

  const buildClient = useCallback(
    (options: { keyStore?: KeyStore; deviceId?: Id }): MigoClient => {
      const created = MigoClient.create({
        baseUrl: config.apiBaseUrl,
        gatewayUrl: config.gatewayUrl,
        hello: webHello(),
        deviceDisplayName: deviceDisplayName(),
        ...(options.keyStore ? { keyStore: options.keyStore } : {}),
        ...(options.deviceId ? { deviceId: options.deviceId } : {}),
        onStateChange: (next) => setConnectionState(next),
        onReset: () => {
          setResetNonce((value) => value + 1);
          scheduleKeyStorePersist();
        },
        onEventError: (op) => {
          // Log only the opcode: payloads and any token/ciphertext they carry must never be logged.
          console.warn('[migo] failed to handle inbound event for opcode', op);
        },
      });
      clientRef.current = created;
      return created;
    },
    [scheduleKeyStorePersist],
  );

  const markReady = useCallback((grant: Grant): void => {
    setAccountId(grant.accountId);
    setDeviceId(grant.deviceId);
    setClient(clientRef.current);
    setError(null);
    setStatus('ready');
  }, []);

  // --- register / login / logout ---

  const register = useCallback(
    async (form: RegisterForm): Promise<void> => {
      setError(null);
      setStatus('connecting');
      try {
        const created = buildClient({});
        const params: Omit<RegisterParams, 'device'> = {
          username: form.username.trim(),
          password: form.password,
        };
        if (form.email?.trim()) {
          params.email = form.email.trim();
        }
        if (form.phone?.trim()) {
          params.phone = form.phone.trim();
        }
        if (form.country?.trim()) {
          params.country = form.country.trim();
        }
        const grant = await created.register(params);
        await Promise.all([
          saveSession({ grant }),
          saveKeyStoreSnapshot(created.keyStore.snapshot()),
        ]);
        wireInbound(created);
        void created.presence.setPresence(PresenceState.Online).catch(() => {});
        markReady(grant);
      } catch (cause) {
        await teardown();
        setStatus('anonymous');
        setError(friendlyError(cause));
        throw cause;
      }
    },
    [buildClient, markReady, teardown, wireInbound],
  );

  const login = useCallback(
    async (form: LoginForm): Promise<void> => {
      setError(null);
      setStatus('connecting');
      try {
        const created = buildClient({});
        const grant = await created.login({
          identifier: form.identifier.trim(),
          password: form.password,
        });
        await Promise.all([
          saveSession({ grant }),
          saveKeyStoreSnapshot(created.keyStore.snapshot()),
        ]);
        wireInbound(created);
        void created.presence.setPresence(PresenceState.Online).catch(() => {});
        markReady(grant);
      } catch (cause) {
        await teardown();
        setStatus('anonymous');
        setError(friendlyError(cause));
        throw cause;
      }
    },
    [buildClient, markReady, teardown, wireInbound],
  );

  const logout = useCallback(async (): Promise<void> => {
    const grant = clientRef.current?.connected ? clientRef.current.grant : null;
    // Best-effort server-side revocation before dropping the local session.
    if (grant) {
      try {
        await new BootstrapClient(config.apiBaseUrl).logout(grant.accessToken, grant.sessionId);
      } catch {
        // Revocation is best-effort; the local session is cleared regardless.
      }
    }
    await teardown();
    await Promise.all([clearSession(), clearKeyStoreSnapshot()]);
    setAccountId(null);
    setDeviceId(null);
    setConnectionState('closed');
    setError(null);
    setStatus('anonymous');
  }, [teardown]);

  // --- resume a persisted session on mount ---

  useEffect(() => {
    let cancelled = false;

    async function restore(): Promise<void> {
      const [session, snapshot] = await Promise.all([loadSession(), loadKeyStoreSnapshot()]);
      if (!session || !snapshot) {
        if (!cancelled) {
          setStatus('anonymous');
        }
        return;
      }

      const keyStore = KeyStore.restore(snapshot);
      const created = buildClient({ keyStore, deviceId: session.grant.deviceId });

      let grant = session.grant;
      if (grant.accessExpiresAtMs <= Date.now() + REFRESH_SKEW_MS) {
        grant = await new BootstrapClient(config.apiBaseUrl).refresh({
          refreshToken: grant.refreshToken,
          deviceId: grant.deviceId,
        });
        await saveSession({ grant });
      }

      await created.resume(grant);
      wireInbound(created);
      void created.presence.setPresence(PresenceState.Online).catch(() => {});
      if (!cancelled) {
        markReady(grant);
      }
    }

    void restore().catch(async () => {
      // A failed resume (revoked or expired beyond refresh, or a corrupt store) drops back to signed-out.
      await teardown();
      await Promise.all([clearSession(), clearKeyStoreSnapshot()]);
      if (!cancelled) {
        setStatus('anonymous');
      }
    });

    return () => {
      cancelled = true;
    };
    // Run exactly once on mount; the callbacks it uses are stable.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // --- presence follows tab visibility (event-driven, never polled) ---

  useEffect(() => {
    if (status !== 'ready') {
      return;
    }
    function onVisibility(): void {
      const current = clientRef.current;
      if (!current) {
        return;
      }
      const next =
        document.visibilityState === 'visible' ? PresenceState.Online : PresenceState.Away;
      void current.presence.setPresence(next).catch(() => {});
    }
    document.addEventListener('visibilitychange', onVisibility);
    return () => document.removeEventListener('visibilitychange', onVisibility);
  }, [status]);

  const value: MigoContextValue = {
    status,
    connectionState,
    accountId,
    deviceId,
    error,
    resetNonce,
    client,
    register,
    login,
    logout,
  };

  return <MigoContext.Provider value={value}>{children}</MigoContext.Provider>;
}
