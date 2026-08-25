/**
 * Identifiers: 128 bits on the wire, 26 Crockford base32 characters in memory.
 *
 * ## Why a branded string and not a byte array
 *
 * `Id` is a `string` at runtime. That is a deliberate departure from the Rust side,
 * where `Id` is `[u8; 16]`, and the reason is that JavaScript compares objects by
 * reference. A `Uint8Array`-backed id would make `a === b` false for two ids that are
 * equal, and — much worse — would make `Map<Id, T>` and `Set<Id>` silently wrong: the
 * lookup would miss and the caller would insert a duplicate. That bug does not throw,
 * does not show up in a unit test written with one id, and surfaces as "the message
 * appears twice" in production.
 *
 * A string costs a base32 conversion per field at the codec boundary and buys `===`,
 * object keys, structural equality in tests, and a value that survives `JSON.stringify`
 * into React state and back. For a chat client that trade is not close.
 *
 * The brand is compile-time only: it stops a bare `string` being passed where an id is
 * required, without adding a wrapper object at runtime.
 *
 * ## The text form
 *
 * The first six bytes are big-endian Unix milliseconds, so ids sort chronologically as
 * both bytes and text, and a database index on them stays append-mostly. The alphabet
 * excludes I, L, O and U: the first three because they are misread as 1, 1 and 0 when a
 * human copies an id out of a support ticket, and U because excluding it keeps accidental
 * profanity out of generated identifiers.
 */

import { WireError } from './errors.js';

/** Crockford base32, minus I, L, O and U. */
const ALPHABET = '0123456789ABCDEFGHJKMNPQRSTVWXYZ';

/**
 * Reverse lookup, built once.
 *
 * Deliberately lenient in the same three ways the Rust decoder is: lowercase is accepted,
 * `I`/`l` read as `1`, and `O` reads as `0`. Those are Crockford's documented confusions,
 * and the rule matters more than the mapping — an id pasted from a support ticket must
 * either parse on both sides or fail on both sides. A validator that is stricter in the
 * client than in the server produces the worst kind of bug report: "it works for them".
 */
const DECODE: ReadonlyMap<string, number> = buildDecodeTable();

function buildDecodeTable(): ReadonlyMap<string, number> {
  const table = new Map<string, number>();
  for (const [index, character] of [...ALPHABET].entries()) {
    table.set(character, index);
    table.set(character.toLowerCase(), index);
  }
  for (const character of 'IiLl') table.set(character, 1);
  for (const character of 'Oo') table.set(character, 0);
  return table;
}

/** Characters in the text form. */
export const ID_TEXT_LEN = 26;
/** Bytes in the wire form. */
export const ID_BYTE_LEN = 16;

declare const idBrand: unique symbol;

/** A Migo identifier. A `string` at runtime; not assignable from a bare `string`. */
export type Id = string & { readonly [idBrand]: 'Id' };

/** The all-zero id. Means "absent" in a required field, and is never generated. */
export const NIL_ID = '00000000000000000000000000' as Id;

/** Converts 16 wire bytes to an id. */
export function idFromBytes(bytes: Uint8Array): Id {
  if (bytes.length !== ID_BYTE_LEN) {
    throw WireError.fieldOverflow('id');
  }
  // 128 bits does not fit a double, and the text form is 26 groups of 5 bits, which is
  // 130 — so the groups do not align with bytes and there is no way around wide
  // arithmetic here. This runs once per id field, not per byte.
  let n = 0n;
  for (const byte of bytes) {
    n = (n << 8n) | BigInt(byte);
  }
  return render(n);
}

/** Renders 128 bits as the canonical 26-character text form. */
function render(n: bigint): Id {
  let text = '';
  for (let i = 0; i < ID_TEXT_LEN; i += 1) {
    const shift = BigInt(125 - i * 5);
    const index = Number((n >> shift) & 0x1fn);
    text += ALPHABET[index];
  }
  return text as Id;
}

