/**
 * The shared session: what the store reads from the web client's IndexedDB.
 *
 * The web client persists two records that together are a resumable session — the grant (this
 * session's tokens) and the key-store snapshot (this device's cryptographic identity, including
 * the account root when the device holds it). This module reads both with the same keys the web
 * client wrote them under, so a user who is signed in to Migo walks into the store already
 * signed in.
 *
 * The store never writes these records. They are the web client's to own; this app is a reader
 * that happens to share the origin. The one exception is the key-store snapshot after a
 * purchase: a tracked on-chain transaction is recorded on the snapshot (the web client's
 * Activity list reads it from there), so the snapshot is written back after the purchase
 * mutates it — with the same writer the web client would use, over the same key.
 */

import { KeyStore, MigoClient, BootstrapClient } from '@migo/sdk';
import type { Grant, KeyStoreSnapshot, MigoClientOptions } from '@migo/sdk';

import { defaultServerEndpoint } from './config.js';
import { deviceDisplayName, storeHello } from './hello.js';
import { idbGet, idbSet } from './idb.js';

/** The key the web client persists the session grant under. */
const SESSION_KEY = 'session';

/** The key the web client persists the keystore snapshot under. */
const SNAPSHOT_KEY = 'keystore-snapshot';

/** The persisted session shape, verbatim from the web client's session-store. */
interface PersistedSession {
  grant: Grant;
}

/** Refresh the access token if it expires within this window, to avoid a doomed first request. */
const REFRESH_SKEW_MS = 30_000;

/**
 * The restored session: the live client plus the key store it was built from.
 *
 * `null` when the browser has no persisted session (the user is signed out) — the caller decides
 * what to show then.
 */
export interface RestoredSession {
  client: MigoClient;
  grant: Grant;
}

/**
 * Resumes the web client's session in this app.
 *
 * Reads the grant and snapshot, refreshes the grant when its access token is stale, builds a
 * `MigoClient` over the restored key store, and resumes. The client options mirror the web
 * client's construction (hello, device name); the store adds nothing and changes nothing.
 * The socket the resume opens is the store's own — the web client's tab is unaffected.
 */
export async function restoreSession(): Promise<RestoredSession | null> {
  const [session, snapshot] = await Promise.all([
    idbGet<PersistedSession>(SESSION_KEY),
    idbGet<KeyStoreSnapshot>(SNAPSHOT_KEY),
  ]);
  if (session === undefined || snapshot === undefined) {
    return null;
  }
  const server = defaultServerEndpoint();
  let grant = session.grant;
  if (grant.accessExpiresAtMs <= Date.now() + REFRESH_SKEW_MS) {
    grant = await new BootstrapClient(server).refresh({
      refreshToken: grant.refreshToken,
      deviceId: grant.deviceId,
    });
    await idbSet<PersistedSession>(SESSION_KEY, { grant });
  }
  const keyStore = KeyStore.restore(snapshot);
  const options: MigoClientOptions = {
    server,
    hello: storeHello(),
    deviceDisplayName: deviceDisplayName(),
    keyStore,
    deviceId: grant.deviceId,
  };
  const client = MigoClient.create(options);
  await client.resume(grant);
  return { client, grant };
}

/** Persists the (possibly mutated by a tracked purchase) snapshot back where the web client reads it. */
export async function persistSnapshot(client: MigoClient): Promise<void> {
  await idbSet(SNAPSHOT_KEY, client.keyStore.snapshot());
}
