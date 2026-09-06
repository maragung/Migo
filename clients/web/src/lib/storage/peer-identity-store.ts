/**
 * Persistence for the last-acknowledged peer identity fingerprint, per conversation and per peer
 * device — the memory behind the key-change warning (§164).
 *
 * A Migo identity belongs to a *device*, so the record is keyed by conversation *and* device: a peer
 * signed in on a phone and a laptop publishes two identities, and a change on one is not a change
 * on the other. The value is the 32-byte fingerprint of the identity the conversation last
 * acknowledged — written the first time a device's identity is observed (silently, because nothing
 * changed), and re-written only by a person's explicit acknowledgment of a change, never by the
 * read that detected it. That last rule is the whole point of the store: a warning that clears
 * itself in the same breath that raised it is a warning nobody ever sees.
 *
 * The bytes are a *fingerprint* — public material, derived from the identity public key — so the
 * store keeps them as they are, in IndexedDB (the sanctioned store), without the sealing the
 * key-store snapshot needs. Nothing here can decrypt anything; the worst a copied database leaks is
 * which identities this browser has verified, which every peer already broadcasts anyway.
 */

import type { Id } from '@migo/sdk';

import { idbDelete, idbGet, idbSet } from './idb.js';

const keyFor = (conversationId: Id, deviceId: Id): string =>
  `peer-identity:${conversationId}:${deviceId}`;

/**
 * Loads the fingerprint this conversation last acknowledged for a peer device, or `undefined` when
 * the conversation has never observed the device.
 */
export function loadPeerIdentity(
  conversationId: Id,
  deviceId: Id,
): Promise<Uint8Array | undefined> {
  return idbGet<Uint8Array>(keyFor(conversationId, deviceId));
}

/**
 * Records a fingerprint as the acknowledged one for a peer device in a conversation.
 *
 * Called on first observation (the silent baseline) and on explicit acknowledgment — the two writes
 * a person or a first sight is entitled to make. It is deliberately *not* called by the read that
 * detects a change; see the module doc.
 */
export function savePeerIdentity(
  conversationId: Id,
  deviceId: Id,
  fingerprint: Uint8Array,
): Promise<void> {
  return idbSet(keyFor(conversationId, deviceId), fingerprint);
}

/**
 * Removes one device's record for a conversation — the forget path, for a conversation this browser
 * no longer holds. An absent key is a no-op, the same contract every store here keeps.
 */
export function clearPeerIdentity(conversationId: Id, deviceId: Id): Promise<void> {
  return idbDelete(keyFor(conversationId, deviceId));
}
