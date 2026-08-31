/**
 * Maps an SDK error to a short, user-facing message.
 *
 * A {@link RemoteError} carries a stable `symbol` and an optional human `message`. The server only
 * fills that message in when it has explicitly marked one safe to disclose; by default it withholds
 * it (section 161), so the message arrives empty and the SDK folds the empty value into the bare
 * `symbol` — `error.message` becomes e.g. `UNAUTHENTICATED`. That symbol is internal vocabulary: it
 * must never be shown to a user, both because it leaks machine codes into the UI and because it would
 * make two deliberately-withheld errors — a missing resource versus a privacy-restricted one
 * (section 180) — distinguishable when the server took care to make them identical. So this shows the
 * server's message only when it actually provided one, and a single generic line otherwise. Transport
 * and timeout failures are rephrased for a person. Nothing here logs the error object, which keeps
 * tokens and payloads out of the console by construction.
 */

import { RemoteError, SdkError, TimeoutError, TransportError } from '@migo/sdk';

export function friendlyError(error: unknown): string {
  if (error instanceof RemoteError) {
    // The SDK's JS message composes `symbol: human text`, so the symbol is stripped here rather
    // than compared: "RATE_LIMITED: Too many requests. Retry in 5 s" reaches a person as "Too
    // many requests. Retry in 5 s". When the server disclosed no human text at all, the bare
    // symbol is never surfaced — a withheld error stays unreadable and indistinguishable.
    const human = error.message.startsWith(`${error.symbol}: `)
      ? error.message.slice(error.symbol.length + 2)
      : error.message;
    return human && human !== error.symbol ? human : 'The server rejected the request.';
  }
  if (error instanceof TimeoutError) {
    return 'The server took too long to respond. Check your connection and try again.';
  }
  if (error instanceof TransportError) {
    return 'Could not reach the Migo server. Check your connection and try again.';
  }
  if (error instanceof SdkError) {
    return error.message;
  }
  return 'Something went wrong. Please try again.';
}
