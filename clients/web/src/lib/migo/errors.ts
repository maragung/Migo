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
    // `message === symbol` is the SDK's signal that the server disclosed no human message; never
    // surface the bare symbol, or a withheld error becomes readable and distinguishable.
    return error.message && error.message !== error.symbol
      ? error.message
      : 'The server rejected the request.';
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
