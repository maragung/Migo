/**
 * The media key of an encrypted call — the TypeScript mirror of `migo-crypto`'s `CallKeyState`.
 *
 * Section 163: a call's media key is *derived from the pairwise E2E session* between the devices,
 * not minted in the clear and not chosen by the server. Both devices already hold a secret no
 * server has ever seen, so HKDF under {@link kdf.LABEL_CALL_KEY} turns it into a media key
 * neither party had to send. Deriving instead of distributing is what keeps the call's
 * confidentiality anchored to the session's: an attacker who cannot read the messages cannot read
 * the call either, because there is nothing else to read.
 *
 * # Rotation
 *
 * A group call's membership changes, and section 163 requires that a participant who leaves cannot
 * decrypt what follows and one who joins cannot decrypt what came before. So the key rotates: the
 * rotating side mints fresh material for the next epoch and returns it *sealed under the current
 * key* — those bytes are the `sealedKeyMaterial` of a `CALL_KEY_UPDATE`, and only a device holding
 * the current epoch can open them. {@link CallKeyState.adopt} accepts such an update and refuses
 * any epoch that does not advance, which is the same replay-and-rollback refusal the message
 * ratchets live by.
 *
 * # The group-call first key
 *
 * {@link CallKeyState.fromSession} is the 1:1 shape: two devices, one shared session, both sides
 * derive the same key with nothing on the wire. A *group* call's first seat has no peer to derive
 * with, so it mints ({@link CallKeyState.create}) and every later participant receives the key by
 * the sealed ask path ({@link CallKeyState.sealedJoinDistribution} /
 * {@link CallKeyState.fromJoinDistribution}) — which is what makes a group call's key *distributed*
 * rather than derived, exactly as a sender key is. The joiner's copy is sealed under a wrapping
 * key derived from the pairwise session the distributor shares with *that joiner* (HKDF under
 * {@link kdf.LABEL_CALL_JOIN}, the call id as salt), and the call id is also the AEAD associated
 * data, so the blob cannot be opened for another call.
 *
 * # Byte-for-byte with the Rust crate
 *
 * The binding is `call_id bytes || epoch u64 big-endian`; a rotation's sealed material is the raw
 * 32-byte next key under the current key; a join distribution's plaintext is `epoch u64
 * big-endian || 32-byte key` (40 bytes). These are wire contracts between clients, not internal
 * encodings, so they match `server/crates/migo-crypto/src/call_key.rs` exactly.
 */

import type { Id } from '@migo/wire';
import { idToBytes } from '@migo/wire';
import { aead, kdf, CryptoError, SymmetricKey } from '@migo/crypto';

/** Length of a call's media key material, mirroring `CALL_KEY_LEN` in the crypto crate. */
export const CALL_KEY_LEN = 32;

/** Length of a join distribution's plaintext: the epoch, then the key. */
export const JOIN_DISTRIBUTION_LEN = 8 + CALL_KEY_LEN;

/**
 * The sending and receiving half of one call's key, one per call per device.
 *
 * Both sides of a 1:1 call hold the same state by construction — same session secret, same call
 * id, same derivation — so there is no handshake to run and nothing to agree on beyond the call id
 * itself. In a group call every participant converges on the same epoch by applying the same
 * updates in order. The key bytes are held privately: nothing here prints, serialises, or exposes
 * them, and the only operations are the ones the protocol needs.
 */
export class CallKeyState {
  readonly #callId: Id;
  #epoch: number;
  #key: Uint8Array;

  private constructor(callId: Id, epoch: number, key: Uint8Array) {
    this.#callId = callId;
    this.#epoch = epoch;
    this.#key = key;
  }

  /**
   * Derives the call's first key (epoch 0) from the pairwise session secret.
   *
   * The call id is the HKDF salt, so one session cannot produce the same media key for two
   * different calls — a key that outlived its call would be a second purpose for a key that
   * already had one.
   */
  static fromSession(sessionSecret: Uint8Array, callId: Id): CallKeyState {
    return new CallKeyState(
      callId,
      0,
      kdf.derive(sessionSecret, idToBytes(callId), kdf.LABEL_CALL_KEY, CALL_KEY_LEN),
    );
  }

  /**
   * Mints a call's first key from fresh randomness: epoch 0, known to no one else.
   *
   * This is the group-call first seat's mint. {@link CallKeyState.fromSession} cannot serve there
   * — a lone first seat shares a pairwise session with nobody seated — and every participant who
   * follows receives the key through the sealed join path, which is what carries it to the group.
   */
  static create(callId: Id): CallKeyState {
    return new CallKeyState(callId, 0, SymmetricKey.generate().expose().slice());
  }

