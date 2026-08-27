/**
 * Shared scaffolding for the SDK test suite.
 *
 * The security invariants this suite exists to defend — no private key on the wire, no key in web
 * storage, binary-only framing — can only be checked by running the real crypto and transport code
 * against real key material and then inspecting every byte that would have left the device. That
 * needs three things the individual tests should not each reinvent: a way to mint two devices' key
 * material, a way to turn one device's published bundle into the {@link PrekeyBundle} its peer would
 * fetch, and doubles that record what the domains push at the transport and the network instead of
 * sending it. Centralising them here also keeps one definition of "every private seed this device
 * holds" ({@link privateSeeds}); a test that hand-listed the seeds would silently stop covering a
 * seed the day the key store grew a new one.
 */

import { frameHeader, idFromBytes } from '@migo/wire';
import type { Frame, Id } from '@migo/wire';
import { encodeMessageAccepted } from '@migo/protocol';
import { IdentityPublic, PrekeyBundle, SignedPrekey } from '@migo/crypto';
import { KeyStore, encodeBody } from '../src/index.js';
import type { GatewayTransport, EventHandler } from '../src/transport.js';
import type { PeerBundleSource } from '../src/index.js';

/** A byte string as hex, for legible failure messages. */
export function hex(bytes: Uint8Array): string {
  return [...bytes].map((byte) => byte.toString(16).padStart(2, '0')).join('');
}

/** Parses a hex string into bytes. */
export function unhex(text: string): Uint8Array {
  const out = new Uint8Array(text.length / 2);
  for (let i = 0; i < out.length; i += 1) {
    out[i] = Number.parseInt(text.slice(i * 2, i * 2 + 2), 16);
  }
  return out;
}

/**
 * Whether `needle` appears as a contiguous run of bytes anywhere in `haystack`.
 *
 * This is the primitive the "never sent" assertions are built on: a private seed leaks if and only
 * if its exact bytes appear somewhere in a frame the client would transmit. An empty needle is
 * reported as absent — a zero-length match is not evidence of anything and would make every search
 * trivially true.
 */
export function containsBytes(haystack: Uint8Array, needle: Uint8Array): boolean {
  if (needle.length === 0 || needle.length > haystack.length) {
    return false;
  }
  for (let start = 0; start + needle.length <= haystack.length; start += 1) {
    let matched = true;
    for (let i = 0; i < needle.length; i += 1) {
      if (haystack[start + i] !== needle[i]) {
        matched = false;
        break;
      }
    }
    if (matched) {
      return true;
    }
  }
  return false;
}

/**
 * A fresh device's key material with a small prekey pool.
 *
 * The default pool is tiny on purpose: every extra one-time prekey is a fresh X25519 keypair, and
 * the suite mints many stores. Nothing here depends on the pool being the production size of 64.
 */
export function newStore(oneTimePrekeys = 4): KeyStore {
  return KeyStore.create(oneTimePrekeys);
}

/**
 * The {@link PrekeyBundle} a peer would fetch for `store`, built from the public halves the store
 * publishes. The one-time prekey is the first the store still holds, so a responder handshake
 * against this bundle can resolve and consume it.
 */
export function bundleFrom(store: KeyStore): PrekeyBundle {
  const published = store.publish();
  const first = published.oneTimePrekeys[0];
  const oneTimePrekey =
    first !== undefined ? { keyId: first.keyId, publicKey: first.publicKey } : null;
  return new PrekeyBundle(
    IdentityPublic.parse(published.identityKey),
    new SignedPrekey(
      published.signedPrekeyId,
      published.signedPrekey,
      published.signedPrekeySignature,
    ),
    oneTimePrekey,
  );
}

/**
 * Every private seed `store` holds, as raw bytes.
 *
 * Sourced from {@link KeyStore.snapshot} rather than a hand-written list so it automatically covers
 * any secret a future store gains. These are the byte strings that must never appear on the wire.
 */
export function privateSeeds(store: KeyStore): Uint8Array[] {
  const snapshot = store.snapshot();
  return [
    snapshot.identitySigningSeed,
    snapshot.identityExchangeSeed,
    snapshot.signedPrekeySeed,
    ...snapshot.oneTimePrekeys.map((prekey) => prekey.seed),
  ];
}

/** A deterministic {@link Id} whose value is `n`, for stable conversation and device ids in tests. */
export function idOf(n: number): Id {
  const bytes = new Uint8Array(16);
  bytes[15] = n & 0xff;
  bytes[14] = (n >>> 8) & 0xff;
  return idFromBytes(bytes);
}

/** Encodes a well-formed `MessageAccepted` payload, so a recorded `MESSAGE_SEND` resolves. */
export function encodeAccepted(messageId: Id, conversationId: Id, seq = 1): Uint8Array {
  return encodeBody(encodeMessageAccepted, {
    messageId,
    conversationId,
    seq,
    createdAt: 1_700_000_000_000,
  });
}

/** A {@link PeerBundleSource} that always serves one bundle and counts how often it was asked. */
export class StaticBundleSource implements PeerBundleSource {
  fetchCount = 0;
  readonly #bundle: PrekeyBundle;

  constructor(bundle: PrekeyBundle) {
    this.#bundle = bundle;
  }

  fetchBundle(): Promise<PrekeyBundle> {
    this.fetchCount += 1;
    return Promise.resolve(this.#bundle);
  }
}

/** One frame the client pushed at the transport: its opcode and the encoded body bytes. */
export interface SentFrame {
  opcode: number;
  body: Uint8Array;
}

/**
 * A transport double that records every frame a domain sends and lets a test inject server events.
 *
 * It stands in for a real {@link GatewayTransport} behind an `Rpc`. `request` returns whatever
 * {@link reply} produces, so a domain awaiting a decoded response gets a well-formed one; `notify`
 * records and returns; `subscribe` registers handlers a test drives with {@link emit}. The private
 * fields of the real transport are nominal, so {@link asTransport} casts through `unknown`.
 */
export class RecordingTransport {
  readonly sent: SentFrame[] = [];
  reply: (opcode: number, body: Uint8Array) => Uint8Array = () => new Uint8Array();
  readonly #handlers = new Map<number, Set<EventHandler>>();

  request(opcode: number, body: Uint8Array): Promise<Frame> {
    this.sent.push({ opcode, body: body.slice() });
    const payload = this.reply(opcode, body);
    return Promise.resolve({ header: frameHeader(opcode, 1), payload });
  }

  notify(opcode: number, body: Uint8Array): Promise<void> {
    this.sent.push({ opcode, body: body.slice() });
    return Promise.resolve();
  }

  subscribe(opcode: number, handler: EventHandler): () => void {
    let set = this.#handlers.get(opcode);
    if (set === undefined) {
      set = new Set();
      this.#handlers.set(opcode, set);
    }
    set.add(handler);
    return () => {
      this.#handlers.get(opcode)?.delete(handler);
    };
  }

  /** Delivers a server event to every subscriber of `opcode`, as the real receive loop would. */
  emit(opcode: number, payload: Uint8Array): void {
    const frame: Frame = { header: frameHeader(opcode, 0), payload };
    for (const handler of this.#handlers.get(opcode) ?? []) {
      handler(payload, frame);
    }
  }

  /** All recorded bodies, for a leak scan across everything sent. */
  bodies(): Uint8Array[] {
    return this.sent.map((frame) => frame.body);
  }

  asTransport(): GatewayTransport {
    return this as unknown as GatewayTransport;
  }
}
