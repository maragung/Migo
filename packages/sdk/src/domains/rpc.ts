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

import type { Frame } from '@migo/wire';

import { decodeBody, encodeBody } from '../codec.js';
import type { BodyDecoder, BodyEncoder } from '../codec.js';
import type { GatewayTransport } from '../transport.js';

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
    const frame = await this.#transport.request(opcode, encodeBody(encode, request));
    return decodeBody(decode, frame.payload);
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