/** Converts an id to its 16 wire bytes. */
export function idToBytes(id: Id): Uint8Array {
  const n = parseToBigInt(id);
  const out = new Uint8Array(ID_BYTE_LEN);
  let remaining = n;
  for (let i = ID_BYTE_LEN - 1; i >= 0; i -= 1) {
    out[i] = Number(remaining & 0xffn);
    remaining >>= 8n;
  }
  return out;
}

/** What was wrong with a candidate identifier. */
export type IdParseFailure =
  | { readonly kind: 'length'; readonly actual: number }
  | { readonly kind: 'character'; readonly position: number }
  | { readonly kind: 'overflow' };

/**
 * Validates text as an id, returning the failure instead of throwing.
 *
 * The id that comes back is *canonical*: uppercase, with Crockford's confusable characters
 * folded. Returning the caller's spelling instead would hand back two different strings for
 * one identifier, and the whole reason `Id` is a string is that `===`, `Map` and `Set` work
 * on it. A lenient parser that skipped this step would reintroduce, in a subtler form,
 * exactly the duplicate-key bug the branded string exists to prevent.
 */
export function tryParseId(
  text: string,
): { ok: true; id: Id } | { ok: false; why: IdParseFailure } {
  const scanned = scan(text);
  return scanned.ok ? { ok: true, id: render(scanned.value) } : scanned;
}

function scan(text: string): { ok: true; value: bigint } | { ok: false; why: IdParseFailure } {
  if (text.length !== ID_TEXT_LEN) {
    return { ok: false, why: { kind: 'length', actual: text.length } };
  }
  let n = 0n;
  for (let position = 0; position < text.length; position += 1) {
    const value = DECODE.get(text[position] as string);
    if (value === undefined) {
      return { ok: false, why: { kind: 'character', position } };
    }
    // 26 characters carry 130 bits but an id is 128, so the leading character may only
    // use its low three. Rejecting the rest keeps the text form injective: without this
    // check, four distinct strings would decode to the same id.
    if (position === 0 && value > 7) {
      return { ok: false, why: { kind: 'overflow' } };
    }
    n = (n << 5n) | BigInt(value);
  }
  return { ok: true, value: n };
}

/** Validates text as an id, throwing on anything malformed. */
export function parseId(text: string): Id {
  const result = tryParseId(text);
  if (!result.ok) {
    // The failure reason is safe to state: it describes the shape of the input, not its
    // content, and an id is not secret material.
    throw new TypeError(`not a Migo id (${describe(result.why)}): length ${text.length}`);
  }
  return result.id;
}

/**
 * True when `value` is a well-formed id *in canonical form*. Narrows for callers holding a
 * bare string.
 *
 * A lenient spelling — lowercase, or with an `O` for a zero — is parseable but is not
 * itself an id, because two spellings of one identifier would not compare equal. Run such
 * a string through {@link parseId} instead of asserting it with this.
 */
export function isId(value: unknown): value is Id {
  if (typeof value !== 'string') {
    return false;
  }
  const parsed = tryParseId(value);
  return parsed.ok && parsed.id === value;
}

/** Unix milliseconds from the id's time prefix. */
export function idUnixMs(id: Id): number {
  const bytes = idToBytes(id);
  let ms = 0;
  for (let i = 0; i < 6; i += 1) {
    ms = ms * 256 + (bytes[i] as number);
  }
  return ms;
}

function parseToBigInt(text: string): bigint {
  const result = scan(text);
  if (!result.ok) {
    throw new TypeError(`not a Migo id (${describe(result.why)}): length ${text.length}`);
  }
  return result.value;
}

function describe(why: IdParseFailure): string {
  switch (why.kind) {
    case 'length':
      return `must be ${ID_TEXT_LEN} characters, got ${why.actual}`;
    case 'character':
      return `invalid character at position ${why.position}`;
    case 'overflow':
      return 'leading character encodes bits above 128';
  }
}
