/**
 * Maps an SDK error to a short, user-facing message.
 *
 * The server only ever sends a curated public message across the wire (internal detail is never
 * exposed), so a {@link RemoteError}'s message is safe to show as-is. Transport and timeout failures
 * are rephrased for a person. Nothing here logs the error object, which keeps tokens and payloads out
 * of the console by construction.
 */

import { RemoteError, SdkError, TimeoutError, TransportError } from '@migo/sdk';

export function friendlyError(error: unknown): string {
  if (error instanceof RemoteError) {
    return error.message || 'The server rejected the request.';
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
