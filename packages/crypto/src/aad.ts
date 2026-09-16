/**
 * What an envelope's tag authenticates besides its contents.
 *
 * The AEAD in `aead.ts` authenticates a ciphertext against an *associated data* string. That string
 * is where a protocol binds the metadata it is not willing to encrypt but is not willing to leave
 * forgeable either — and until envelope version 2, Migo's bound less than the specification asks
 * for.
 *
 * # What version 1 binds, and the hole that leaves
 *
 * A v1 associated data is `x3dh_associated_data || ratchet_header`: the two device identities,
 * through X3DH's `IK_initiator || IK_responder`, and the ratchet public key with its two counters.
 * The identity binding is the anti-unknown-key-share protection section 11 cares about most, and it
 * holds. What v1 does not bind is *context*: which conversation the envelope belongs to, which
 * device sent it, which message it is, and which version and scheme the envelope claims.
 *
 * Section 11 names the consequence: without it, "E2E melindungi isi tetapi tidak melindungi
 * konteks". A server that can read the `conversation_id` field of a `MessageSend` frame can rewrite
 * it, and the tag will still verify — the receiver computes the same associated data either way.
 * For an ordinary message that is a denial of service at worst. For a *sender-key distribution*,
 * which is the pairwise envelope this layer exists to carry, it is worse: the distribution installs
 * a chain key, and moving one into a conversation it was not meant for installs a chain there that
 * the sender never authorised.
 *
 * # What version 2 binds, and why the layout is what it is
 *
 * The v2 associated data is the v1 one followed by a context naming the five fields of section 11:
 *
 * ```text
 * "migo-envelope-aad"   17 bytes   purpose label, so these bytes are not any other structure's
 * envelope_version      u8         must equal the envelope's own version byte
 * scheme                u8         must equal the envelope's own scheme byte
 * sender_device         16 bytes   the device the frame claims to be from
 * conversation_id       16 bytes   the conversation the frame claims to belong to
 * flags                 u8         bit 0: a message id follows
 * message_id            16 bytes   present only when flags bit 0 is set
 * ```
 *
 * Every field but the message id is fixed width, so no length prefix is needed and no two different
 * field assignments can encode to the same bytes. The message id gets a flag byte rather than a
 * length prefix because it is the only optional field: one byte states everything the receiver
 * needs, and a length that could disagree with the flag is a length that will.
 *
 * The context is built from the metadata the *frame claims*, not from anything the receiver
 * independently knows. That is the whole mechanism: the receiver recomputes the context from what it
 * was told, the tag only verifies if the sender bound exactly those values, and a server that edits
 * any of them turns a readable message into a failed authentication instead of a silent relocation.
 *
 * # Ids stay untyped here
 *
 * {@link context} takes raw 16-byte arrays rather than the `Id` type from `@migo/wire`, because
 * this package deliberately does not depend on the codec (see `tsconfig.json`): a client can be
 * audited for its cryptography without reading the framing layer. Callers convert with `idToBytes`,
 * and the length is checked here rather than assumed, because a wrong-width id produces a context
 * that fails to authenticate at the *peer* — a bug that surfaces as "it says delivered but nothing
 * arrives", nowhere near its cause.
 *
 * # The version-1 rule
 *
 * {@link context} returns empty bytes for {@link EnvelopeVersion.V1}, whatever metadata the caller
 * passes. This is not a convenience: reading the version byte before the tag has been verified is
 * only safe while a v1 envelope's associated data does not depend on the metadata, and a receiver
 * must be able to compute the right associated data for both versions from the byte it sees. If a
 * v1 context ever became non-empty, every stored v1 message would fail to open, and the failure
 * would look like corruption rather than a version mistake.
 *
 * The cross-language vectors in `shared/protocol/vectors/crypto/aad-context.json` pin both the
 * layout and that rule, and the four implementations are checked against them.
 */

import { CryptoError } from './errors.js';

/** The purpose label every v2 context starts with. */
export const DOMAIN = new TextEncoder().encode('migo-envelope-aad');

/** Bit 0 of the flags byte: a message id follows. */
export const FLAG_MESSAGE_ID = 0x01;

/**
 * Flags no build writes.
 *
 * A receiver refuses a context carrying one, because a bit whose meaning is undefined is a bit an
 * attacker chooses the meaning of.
 */
export const KNOWN_FLAGS = FLAG_MESSAGE_ID;

/** Double Ratchet, no prekey material. The 1:1 path. */
export const SCHEME_DOUBLE_RATCHET = 1;
/** Double Ratchet with X3DH material attached: the first message of a session. */
export const SCHEME_DOUBLE_RATCHET_PREKEY = 2;

/** Bytes in an identifier, matching `ID_BYTE_LEN` in `@migo/wire` and the Rust `Id`. */
export const ID_BYTE_LEN = 16;

/**
 * An envelope version, as a value that cannot come from a number the protocol does not define.
 *
 * Named the way the Rust enum is (`EnvelopeVersion.V2`) so a reader moving between the two
 * implementations reads the same code.
 */
export const EnvelopeVersion = {
  /** Binds identities and the ratchet header, and nothing else. */
  V1: 1,
  /** Binds the five context fields of section 11 as well. */
  V2: 2,
} as const;

/** One of the envelope versions this build writes. */
export type EnvelopeVersion = (typeof EnvelopeVersion)[keyof typeof EnvelopeVersion];

/** Every version a reader must accept, oldest first. */
export const ACCEPTED_VERSIONS: readonly EnvelopeVersion[] = [
  EnvelopeVersion.V1,
  EnvelopeVersion.V2,
];

