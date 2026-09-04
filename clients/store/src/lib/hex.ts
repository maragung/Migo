/** Lowercase hex for hashes and addresses — the only public material the store shows. */
export function hexOf(bytes: Uint8Array): string {
  let out = '';
  for (const byte of bytes) {
    out += byte.toString(16).padStart(2, '0');
  }
  return out;
}