  /**
   * Opens a join distribution into the state it carries: the joiner's first key of a call already
   * in progress.
   *
   * This is a constructor, not an {@link CallKeyState.adopt}: the joiner holds no earlier epoch to
   * compare against, so the first distribution is the baseline, exactly as the first sender-key
   * distribution is. The blob must open under the session secret this device shares with the
   * sender of the distribution and be bound to `callId`.
   */
  static fromJoinDistribution(
    sessionSecret: Uint8Array,
    callId: Id,
    sealed: Uint8Array,
  ): CallKeyState {
    const plaintext = aead.open(joinWrappingKey(sessionSecret, callId), idToBytes(callId), sealed);
    if (plaintext.length !== JOIN_DISTRIBUTION_LEN) {
      throw CryptoError.badLength(
        'call join distribution',
        JOIN_DISTRIBUTION_LEN,
        plaintext.length,
      );
    }
    const epoch = Number(new DataView(plaintext.buffer, plaintext.byteOffset, 8).getBigUint64(0));
    return new CallKeyState(callId, epoch, plaintext.slice(8));
  }

  /** The epoch this state's key belongs to. Zero until the first rotation. */
  epoch(): number {
    return this.#epoch;
  }

  /**
   * Rotates: mints the next epoch's key and returns it sealed under the current one.
   *
   * The sealed bytes are what a `CALL_KEY_UPDATE` carries as `sealedKeyMaterial`; its `epoch`
   * field is the value {@link CallKeyState.epoch} reports after this call. State advances only
   * after the sealing succeeds, so a failure leaves the call on the old key rather than between
   * keys. The rotating device must not depend on hearing its own update back — the server does
   * not relay a `CALL_KEY_UPDATE` to the connection that sent it.
   */
  rotate(): Uint8Array {
    const next = SymmetricKey.generate().expose().slice();
    const sealed = aead.seal(
      SymmetricKey.fromBytes(this.#key),
      this.#binding(this.#epoch + 1),
      next,
    );
    this.#key = next;
    this.#epoch += 1;
    return sealed;
  }

  /**
   * Adopts a distributed update, moving to `epoch`.
   *
   * The update must open under the *current* key and be bound to exactly `epoch`, and the epoch
   * must advance: an update that repeats or rolls back the epoch is a replay of an old key and is
   * refused with `KeyAlreadyUsed`. As everywhere in the crypto layers, the state moves only after
   * the new material is verified, so a bad update cannot destroy a working key.
   */
  adopt(epoch: number, sealed: Uint8Array): void {
    if (epoch <= this.#epoch) {
      // Reuse would let a replayed update re-install an old key and re-open the media it sealed,
      // which is the call-side face of the rule the message ratchets enforce with the same error.
      throw new CryptoError('KeyAlreadyUsed', 'call key update does not advance the epoch');
    }
    const material = aead.open(SymmetricKey.fromBytes(this.#key), this.#binding(epoch), sealed);
    if (material.length !== CALL_KEY_LEN) {
      throw CryptoError.badLength('call key material', CALL_KEY_LEN, material.length);
    }
    this.#key = material;
    this.#epoch = epoch;
  }

  /**
   * Seals the current epoch and key for a participant joining mid-call.
   *
   * The wrapping key is derived from `sessionSecret` — the pairwise session this device shares
   * *with the joiner*, not the one the call started from — under its own label, so the call's own
   * key and the key that wraps it for the joiner are never the same material. The joiner reads the
   * epoch out of the sealed body itself, which means a blob that lied about its epoch does not
   * parse.
   */
  sealedJoinDistribution(sessionSecret: Uint8Array): Uint8Array {
    const plaintext = new Uint8Array(JOIN_DISTRIBUTION_LEN);
    new DataView(plaintext.buffer).setBigUint64(0, BigInt(this.#epoch));
    plaintext.set(this.#key, 8);
    return aead.seal(
      joinWrappingKey(sessionSecret, this.#callId),
      idToBytes(this.#callId),
      plaintext,
    );
  }

  /**
   * Seals one media frame under the current key.
   *
   * The associated data binds the call and the epoch, so a frame cannot be lifted into another
   * call, and a frame from before a rotation cannot be presented as one from after it even to a
   * device that kept the old key.
   */
  sealFrame(frame: Uint8Array): Uint8Array {
    return aead.seal(SymmetricKey.fromBytes(this.#key), this.#binding(this.#epoch), frame);
  }

  /** Opens a media frame sealed under the current key. */
  openFrame(sealed: Uint8Array): Uint8Array {
    return aead.open(SymmetricKey.fromBytes(this.#key), this.#binding(this.#epoch), sealed);
  }

  /**
   * The bytes every cryptographic operation here binds: which call, which epoch. Fixed-width so
   * the pair cannot be re-split.
   */
  #binding(epoch: number): Uint8Array {
    const out = new Uint8Array(16 + 8);
    out.set(idToBytes(this.#callId), 0);
    new DataView(out.buffer, 16, 8).setBigUint64(0, BigInt(epoch));
    return out;
  }
}

/**
 * The key that wraps a join distribution for one joiner.
 *
 * Derived from the pairwise session secret the distributor shares with that joiner, under its own
 * label — the third purpose that secret serves, so it must not share a label with the ratchet or
 * the call key itself.
 */
function joinWrappingKey(sessionSecret: Uint8Array, callId: Id): SymmetricKey {
  return SymmetricKey.fromBytes(
    kdf.derive(sessionSecret, idToBytes(callId), kdf.LABEL_CALL_JOIN, CALL_KEY_LEN),
  );
}
