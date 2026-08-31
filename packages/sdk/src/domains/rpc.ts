/**
 * The typed request/event bridge every domain is built on.
 *
 * A domain never touches {@link GatewayTransport} directly. It calls through this thin layer, which
 * pairs an opcode with the generated `encodeX`/`decodeX` for that opcode's payload and response, so a
 * domain method reads as one line — `rpc.call(OP.SYNC, encodeSyncRequest, decodeSyncResponse, req)` —
 * and a mismatched struct is a compile error rather than a wire fault.
 *
 * # Why event decoding is wrapped
 *
 * The transport fans a server event out to its listeners by calling them inline in its receive loop,
 * and it does not catch what a listener throws (see `GatewayTransport`'s dispatch). A listener that
 * throws — because a single event decoded badly, or an application handler had a bug — would escape
 * into the socket's `onmessage` and take down the whole receive path, losing every subsequent frame.
 * So {@link Rpc.on} decodes and dispatches inside a `try`/`catch`: one malformed or mishandled event
 * is routed to an optional error sink and dropped, and the connection keeps delivering.
 */

import { OP, CODE } from '@migo/protocol';
import type { Frame } from '@migo/wire';

import { decodeBody, encodeBody } from '../codec.js';
import type { BodyDecoder, BodyEncoder } from '../codec.js';
import { RemoteError } from '../errors.js';
import type { GatewayTransport } from '../transport.js';

/**
 * The read opcodes a RATE_LIMITED reply retries once against. These are the listing/reporting
 * surfaces a UI fires in bursts as sections open — exactly the traffic the server's advice
 * (`retry_after_ms`) can absorb — and they are all safe to re-issue: none of them mutates
 * anything. Everything else (a message send, a gift, a friend action) surfaces its error, because
 * an automatic second send is how a message arrives twice.
 */
const RETRYABLE_READS: ReadonlySet<number> = new Set([
  OP.CONVERSATION_LIST,
  OP.SYNC,
  OP.ROOM_LIST,
  OP.ROOM_ROSTER,
  OP.RELATIONSHIP_LIST,
  OP.PROFILE_FETCH,
  OP.NOTIFICATION_LIST,
  OP.SEARCH,
  OP.SUGGESTIONS,
  OP.BALANCE_FETCH,
  OP.GIFT_CATALOGUE,
  OP.LEDGER_HISTORY,
  OP.PROGRESSION,
  OP.BADGES,
  OP.LEADERBOARD,
]);

/** The largest backoff a server's advice can command; anything larger is a policy worth surfacing. */
const MAX_RETRY_DELAY_MS = 10_000;

/** Notified when an inbound event fails to decode, or its handler throws. Never fatal. */
export type EventErrorHandler = (opcode: number, cause: unknown) => void;

/** A decoded server event and the frame it arrived on. */
export type EventHandler<Event> = (event: Event, frame: Frame) => void;

/**
 * The request/notify/subscribe surface shared by all domains.
 *
 * Holds no protocol knowledge of its own — the opcode and the codecs are always passed in by the
 * domain — so it stays a mechanical adapter over the transport.
 */
export class Rpc {
  readonly #transport: GatewayTransport;
  readonly #onEventError: EventErrorHandler | undefined;

  constructor(transport: GatewayTransport, onEventError?: EventErrorHandler) {
    this.#transport = transport;
    this.#onEventError = onEventError;
  }

  /**
   * Sends a request and decodes its reply.
   *
   * An ERROR-flagged reply has already rejected as a {@link RemoteError} inside the transport, and a
   * missing reply as a {@link TimeoutError}; this only runs on the success path, so the decode is
   * always against the opcode's declared response type.
   */
  async call<Request, Response>(
    opcode: number,
    encode: BodyEncoder<Request>,
    decode: BodyDecoder<Response>,
    request: Request,
  ): Promise<Response> {
    try {
      const frame = await this.#transport.request(opcode, encodeBody(encode, request));
      return decodeBody(decode, frame.payload);
    } catch (cause) {
      // The server answered a read with "too many, wait N ms" and the client obeys — once. This
      // is the official client's half of the rate-limit contract: it never retries a mutation,
      // and it caps the wait so a hostile or misconfigured node cannot stall a UI for minutes.
      if (
        RETRYABLE_READS.has(opcode) &&
        cause instanceof RemoteError &&
        cause.code === CODE.RATE_LIMITED &&
        cause.retryAfterMs !== undefined
      ) {
        const wait = Math.min(Math.max(0, cause.retryAfterMs), MAX_RETRY_DELAY_MS);
        await new Promise((resolve) => setTimeout(resolve, wait));
        const frame = await this.#transport.request(opcode, encodeBody(encode, request));
        return decodeBody(decode, frame.payload);
      }
      throw cause;
    }
  }

  /** Sends a fire-and-forget frame the protocol gives no reply to (TYPING, MESSAGE_RECEIPT). */
  async notify<Request>(
    opcode: number,
    encode: BodyEncoder<Request>,
    request: Request,
  ): Promise<void> {
    await this.#transport.notify(opcode, encodeBody(encode, request));
  }

  /**
   * Subscribes to a server event opcode, decoding each frame before the handler sees it.
   *
   * A decode failure or a throw from `handler` is delivered to the error sink and swallowed, so it
   * never propagates into the transport's receive loop. Returns the transport's unsubscribe function.
   */
  on<Event>(opcode: number, decode: BodyDecoder<Event>, handler: EventHandler<Event>): () => void {
    return this.#transport.subscribe(opcode, (payload, frame) => {
      let event: Event;
      try {
        event = decodeBody(decode, payload);
      } catch (cause) {
        this.#onEventError?.(opcode, cause);
        return;
      }
      try {
        handler(event, frame);
      } catch (cause) {
        this.#onEventError?.(opcode, cause);
      }
    });
  }
}