/**
 * Reads a version byte, or `null` for a value no build writes.
 *
 * A reader refuses anything outside {@link ACCEPTED_VERSIONS} rather than defaulting, because a
 * default would mean computing an associated data for a layout the sender did not choose.
 */
export function fromWire(value: number): EnvelopeVersion | null {
  if (value === EnvelopeVersion.V1 || value === EnvelopeVersion.V2) {
    return value;
  }
  return null;
}

/** Whether a version's associated data carries a context. */
export function bindsContext(version: EnvelopeVersion): boolean {
  return version === EnvelopeVersion.V2;
}

/** A 16-byte identifier, checked rather than trusted. */
function checkId(what: string, bytes: Uint8Array): void {
  if (bytes.length !== ID_BYTE_LEN) {
    throw CryptoError.badLength(what, ID_BYTE_LEN, bytes.length);
  }
}

/**
 * Builds the context an envelope of `version` binds.
 *
 * Returns empty bytes for {@link EnvelopeVersion.V1} — see the module docs; that is the
 * compatibility rule, not an optimisation.
 *
 * `messageId` is `null` (or omitted) for an envelope that is not a message: a sender-key
 * distribution rides inside the message that carries it, so at seal time it has no id of its own to
 * bind, and pretending otherwise by binding the carrier's id would bind a value the receiver does
 * not have.
 */
export function context(
  version: EnvelopeVersion,
  scheme: number,
  senderDevice: Uint8Array,
  conversationId: Uint8Array,
  messageId: Uint8Array | null = null,
): Uint8Array {
  if (!bindsContext(version)) {
    return new Uint8Array(0);
  }
  checkId('sender device id', senderDevice);
  checkId('conversation id', conversationId);
  if (messageId !== null) {
    checkId('message id', messageId);
  }

  const out = new Uint8Array(DOMAIN.length + 3 + ID_BYTE_LEN * (messageId === null ? 2 : 3));
  out.set(DOMAIN, 0);
  let at = DOMAIN.length;
  out[at] = version;
  out[at + 1] = scheme;
  at += 2;
  out.set(senderDevice, at);
  at += ID_BYTE_LEN;
  out.set(conversationId, at);
  at += ID_BYTE_LEN;
  out[at] = messageId === null ? 0 : FLAG_MESSAGE_ID;
  at += 1;
  if (messageId !== null) {
    out.set(messageId, at);
  }
  return out;
}

/**
 * Assembles the associated data a tag is computed over: `v1 prefix || context`.
 *
 * The one place the two halves are joined, so a reader and a writer of the same version cannot
 * disagree about the order.
 */
export function assemble(
  associatedData: Uint8Array,
  header: Uint8Array,
  contextBytes: Uint8Array,
): Uint8Array {
  const out = new Uint8Array(associatedData.length + header.length + contextBytes.length);
  out.set(associatedData, 0);
  out.set(header, associatedData.length);
  out.set(contextBytes, associatedData.length + header.length);
  return out;
}

/** The fields a context carries, as {@link parseContext} recovered them. */
export interface ParsedContext {
  /** The envelope version the context binds. */
  readonly version: EnvelopeVersion;
  /** The scheme byte the context binds. */
  readonly scheme: number;
  /** The sending device the context binds. */
  readonly senderDevice: Uint8Array;
  /** The conversation the context binds. */
  readonly conversationId: Uint8Array;
  /** The message id the context binds, or `null` when it carries none. */
  readonly messageId: Uint8Array | null;
}

/**
 * Reads the fields a context claims back out of its bytes.
 *
 * A receiver does not need this — it builds the context from the metadata it was handed and lets
 * the tag decide. It exists for the conformance vectors and for diagnostics, so it validates rather
 * than assumes: a buffer that is not a well-formed context, or that carries a flag no build writes,
 * is refused.
 */
export function parseContext(bytes: Uint8Array): ParsedContext {
  const fixed = DOMAIN.length + 3 + ID_BYTE_LEN * 2;
  if (bytes.length < fixed) {
    throw CryptoError.badLength('envelope aad context', fixed, bytes.length);
  }
  for (let i = 0; i < DOMAIN.length; i += 1) {
    if (bytes[i] !== DOMAIN[i]) {
      throw CryptoError.malformedHeader();
    }
  }
  let at = DOMAIN.length;
  const version = fromWire(bytes[at] ?? -1);
  if (version === null) {
    throw CryptoError.malformedHeader();
  }
  at += 1;
  const scheme = bytes[at] ?? 0;
  at += 1;
  const senderDevice = bytes.slice(at, at + ID_BYTE_LEN);
  at += ID_BYTE_LEN;
  const conversationId = bytes.slice(at, at + ID_BYTE_LEN);
  at += ID_BYTE_LEN;
  const flags = bytes[at] ?? 0;
  at += 1;
  if ((flags & ~KNOWN_FLAGS) !== 0) {
    throw CryptoError.malformedHeader();
  }
  let messageId: Uint8Array | null = null;
  if ((flags & FLAG_MESSAGE_ID) !== 0) {
    if (bytes.length < at + ID_BYTE_LEN) {
      throw CryptoError.badLength(
        'envelope aad context message id',
        at + ID_BYTE_LEN,
        bytes.length,
      );
    }
    messageId = bytes.slice(at, at + ID_BYTE_LEN);
    at += ID_BYTE_LEN;
  }
  if (at !== bytes.length) {
    throw CryptoError.badLength('envelope aad context', at, bytes.length);
  }
  return { version, scheme, senderDevice, conversationId, messageId };
}
