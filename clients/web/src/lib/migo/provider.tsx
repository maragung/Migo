'use client';

/**
 * The client lifecycle, owned by one React context.
 *
 * This provider is the single place a {@link MigoClient} is constructed, brought online, and torn down.
 * It handles the two ways a session begins — register a new account, or sign an existing one in from
 * its `.migo` key file — plus the resume of a persisted session on a return visit, and the persistence
 * that makes resume possible:
 *
 *   - The key-store snapshot (this device's private identity) is written to IndexedDB after every
 *     operation that can mutate it, so a reload keeps the identity and its ability to read history.
 *   - The grant (the session tokens) is written to IndexedDB and, when its access token has expired by
 *     the time we return, refreshed over REST before the socket is opened.
 *   - The user-chosen {@link ServerEndpoint} is also written to IndexedDB, so a self-hosted user does
 *     not have to type the address on every visit.
 *
 * Everything realtime flows through the SDK's gateway WebSocket; this module never polls. The only
 * timers involved are the SDK's own heartbeat and reconnect backoff.
 */

import { createContext, useCallback, useEffect, useRef, useState } from 'react';
import type { ReactNode } from 'react';

import { BootstrapClient, KeyStore, MigoClient, PresenceState, account } from '@migo/sdk';
import type {
  CaptchaProof,
  ConnectionState,
  Grant,
  Id,
  RegisterParams,
  ServerEndpoint,
} from '@migo/sdk';

import { defaultServerEndpoint } from '@/lib/config.js';
import { RESTORE_FAILED, containerFileName } from '@/lib/account-file.js';
import { friendlyError } from '@/lib/migo/errors.js';
import { deviceDisplayName, webHello } from '@/lib/migo/hello.js';
import { saveAccountRecord } from '@/lib/storage/account-record-store.js';
import { loadDeviceRecord, saveDeviceRecord } from '@/lib/storage/device-record-store.js';
import { keyFileId, saveKeyFile } from '@/lib/storage/key-file-store.js';
import {
  clearKeyStoreSnapshot,
  loadKeyStoreSnapshot,
  saveKeyStoreSnapshot,
} from '@/lib/storage/keystore-store.js';
import { clearSession, loadSession, saveSession } from '@/lib/storage/session-store.js';
import {
  clearServerEndpoint,
  loadServerEndpoint,
  saveServerEndpoint,
} from '@/lib/storage/server-endpoint-store.js';

/** The overall authentication lifecycle, distinct from the transport's {@link ConnectionState}. */
export type AuthStatus = 'initializing' | 'anonymous' | 'connecting' | 'ready';

/** The fields a registration form collects. */
export interface RegisterForm {
  username: string;
  passphrase: string;
  email?: string;
  gender?: number;
}

/**
 * The container refused to open, and the screen owes the user the one honest sentence for it.
 * Carries the line itself because the provider's generic error mapping has no way to tell a wrong
 * passphrase from a tampered file — §182 forbids the distinction — and must not bury it.
 */
