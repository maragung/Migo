/**
 * The one place client-side key material is drawn from.
 *
 * The Rust crate threads a `&mut dyn Random` through every function that needs entropy, so a
 * deterministic simulation can inject a seeded generator and a reviewer can see, in the
 * signature, exactly which functions consume randomness. JavaScript has no such handle to pass,
 * and the equivalent hazard is a test helper that quietly returns `Math.random()` bytes past a
 * reviewer who does not notice. The countermeasure here is the mirror image: there is exactly
 * one function that produces random bytes, it comes straight from the platform CSPRNG, and it
 * cannot be swapped for anything weaker. Determinism, where a test needs it, is routed through
 * the `*WithNonce` and `fromSeed` entry points that take their bytes as arguments instead.
 *
 * This is the same reasoning, and the same refusal-to-fall-back, as {@link module:aead}'s
 * private `randomBytes`; it is lifted into its own module because {@link module:identity} and
 * the ratchet need it too, and a second copy is a second place the fallback could be added by
 * mistake.
 */

/**
 * Returns `length` cryptographically secure random bytes.
 *
 * Throws if the platform exposes no CSPRNG. A fallback to a non-cryptographic generator is not
 * offered on purpose: it would produce keys that look random in every test and are predictable
 * in production, which is the single worst failure mode a key generator can have.
 */
export function randomBytes(length: number): Uint8Array {
  const source = globalThis.crypto;
  if (typeof source?.getRandomValues !== 'function') {
    throw new TypeError('no cryptographic random source: crypto.getRandomValues is unavailable');
  }
  const out = new Uint8Array(length);
  source.getRandomValues(out);
  return out;
}
