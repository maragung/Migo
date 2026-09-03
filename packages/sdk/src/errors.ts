/**
 * The SDK's error vocabulary.
 *
 * Two failure sources meet here. The gateway answers a request with an `ERROR`-flagged frame
 * whose body is a protocol {@link ProtocolErrorMessage}; the REST bootstrap answers with a JSON
 * envelope `{ "error": { code, symbol, message, retry_after_ms } }`. Both carry the same stable
 * {@link CODE} and a machine-readable `symbol`, so both become a {@link RemoteError} and a caller
 * branches on `error.code`/`error.symbol` — never on the human-facing `message`, which section 161
 * forbids the server from making meaningful and which a client must never localise from.
 *
 * The transport's own failures — a socket that closed, a handshake the server rejected, an RPC
 * that never answered — are not protocol errors and get their own types, so a caller can tell "the
 * server said no" ({@link RemoteError}) from "the connection is gone" ({@link TransportError}).
 */

import { CODE } from '@migo/protocol';
import type { Error as ProtocolErrorMessage } from '@migo/protocol';
import type { Id } from '@migo/wire';

import type { CaptchaChallenge, CaptchaMode } from './rest.js';

/** The closed set of error codes, re-exported so callers branch on a name, not a magic number. */
export { CODE } from '@migo/protocol';

/** Base class for everything this SDK throws, so `instanceof SdkError` catches all of it. */
export class SdkError extends Error {
  constructor(message: string) {
    super(message);
    this.name = 'SdkError';
  }
}

/**
 * The server refused a request, over either transport.
 *
 * `code` and `symbol` are the contract; `message` is a developer-facing hint that may be empty and
 * must never be shown to a user or parsed. `retryAfterMs` is set on {@link CODE.RATE_LIMITED} and
 * the transient server faults, and {@link retryable} reads it back as a boolean.
 */
export class RemoteError extends SdkError {
  /** Stable numeric code from {@link CODE}. */
  readonly code: number;
  /** Stable machine-readable symbol, e.g. `RATE_LIMITED`. Localise from this, never from message. */
  readonly symbol: string;
  /** Milliseconds to wait before retrying, when the server suggested one. */
  readonly retryAfterMs: number | undefined;
  /** The offending field, for a validation error. */
  readonly field: string | undefined;
  /**
   * A fresh captcha challenge the server attached to this refusal, when there was one to
   * attach: a bootstrap attempt that carried a proof spent it whatever the verdict, so the
   * refusal hands the next challenge over in the same response. `undefined` on every other
   * error — a caller reads it to swap the captcha picture without a second round trip, and
   * never has to fetch a replacement the server already minted.
   */
  readonly captcha: CaptchaChallenge | undefined;

  constructor(
    code: number,
    symbol: string,
    message: string,
    retryAfterMs?: number,
    field?: string,
    captcha?: CaptchaChallenge,
  ) {
    // The symbol leads the JS message so a stack trace is legible; the human string, which may be
    // empty by design, only follows when present.
    super(message ? `${symbol}: ${message}` : symbol);
    this.name = 'RemoteError';
    this.code = code;
    this.symbol = symbol;
    this.retryAfterMs = retryAfterMs;
    this.field = field;
    this.captcha = captcha;
  }

  /** Whether the server asked the caller to back off and try again. */
  get retryable(): boolean {
    return this.retryAfterMs !== undefined;
  }

  /** Builds a {@link RemoteError} from a decoded protocol `Error` frame body. */
  static fromMessage(error: ProtocolErrorMessage): RemoteError {
    return new RemoteError(
      error.code,
      error.symbol,
      error.message ?? '',
      error.retryAfterMs,
      error.field,
    );
  }

  /**
   * Builds a {@link RemoteError} from the REST error envelope.
   *
   * The envelope is `{ "error": { code, symbol, message, retry_after_ms } }` (snake_case, as the
   * server emits it) — plus, on the captcha-gated bootstrap refusals, an optional `captcha`
   * object carrying the replacement challenge (see {@link RemoteError.captcha}). A body that
   * does not match that shape becomes a generic {@link CODE.INTERNAL_ERROR}, since a malformed
   * error is still an error and the caller should not have to guess.
   */
  static fromEnvelope(status: number, body: unknown): RemoteError {
    const envelope = (body as { error?: unknown } | null)?.error;
    if (envelope !== null && typeof envelope === 'object') {
      const record = envelope as Record<string, unknown>;
      const code = typeof record['code'] === 'number' ? record['code'] : CODE.INTERNAL_ERROR;
      const symbol = typeof record['symbol'] === 'string' ? record['symbol'] : 'INTERNAL_ERROR';
      const message = typeof record['message'] === 'string' ? record['message'] : '';
      const retryAfterMs =
        typeof record['retry_after_ms'] === 'number' ? record['retry_after_ms'] : undefined;
      const field = typeof record['field'] === 'string' ? record['field'] : undefined;
      const captcha = captchaFrom(record['captcha']);
      return new RemoteError(code, symbol, message, retryAfterMs, field, captcha);
    }
    return new RemoteError(CODE.INTERNAL_ERROR, 'INTERNAL_ERROR', `HTTP ${status}`);
  }
}

/**
 * Coerces an error envelope's optional `captcha` object into a {@link CaptchaChallenge}, or
 * `undefined` when it is absent or not challenge-shaped.
 *
 * Defensive for the same reason {@link RemoteError.fromEnvelope} is: a malformed attachment
 * must not turn a readable refusal into an exception, and the caller that ignores `captcha`
 * loses nothing. The shape mirrors what `POST /v1/auth/captcha` returns, so a form can hand
 * the object to its widget without translation.
 */
function captchaFrom(raw: unknown): CaptchaChallenge | undefined {
  if (typeof raw !== 'object' || raw === null) {
    return undefined;
  }
  const record = raw as Record<string, unknown>;
  if (
    typeof record['challenge_id'] !== 'string' ||
    typeof record['image_png_base64'] !== 'string'
  ) {
    return undefined;
  }
  const mode: CaptchaMode = record['mode'] === 'image_alt' ? 'image_alt' : 'image';
  const ttl = typeof record['ttl_seconds'] === 'number' ? record['ttl_seconds'] : 0;
  return {
    // The string came off a JSON body the server shaped, so it is already the wire form of an
    // id; the brand marks what every other challenge in the SDK already is.
    challenge_id: record['challenge_id'] as Id,
    image_png_base64: record['image_png_base64'],
    mode,
    ttl_seconds: ttl,
  };
}

/**
 * The connection failed or was closed.
 *
 * Distinct from a {@link RemoteError}: the server did not answer a request with a verdict, the
 * link itself is unusable. `reason` is one of the protocol {@link CODE} symbols when the close
 * carried one, or a transport-level description otherwise.
 */
export class TransportError extends SdkError {
  constructor(message: string) {
    super(message);
    this.name = 'TransportError';
  }
}

/** A request was sent but no reply arrived before the deadline. */
export class TimeoutError extends SdkError {
  constructor(message: string) {
    super(message);
    this.name = 'TimeoutError';
  }
}
