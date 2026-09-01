/**
 * AVAX display arithmetic (§184): wei in, a person-readable string out.
 *
 * AVAX has 18 decimals and gas prices are quoted in nAVAX (9 decimals). Everything here is
 * `bigint` on the way in — a `number` silently loses precision at 2^53, which is nine digits
 * below one AVAX — and a trimmed decimal string on the way out, so the amount a person typed is
 * the amount they read back.
 */

/** One AVAX in wei. */
const UNIT = 10n ** 18n;

/** A wei amount as AVAX, 18 decimals, trailing zeros trimmed. */
export function avaxOf(wei: bigint): string {
  return decimalOf(wei, 18n);
}

/** A wei amount as nAVAX (§184's fee unit), 9 decimals, trailing zeros trimmed. */
export function navaxOf(wei: bigint): string {
  return decimalOf(wei, 9n);
}

function decimalOf(wei: bigint, decimals: bigint): string {
  const unit = 10n ** decimals;
  const whole = wei / unit;
  let fraction = (wei % unit).toString(10);
  if (fraction.match(/^0*$/)) {
    return whole.toString(10);
  }
  while (fraction.length < Number(decimals)) {
    fraction = `0${fraction}`;
  }
  return `${whole}.${fraction.replace(/0+$/, '')}`;
}

/**
 * The send form's amount string as wei, or `null` when it is not an amount this chain accepts.
 *
 * The refusals are the ones every client enforces: empty, signed, non-decimal, a second dot, more
 * than 18 fractional digits.
 */
export function parseAvaxAmount(text: string): bigint | null {
  const trimmed = text.trim();
  if (trimmed.length === 0) {
    return null;
  }
  const parts = trimmed.split('.');
  if (parts.length > 2) {
    return null;
  }
  const whole = parts[0] ?? '';
  const fraction = parts[1] ?? '';
  if (whole.length === 0 && fraction.length === 0) {
    return null;
  }
  if (!/^\d*$/.test(whole) || !/^\d*$/.test(fraction)) {
    return null;
  }
  if (fraction.length > 18) {
    return null;
  }
  const wholeWei = (whole.length === 0 ? 0n : BigInt(whole)) * UNIT;
  const fractionWei =
    fraction.length === 0 ? 0n : BigInt(fraction) * 10n ** BigInt(18 - fraction.length);
  return wholeWei + fractionWei;
}

/** Lowercase hex for the public material the chain surface shows: hashes and addresses only. */
export function hexOf(bytes: Uint8Array): string {
  let out = '';
  for (const byte of bytes) {
    out += byte.toString(16).padStart(2, '0');
  }
  return out;
}
