/**
 * Timestamps.
 *
 * On the wire a timestamp is milliseconds since the **Migo epoch**, 2024-01-01T00:00:00Z.
 * Counting from 1970 would spend a varint byte on 54 years that no Migo message can
 * possibly fall in; counting from 2024 fits the next 68 years in five bytes instead of
 * six. At a few timestamps per message that byte is real.
 *
 * In memory a timestamp is a `number` of **Unix** milliseconds, so it goes straight into
 * `new Date(...)`, `Intl.DateTimeFormat`, and every date library, with no wrapper type
 * and no chance of an epoch-confused subtraction leaking into product code. The
 * conversion lives here and nowhere else, which is the only way to be sure it happens
 * exactly once per field.
 *
 * Getting this wrong is a 54-year offset, so the conformance vectors pin it: a build
 * that forgets the epoch fails `crypto`-free, in `mse.json`, on the first timestamp case.
 */

/** Milliseconds between the Unix epoch and the Migo epoch. */
export const MIGO_EPOCH_MS = 1704067200000;

/** Converts Unix milliseconds to the wire representation. */
export function toWire(unixMs: number): number {
  const wire = Math.trunc(unixMs) - MIGO_EPOCH_MS;
  // A timestamp before 2024 cannot be represented and is almost always a default-
  // constructed zero rather than a real date. Clamped rather than rejected, matching
  // `Timestamp::to_wire`, so one bad clock cannot make a whole frame unsendable.
  return wire < 0 ? 0 : wire;
}

/** Converts the wire representation to Unix milliseconds. */
export function fromWire(wireMs: number): number {
  return wireMs + MIGO_EPOCH_MS;
}