export class AccountFileError extends Error {}

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
  /**
   * Seals the key store's current snapshot (identity, prekeys, root, tracked transactions) to
   * IndexedDB. The wallet flow calls this after mutating the tracked list, the same way the
   * inbound path does after a prekey is consumed.
   */
  persistKeyStore: () => void;
  register: (
    form: RegisterForm,
    server: ServerEndpoint,
    captcha: CaptchaProof | null,
  ) => Promise<void>;
  /**
   * Signs in from a `.migo` account file and its passphrase — the only sign-in the web client has.
   *
   * Opening the container yields the account root, and the sign-in is the ML-DSA identity ceremony
   * (§182), not a passphrase: a stored device record answers the login challenge (the ceremony reuses
   * the device this browser introduced last time), and a browser with no record — or one whose
   * device the server no longer knows — falls back to add-device, which mints a fresh device
   * credential and records it for next time. Either way the session runs as a founding-grade
   * device, because the root in the file reproduces the founding identity.
   */
  loginWithFile: (
    bytes: Uint8Array,
    passphrase: string,
    server: ServerEndpoint,
    fileName?: string,
  ) => Promise<void>;
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
  const serverRef = useRef<ServerEndpoint | null>(null);
  const inboundOffRef = useRef<(() => void) | null>(null);
  const persistScheduledRef = useRef(false);
  // The root a registration attempt minted but has not yet made stick (§12). A registration that
  // fails after the server heard it must be retried with the *same* root: a fresh one would be a
  // different identity key, which the server can only answer with USERNAME_TAKEN. Cleared the
  // moment the account exists locally — from then on the key-store snapshot is the root's home.
  const pendingRootRef = useRef<account.MigoRoot | null>(null);

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
    serverRef.current = null;
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
    (options: { keyStore?: KeyStore; deviceId?: Id; server: ServerEndpoint }): MigoClient => {
      const created = MigoClient.create({
        server: options.server,
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
      serverRef.current = options.server;
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

  // --- the account root's public material (§182) ---

  /**
   * Publishes the root's account material: the ML-DSA identity key, and the root's first wallet if
   * the server does not know it yet.
   *
   * Best-effort by design — a failure here is not a failed sign-in, because the calls are
   * idempotent and the next resume tries again. Only a device that holds the root enrols at all;
   * every additional device signs in with a fresh identity and no root, and the address is a pure
   * function of the root, so "which wallets exist" is server state, not a matter of opinion.
   */
  const enrolAccountMaterial = useCallback(
    async (created: MigoClient, grant: Grant, server: ServerEndpoint): Promise<void> => {
      const root = created.keyStore.root();
      if (root === null) {
        return;
      }
      try {
        const rest = new BootstrapClient(server);
        await rest.publishIdentityKey(grant.accessToken, {
          identityPublicKey: account.IdentityKey.fromRoot(root).publicKey(),
        });
        const known = new Set(
          (await rest.wallets(grant.accessToken)).map((wallet) => wallet.address),
        );
        const address = account.EvmWallet.fromRoot(root, 0).addressChecksummed();
        if (!known.has(address)) {
          await rest.registerWallet(grant.accessToken, { address, derivationIndex: 0 });
        }
      } catch {
        // Deliberately quiet: the material publishes again on the next resume.
      }
    },
    [],
  );

  // --- register / login / logout ---

  const register = useCallback(
    async (
      form: RegisterForm,
      server: ServerEndpoint,
      captcha: CaptchaProof | null,
    ): Promise<void> => {
      setError(null);
      setStatus('connecting');
      // Persist the new endpoint *before* opening a socket, so a mid-flight failure can be retried
      // against the same server without the form losing the address the user just typed.
      try {
        await saveServerEndpoint(server);
      } catch {
        // Persistence is best-effort: a failed write will not block the in-flight attempt.
      }
      try {
        // A registration is the founding device of a brand-new account (§182): the root is minted
        // here, the E2EE identity is derived from the root's E2EE domain, and both seal into the
        // snapshot below — which is what a `.migo` container can later be rebuilt from.
        //
        // The root is reused across attempts (§12): a retry after a failed request is the same
        // account-to-be, not a new one, and the identity key travels with the request so the
        // server can reconcile a retry whose first attempt already landed.
        const root = pendingRootRef.current ?? account.MigoRoot.generate();
        pendingRootRef.current = root;
        const created = buildClient({ server, keyStore: KeyStore.founding(root) });
        const params: Omit<RegisterParams, 'device'> = {
          username: form.username.trim(),
          passphrase: form.passphrase,
          identityPublicKey: account.IdentityKey.fromRoot(root).publicKey(),
        };
        if (form.email?.trim()) {
          params.email = form.email.trim();
        }
        if (form.gender !== undefined) {
          params.gender = form.gender;
        }
        if (captcha !== null) {
          params.captcha = captcha;
        }
        const grant = await created.register(params);
        await Promise.all([
          saveSession({ grant }),
          saveKeyStoreSnapshot(created.keyStore.snapshot()),
          // The account record is what the login screen reads to offer "Continue as {username}";
          // a founding registration always holds the root.
          saveAccountRecord({
            username: form.username.trim(),
            accountId: grant.accountId,
            hasRoot: true,
            savedAt: Date.now(),
          }),
        ]);
        wireInbound(created);
        void created.presence.setPresence(PresenceState.Online).catch(() => {});
        void enrolAccountMaterial(created, grant, server);
        // The root has a durable home now (the snapshot above), so a later registration is a
        // genuinely new account and must mint a genuinely new root.
        pendingRootRef.current = null;
        markReady(grant);
      } catch (cause) {
        await teardown();
        setStatus('anonymous');
        setError(friendlyError(cause));
        throw cause;
      }
    },
    [buildClient, markReady, enrolAccountMaterial, teardown, wireInbound],
  );

  const loginWithFile = useCallback(
    async (
      bytes: Uint8Array,
      passphrase: string,
      server: ServerEndpoint,
      fileName?: string,
    ): Promise<void> => {
      setError(null);
      setStatus('connecting');
      try {
        await saveServerEndpoint(server);
      } catch {
        // Best-effort, see register above.
      }
      try {
        // One container, one credential: the passphrase that sealed the file at registration opens
        // it now (§182 — the reader cannot tell a wrong passphrase from a tampered file, and the
        // screen owes the user one sentence, not a guess).
        let opened: account.AccountFile;
        try {
          opened = await account.openContainer(passphrase, bytes);
        } catch {
          throw new AccountFileError(RESTORE_FAILED);
        }
        const root = opened.rootSecret();
        const fileAccountId = opened.accountId;
        if (fileAccountId === undefined) {
          // A container sealed before accounts were named in the file: it can still found the
          // identity, but the ceremonies have no account to introduce it to.
          throw new AccountFileError(RESTORE_FAILED);
        }
        const accountId = fileAccountId as Id;
        const bootstrap = new BootstrapClient(server);
        const identity = account.IdentityKey.fromRoot(root);

        // Tier one: the device record. A browser that signed in from a file before answers the
        // login ceremony as the same device — the challenge is bound to it and to the username,
        // and both signatures come from material this browser already holds. A record whose
        // device the server no longer knows (revoked, or the record is stale) falls through to
        // tier two, which is not a difference the user needs to be told about.
        const record = await loadDeviceRecord(accountId);
        let grant: Grant | null = null;
        if (record !== undefined) {
          try {
            const challenge = await bootstrap.identityLoginChallenge({
              identifier: record.username,
              deviceId: record.deviceId,
            });
            grant = await bootstrap.identityLogin({
              challengeId: challenge.challengeId,
              identitySignature: identity.signLogin(challenge.payload),
              deviceSignature: account.DeviceCredential.fromSeed(record.credentialSeed).signLogin(
                challenge.payload,
              ),
            });
          } catch {
            grant = null;
          }
        }

        // Tier two: add-device. The file's root signs the challenge and vouches for a fresh
        // device credential, whose seed is then recorded here — the next sign-in takes tier one.
        let username = record?.username ?? '';
        if (grant === null) {
          const credential = account.DeviceCredential.generate();
          const challenge = await bootstrap.addDeviceChallenge({
            accountId,
            device: {
              platform: webHello().platform,
              displayName: deviceDisplayName(),
            },
          });
          grant = await bootstrap.addDevice({
            challengeId: challenge.challengeId,
            identitySignature: identity.signLogin(challenge.payload),
            devicePublicKey: credential.publicKey(),
            deviceSignature: credential.signLogin(challenge.payload),
          });
          await saveDeviceRecord({
            accountId,
            deviceId: grant.deviceId,
            username,
            credentialSeed: credential.exposeSeed(),
            savedAt: Date.now(),
          });
        }

        // The file's root reproduces the founding identity deterministically, so the session runs
        // as the account's founding device — root present, E2EE history readable.
        const created = buildClient({
          server,
          keyStore: KeyStore.founding(root),
          deviceId: grant.deviceId,
        });
        await created.resume(grant);
        // The grant names no username and tier two arrived without one; the profile is the only
        // place it is written down. Best-effort: an empty username is survivable (the file name
        // falls back to "account", and the banner fetches the profile on its own).
        if (username === '') {
          try {
            username = (await created.profile.fetchOne(grant.accountId))?.username ?? '';
          } catch {
            // See above — the shell still renders.
          }
        }
        await Promise.all([
          saveSession({ grant }),
          saveKeyStoreSnapshot(created.keyStore.snapshot()),
          saveAccountRecord({
            username,
            accountId: grant.accountId,
            hasRoot: true,
            savedAt: Date.now(),
          }),
        ]);
        // The sealed file itself is remembered too, ciphertext and nothing else: the next
        // sign-in on this browser picks the account from a list instead of hunting for the
        // file, and still pays the passphrase at the door. The salt is the row's identity, so
        // a re-import upserts and the username this login just learned lands on the row.
        // Best-effort: a storage failure costs the convenience, never the session.
        try {
          await saveKeyFile({
            id: keyFileId(bytes),
            bytes,
            fileName: fileName ?? containerFileName(username),
            username,
            accountId: grant.accountId,
            savedAt: Date.now(),
          });
        } catch {
          // See above.
        }
        wireInbound(created);
        void created.presence.setPresence(PresenceState.Online).catch(() => {});
        // A founding-grade device publishes the account material idempotently — see resume.
        void enrolAccountMaterial(created, grant, server);
        markReady(grant);
      } catch (cause) {
        await teardown();
        setStatus('anonymous');
        setError(cause instanceof AccountFileError ? cause.message : friendlyError(cause));
        throw cause;
      }
    },
    [buildClient, markReady, enrolAccountMaterial, teardown, wireInbound],
  );

  const logout = useCallback(async (): Promise<void> => {
    const grant = clientRef.current?.connected ? clientRef.current.grant : null;
    const server = serverRef.current;
    // Best-effort server-side revocation before dropping the local session. The endpoint for the
    // call is the one the live client was built with: rebuilding a fresh one from the env default
    // would mean logging out from a self-hosted server on the env default URL, which is a bug.
    if (grant && server) {
      try {
        await new BootstrapClient(server).logout(grant.accessToken, grant.sessionId);
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
      const [session, snapshot, endpoint] = await Promise.all([
        loadSession(),
        loadKeyStoreSnapshot(),
        loadServerEndpoint(),
      ]);
      const server = endpoint ?? defaultServerEndpoint();
      if (!session || !snapshot) {
        if (!cancelled) {
          setStatus('anonymous');
        }
        return;
      }

      const keyStore = KeyStore.restore(snapshot);
      const created = buildClient({ keyStore, deviceId: session.grant.deviceId, server });

      let grant = session.grant;
      if (grant.accessExpiresAtMs <= Date.now() + REFRESH_SKEW_MS) {
        grant = await new BootstrapClient(server).refresh({
          refreshToken: grant.refreshToken,
          deviceId: grant.deviceId,
        });
        await saveSession({ grant });
      }

      await created.resume(grant);
      wireInbound(created);
      void created.presence.setPresence(PresenceState.Online).catch(() => {});
      // The legacy upgrade door: a device that holds the root re-publishes its material on every
      // resume, idempotently — it is what makes an account created before the root existed
      // ML-DSA-loginable the day its founding device returns.
      void enrolAccountMaterial(created, grant, server);
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
    persistKeyStore: scheduleKeyStorePersist,
    register,
    loginWithFile,
    logout,
  };

  return <MigoContext.Provider value={value}>{children}</MigoContext.Provider>;
}

// Reserved for a future "forget server" affordance; kept exported to make the import non-dead.
export { clearServerEndpoint };
