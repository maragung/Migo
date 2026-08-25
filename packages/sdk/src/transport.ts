/**
 * The gateway transport: one WebSocket to a node, driven through the MWP/1 lifecycle.
 *
 * This is the piece that owns the socket. It performs the HELLO/WELCOME handshake, authenticates
 * inline or with a follow-up AUTHENTICATE, keeps the connection alive with client-driven PINGs,
 * matches replies to requests by correlation id, streams server-initiated events to listeners,
 * acknowledges Critical frames by cumulative watermark, and — when the socket drops — reconnects
 * with backoff and resumes the session so in-flight requests are answered by the replay rather than
 * lost.
 *
 * # The sequencing contract
 *
 * The server assigns a `frame_seq` to a server→client frame iff it is Critical and it left through
 * the session mailbox. Two Critical frames bypass the mailbox and carry no seq: WELCOME (written
 * straight to the socket during the handshake) and RECONNECT_HINT (written at graceful shutdown).
 * This transport reconstructs the server's counter by counting every inbound frame for which
 * {@link isSequenced} is true — which excludes those two and every Coalescable/Droppable event.
 * The count is what a resume request replays from and what an ACK watermark reports; an off-by-one
 * here turns a clean resume into a full resync. WELCOME never reaches the counter because it is
 * consumed by the handshake before the counting dispatch path runs; RECONNECT_HINT is excluded by
 * {@link isSequenced} itself.
 *
 * # Layering
 *
 * The transport speaks frames and bytes, not domain types. A caller hands it an opcode and an
 * already-encoded body and gets back a reply frame to decode, or subscribes to an opcode and
 * decodes the event bodies itself. The typed domain wrappers live one layer up.
 */

import { decodeFrame, encodeFrame, frameHeader, maybeDeflate, unpackFrame } from '@migo/wire';
import type { Frame, Id } from '@migo/wire';
import {
  decodeAuthenticated,
  decodeError,
  decodePong,
  decodeReconnectHint,
  decodeWelcome,
  encodeAck,
  encodeAuthenticate,
  encodeHello,
  encodePing,
  FEATURE,
  FLAG,
  OP,
  PROTOCOL_VERSION,
} from '@migo/protocol';
import type {
  BandwidthMode,
  ClientInfo,
  Hello,
  Limits,
  NodeInfo,
  Platform,
  ReconnectHint,
  ResumeRequest,
  Welcome,
} from '@migo/protocol';

import {
  decodeBody,
  encodeBody,
  hasErrorFlag,
  isSequenced,
  opcodeLabel,
  requiresAck,
} from './codec.js';
import { RemoteError, TimeoutError, TransportError } from './errors.js';

/** The feature bits a stock client offers; the server intersects this with its own. */
export const DEFAULT_CLIENT_FEATURES =
  FEATURE.COMPRESSION |
  FEATURE.BATCHING |
  FEATURE.E2E_V1 |
  FEATURE.GROUP_E2E_V1 |
  FEATURE.PRESENCE |
  FEATURE.TYPING |
  FEATURE.ROOMS |
  FEATURE.RESUME |
  FEATURE.VOICE_MESSAGE;

/** How a caller wants to introduce itself in the handshake. */
export interface HelloParams {
  platform: Platform;
  appVersion: string;
  locale: string;
  bandwidthMode: BandwidthMode;
  osVersion?: string;
  deviceModel?: string;
  /** The feature bits to offer; defaults to {@link DEFAULT_CLIENT_FEATURES}. */
  features?: bigint;
  /** An access token to authenticate inline in HELLO; supply with {@link deviceId}. */
  accessToken?: string;
  /** The device this session runs on; required alongside {@link accessToken}. */
  deviceId?: Id;
}

/** A factory for the WebSocket, injectable for Node without a global or for tests. */
export type WebSocketFactory = (url: string) => WebSocket;

/** Options for a {@link GatewayTransport}. */
export interface TransportOptions {
  /** The gateway URL, e.g. `wss://node.example/ws`. */
  url: string;
  /** The handshake parameters. */
  hello: HelloParams;
  /** The WebSocket factory; defaults to the global `WebSocket`. */
  webSocketFactory?: WebSocketFactory;
  /** Milliseconds between heartbeats; defaults to the server's negotiated `heartbeatMs`. */
  heartbeatMs?: number;
  /** Milliseconds to wait for a reply before a {@link TimeoutError}; default 30000. */
  requestTimeoutMs?: number;
  /** The ceiling for reconnect backoff; default 30000. */
  maxReconnectDelayMs?: number;
  /** Called on every connection-state transition. */
  onStateChange?: (state: ConnectionState) => void;
  /** Called when a reconnect could not resume: the session is fresh and app state must resync. */
  onReset?: () => void;
  /** Called with a decoded RECONNECT_HINT before the transport acts on it. */
  onReconnectHint?: (hint: ReconnectHint) => void;
}

/** The lifecycle states a transport moves through. */
export type ConnectionState =
  'idle' | 'connecting' | 'authenticating' | 'ready' | 'reconnecting' | 'closed';

/** The negotiated session, as WELCOME described it. */
export interface SessionInfo {
  sessionId: Id;
  node: NodeInfo;
  features: bigint;
  limits: Limits;
  authenticatedUser: Id | undefined;
  resumed: boolean;
}

/** A handler for a server-initiated event on a given opcode. Receives the decoded-ready payload. */
export type EventHandler = (payload: Uint8Array, frame: Frame) => void;

/** A reply awaiting its correlation, held between send and receive. */
interface Pending {
  resolve: (frame: Frame) => void;
  reject: (error: Error) => void;
  timer: ReturnType<typeof setTimeout>;
  opcode: number;
}

const DEFAULT_REQUEST_TIMEOUT_MS = 30_000;
const DEFAULT_MAX_RECONNECT_DELAY_MS = 30_000;
const RECONNECT_BASE_MS = 500;
/** A short window to coalesce many ACK-required frames into a single watermark ACK. */
const ACK_COALESCE_MS = 5;
/** WebSocket.OPEN, spelled out so the DOM lib constant isn't required at call sites. */
const WS_OPEN = 1;

/**
 * A single gateway connection.
 *
 * Construct it, `await connect()`, then issue {@link request}s and {@link subscribe} to events.
 * The transport reconnects on its own after an unexpected drop; call {@link close} to stop it for
 * good.
 */
export class GatewayTransport {
  readonly #options: TransportOptions;
  readonly #features: bigint;
  readonly #requestTimeoutMs: number;
  readonly #maxReconnectDelayMs: number;
  readonly #makeSocket: WebSocketFactory;

  #ws: WebSocket | null = null;
  #state: ConnectionState = 'idle';
  #session: SessionInfo | null = null;

  /** The running access token / device id, updated by {@link reauthenticate} and reused on resume. */
  #accessToken: string | undefined;
  #deviceId: Id | undefined;

  /** Correlation id generator; 0 is reserved for events and ACKs. */
  #nextCorrelationId = 1;
  readonly #pending = new Map<number, Pending>();
  readonly #eventHandlers = new Map<number, Set<EventHandler>>();

  /** The count of inbound Critical frames — the server's `frame_seq` mirror. */
  #lastServerSeq = 0;
  #lastAckedSeq = 0;
  #ackTimer: ReturnType<typeof setTimeout> | null = null;

  #heartbeatTimer: ReturnType<typeof setTimeout> | null = null;
  #reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  #reconnectAttempt = 0;
  /** False once {@link close} is called, so an intentional close does not trigger a reconnect. */
  #shouldReconnect = true;
  /** True while a reconnect (as opposed to a first connect) is in flight. */
  #isReconnect = false;

  /** The in-flight handshake's settlers, or null when not handshaking. */
  #handshake: { resolve: () => void; reject: (error: Error) => void } | null = null;
  /** True between sending HELLO and consuming WELCOME. */
  #awaitingWelcome = false;

  constructor(options: TransportOptions) {
    this.#options = options;
    this.#features = options.hello.features ?? DEFAULT_CLIENT_FEATURES;
    this.#requestTimeoutMs = options.requestTimeoutMs ?? DEFAULT_REQUEST_TIMEOUT_MS;
    this.#maxReconnectDelayMs = options.maxReconnectDelayMs ?? DEFAULT_MAX_RECONNECT_DELAY_MS;
    this.#accessToken = options.hello.accessToken;
    this.#deviceId = options.hello.deviceId;
    const factory = options.webSocketFactory ?? defaultWebSocketFactory();
    this.#makeSocket = factory;
  }

  /** The current lifecycle state. */
  get state(): ConnectionState {
    return this.#state;
  }

  /** The negotiated session, or null before the first WELCOME. */
  get session(): SessionInfo | null {
    return this.#session;
  }

  /** The features the server confirmed, or 0 before the handshake. */
  get negotiatedFeatures(): bigint {
    return this.#session?.features ?? 0n;
  }

  /**
   * Opens the connection and settles when the session is Ready.
   *
   * Rejects if the server refuses the handshake (an unsupported version, a rejected token). After
   * a successful connect, later drops are handled by the internal reconnect loop, not by this call.
   */
  connect(): Promise<void> {
    this.#shouldReconnect = true;
    this.#isReconnect = false;
    return this.#open();
  }

  /**
   * Replaces the credentials used for inline auth and resume, and — if already connected but not
   * authenticated — sends an AUTHENTICATE now.
   *
   * Used after a REST refresh mints a new access token mid-session.
   */
  async reauthenticate(accessToken: string, deviceId: Id): Promise<void> {
    this.#accessToken = accessToken;
    this.#deviceId = deviceId;
    if (this.#state === 'ready' && this.#session?.authenticatedUser === undefined) {
      await this.#authenticate();
    }
  }

  /**
   * Sends a request and resolves with its reply frame.
   *
   * The caller decodes the reply body with the decoder for the opcode's response type. An
   * ERROR-flagged reply rejects with a {@link RemoteError}; no reply within the timeout rejects
   * with a {@link TimeoutError}.
   */
  async request(opcode: number, body: Uint8Array): Promise<Frame> {
    if (this.#state !== 'ready' || this.#ws === null || this.#ws.readyState !== WS_OPEN) {
      throw new TransportError(`cannot send ${opcodeLabel(opcode)}: transport is ${this.#state}`);
    }
    const correlation = this.#allocateCorrelation();
    const bytes = await this.#buildFrame(opcode, correlation, body);
    return await new Promise<Frame>((resolve, reject) => {
      const timer = setTimeout(() => {
        if (this.#pending.delete(correlation)) {
          reject(
            new TimeoutError(
              `request ${opcodeLabel(opcode)} timed out after ${this.#requestTimeoutMs}ms`,
            ),
          );
        }
      }, this.#requestTimeoutMs);
      this.#pending.set(correlation, { resolve, reject, timer, opcode });
      try {
        this.#ws?.send(bytes);
      } catch (cause) {
        clearTimeout(timer);
        this.#pending.delete(correlation);
        reject(new TransportError(`failed to send ${opcodeLabel(opcode)}: ${String(cause)}`));
      }
    });
  }

  /**
   * Sends a fire-and-forget frame with no reply expected.
   *
   * Used for the opcodes the protocol table gives no response — TYPING, MESSAGE_RECEIPT. The frame
   * rides correlation 0, marking it uncorrelated.
   */
  async notify(opcode: number, body: Uint8Array): Promise<void> {
    if (this.#state !== 'ready' || this.#ws === null || this.#ws.readyState !== WS_OPEN) {
      throw new TransportError(`cannot send ${opcodeLabel(opcode)}: transport is ${this.#state}`);
    }
    const bytes = await this.#buildFrame(opcode, 0, body);
    this.#ws.send(bytes);
  }

  /**
   * Registers a listener for a server-initiated event opcode.
   *
   * Returns an unsubscribe function. Multiple listeners on one opcode all fire.
   */
  subscribe(opcode: number, handler: EventHandler): () => void {
    let set = this.#eventHandlers.get(opcode);
    if (set === undefined) {
      set = new Set();
      this.#eventHandlers.set(opcode, set);
    }
    set.add(handler);
    return () => {
      this.#eventHandlers.get(opcode)?.delete(handler);
    };
  }

  /** Closes the connection for good; no reconnect follows. */
  close(): void {
    this.#shouldReconnect = false;
    this.#clearTimers();
    this.#rejectAllPending(new TransportError('transport closed'));
    if (this.#ws !== null) {
      try {
        this.#ws.close(1000, 'client shutdown');
      } catch {
        // A close on an already-closing socket is harmless.
      }
      this.#ws = null;
    }
    this.#setState('closed');
  }

  // --- connection lifecycle -------------------------------------------------------------------

  /** Opens a socket and settles when the handshake reaches Ready. */
  #open(): Promise<void> {
    this.#setState(this.#isReconnect ? 'reconnecting' : 'connecting');
    return new Promise<void>((resolve, reject) => {
      this.#handshake = { resolve, reject };
      let ws: WebSocket;
      try {
        ws = this.#makeSocket(this.#options.url);
      } catch (cause) {
        this.#handshake = null;
        reject(new TransportError(`failed to open socket: ${String(cause)}`));
        return;
      }
      ws.binaryType = 'arraybuffer';
      this.#ws = ws;
      ws.onopen = () => {
        void this.#sendHello();
      };
      ws.onmessage = (event: MessageEvent) => {
        this.#onMessage(event.data);
      };
      ws.onerror = () => {
        // `onclose` always follows and carries the actionable outcome; nothing to do here.
      };
      ws.onclose = (event: CloseEvent) => {
        this.#onClose(event.code, event.reason);
      };
    });
  }

  /** Builds and sends the HELLO frame, with a resume request when reconnecting. */
  async #sendHello(): Promise<void> {
    this.#awaitingWelcome = true;
    const params = this.#options.hello;
    const client: ClientInfo = {
      platform: params.platform,
      appVersion: params.appVersion,
      ...(params.osVersion !== undefined ? { osVersion: params.osVersion } : {}),
      ...(params.deviceModel !== undefined ? { deviceModel: params.deviceModel } : {}),
    };
    const resume = this.#resumeRequest();
    const hello: Hello = {
      protocolVersion: PROTOCOL_VERSION,
      client,
      features: this.#features,
      locale: params.locale,
      bandwidthMode: params.bandwidthMode,
      ...(this.#accessToken !== undefined ? { accessToken: this.#accessToken } : {}),
      ...(this.#deviceId !== undefined ? { deviceId: this.#deviceId } : {}),
      ...(resume !== undefined ? { resume } : {}),
    };
    try {
      const bytes = await this.#buildFrame(
        OP.HELLO,
        this.#allocateCorrelation(),
        encodeBody(encodeHello, hello),
      );
      this.#ws?.send(bytes);
    } catch (cause) {
      this.#failHandshake(new TransportError(`failed to send HELLO: ${String(cause)}`));
    }
  }

  /** The resume request to attach to HELLO, or undefined on a first connect. */
  #resumeRequest(): ResumeRequest | undefined {
    if (!this.#isReconnect || this.#session === null) {
      return undefined;
    }
    return { sessionId: this.#session.sessionId, lastFrameSeq: this.#lastServerSeq };
  }

  /** Consumes the WELCOME reply: records the session, then authenticates or goes Ready. */
  #onWelcome(frame: Frame): void {
    this.#awaitingWelcome = false;
    if (hasErrorFlag(frame)) {
      this.#failHandshake(RemoteError.fromMessage(decodeBody(decodeError, frame.payload)));
      return;
    }
    let welcome: Welcome;
    try {
      welcome = decodeBody(decodeWelcome, frame.payload);
    } catch (cause) {
      this.#failHandshake(new TransportError(`malformed WELCOME: ${String(cause)}`));
      return;
    }
    const resumed = welcome.resumed === true;
    this.#session = {
      sessionId: welcome.sessionId,
      node: welcome.node,
      features: welcome.features,
      limits: welcome.limits,
      authenticatedUser: welcome.authenticatedUser,
      resumed,
    };

    // A resumed session keeps its seq space and its in-flight requests: the server replays the
    // Critical frames past our watermark, and those replies resolve the pending promises. A fresh
    // session on a reconnect means the old requests will never be answered — reject them and tell
    // the app to resync — and the seq space restarts at zero.
    if (this.#isReconnect && !resumed) {
      this.#rejectAllPending(new TransportError('session could not be resumed'));
      this.#lastServerSeq = 0;
      this.#lastAckedSeq = 0;
      this.#options.onReset?.();
    }

    if (resumed || welcome.authenticatedUser !== undefined) {
      this.#onReady();
      return;
    }
    // AwaitingAuth: present the token now. The AUTHENTICATED reply is the first mailbox frame, so
    // it is counted as seq 1 by the normal dispatch path.
    this.#setState('authenticating');
    this.#authenticate().then(
      () => this.#onReady(),
      (error: unknown) =>
        this.#failHandshake(error instanceof Error ? error : new TransportError(String(error))),
    );
  }

  /** Sends AUTHENTICATE and awaits AUTHENTICATED. */
  async #authenticate(): Promise<void> {
    if (this.#accessToken === undefined || this.#deviceId === undefined) {
      throw new RemoteError(1100, 'UNAUTHENTICATED', 'no access token to authenticate with');
    }
    const body = encodeBody(encodeAuthenticate, {
      accessToken: this.#accessToken,
      deviceId: this.#deviceId,
    });
    const reply = await this.request(OP.AUTHENTICATE, body);
    const authenticated = decodeBody(decodeAuthenticated, reply.payload);
    if (this.#session !== null) {
      this.#session = { ...this.#session, authenticatedUser: authenticated.userId };
    }
  }

  /** Marks the session Ready, settling the handshake and starting the heartbeat. */
  #onReady(): void {
    this.#reconnectAttempt = 0;
    this.#setState('ready');
    this.#startHeartbeat();
    const settle = this.#handshake;
    this.#handshake = null;
    settle?.resolve();
  }

  /** Fails the in-flight handshake and closes the socket. */
  #failHandshake(error: Error): void {
    const settle = this.#handshake;
    this.#handshake = null;
    this.#awaitingWelcome = false;
    // A handshake rejection is terminal for this attempt; do not auto-reconnect a refused token or
    // an unsupported version, which would just be refused again.
    this.#shouldReconnect = false;
    if (this.#ws !== null) {
      try {
        this.#ws.close();
      } catch {
        // ignore
      }
      this.#ws = null;
    }
    this.#setState('closed');
    settle?.reject(error);
  }

  /** Handles socket closure: reconnect with backoff unless the close was intentional. */
  #onClose(code: number, reason: string): void {
    this.#stopHeartbeat();
    this.#ws = null;
    if (this.#handshake !== null) {
      // Closed before the handshake settled — surface it to the connect() caller.
      this.#failHandshake(
        new TransportError(`connection closed during handshake (${code} ${reason})`),
      );
      return;
    }
    if (!this.#shouldReconnect) {
      this.#setState('closed');
      return;
    }
    this.#scheduleReconnect();
  }

  /** Schedules a reconnect after an exponential, jittered backoff. */
  #scheduleReconnect(): void {
    this.#setState('reconnecting');
    this.#isReconnect = true;
    const exponential = RECONNECT_BASE_MS * 2 ** this.#reconnectAttempt;
    const capped = Math.min(this.#maxReconnectDelayMs, exponential);
    const jittered = capped * (0.5 + Math.random() * 0.5);
    this.#reconnectAttempt += 1;
    this.#reconnectTimer = setTimeout(() => {
      this.#open().catch(() => {
        // A failed reconnect closes the socket, whose onclose schedules the next attempt; unless
        // the failure was a terminal handshake rejection, which cleared #shouldReconnect.
        if (this.#shouldReconnect) {
          this.#scheduleReconnect();
        }
      });
    }, jittered);
  }

  // --- inbound --------------------------------------------------------------------------------

  /** Decodes a WebSocket message into frames and dispatches each. */
  #onMessage(data: unknown): void {
    const bytes = toBytes(data);
    if (bytes === null) {
      // A Blob (no arraybuffer binaryType) resolves asynchronously; dispatch when it lands.
      if (typeof Blob !== 'undefined' && data instanceof Blob) {
        void data.arrayBuffer().then((buffer) => this.#dispatchBytes(new Uint8Array(buffer)));
      }
      return;
    }
    this.#dispatchBytes(bytes);
  }

  /** Unpacks a frame (inflating and de-batching) and routes every sub-frame. */
  #dispatchBytes(bytes: Uint8Array): void {
    let outer: Frame;
    try {
      outer = decodeFrame(bytes);
    } catch {
      // A frame we cannot decode is a protocol fault we cannot recover from on this socket.
      this.#ws?.close();
      return;
    }
    void unpackFrame(outer).then(
      (frames) => {
        for (const frame of frames) {
          this.#handleFrame(frame);
        }
      },
      () => {
        this.#ws?.close();
      },
    );
  }

  /** Routes one decoded inbound frame. */
  #handleFrame(frame: Frame): void {
    // The first frame after HELLO is the WELCOME (or a handshake error); it bypasses the mailbox
    // and must not be counted, so it is intercepted before the sequencing path.
    if (this.#awaitingWelcome) {
      this.#onWelcome(frame);
      return;
    }

    if (isSequenced(frame)) {
      this.#lastServerSeq += 1;
    }
    if (requiresAck(frame)) {
      this.#scheduleAck();
    }

    const opcode = frame.header.opcode;

    // RECONNECT_HINT is a server-initiated instruction to migrate; act on it after notifying.
    if (opcode === OP.RECONNECT_HINT && frame.header.correlation === 0) {
      this.#onReconnectHint(frame);
      return;
    }

    const correlation = frame.header.correlation;
    if (correlation !== 0) {
      const pending = this.#pending.get(correlation);
      if (pending !== undefined) {
        this.#pending.delete(correlation);
        clearTimeout(pending.timer);
        if (hasErrorFlag(frame)) {
          pending.reject(RemoteError.fromMessage(decodeBody(decodeError, frame.payload)));
        } else {
          pending.resolve(frame);
        }
        return;
      }
      // A correlated frame with no pending entry: a late reply to a request we already timed out,
      // or one whose promise was rejected on a failed resume. Nothing to deliver it to.
      return;
    }

    // Correlation 0: a server-initiated event. Fan out to opcode listeners.
    const handlers = this.#eventHandlers.get(opcode);
    if (handlers !== undefined) {
      for (const handler of handlers) {
        handler(frame.payload, frame);
      }
    }
  }

  /** Acts on a RECONNECT_HINT: notify, then reconnect after the server's grace period. */
  #onReconnectHint(frame: Frame): void {
    let hint: ReconnectHint;
    try {
      hint = decodeBody(decodeReconnectHint, frame.payload);
    } catch {
      return;
    }
    this.#options.onReconnectHint?.(hint);
    // The server is about to close us; let it, and reconnect after the hinted delay. The onclose
    // path drives the actual reconnect, so here we only make sure we will try.
    this.#shouldReconnect = true;
  }

  // --- acknowledgement ------------------------------------------------------------------------

  /** Schedules a single cumulative ACK for the current watermark, coalescing bursts. */
  #scheduleAck(): void {
    if (this.#ackTimer !== null) {
      return;
    }
    this.#ackTimer = setTimeout(() => {
      this.#ackTimer = null;
      void this.#flushAck();
    }, ACK_COALESCE_MS);
  }

  /** Sends an ACK carrying the highest seq counted, if it has advanced since the last one. */
  async #flushAck(): Promise<void> {
    if (this.#lastServerSeq <= this.#lastAckedSeq) {
      return;
    }
    if (this.#state !== 'ready' || this.#ws === null || this.#ws.readyState !== WS_OPEN) {
      return;
    }
    const watermark = this.#lastServerSeq;
    try {
      const bytes = await this.#buildFrame(
        OP.ACK,
        0,
        encodeBody(encodeAck, { frameSeq: watermark }),
      );
      this.#ws.send(bytes);
      this.#lastAckedSeq = watermark;
    } catch {
      // A failed ACK is advisory; the next ACK-required frame reschedules one.
    }
  }

  // --- heartbeat ------------------------------------------------------------------------------

  /** Starts the client-driven heartbeat at the negotiated (or overridden) interval. */
  #startHeartbeat(): void {
    this.#stopHeartbeat();
    const interval = this.#options.heartbeatMs ?? this.#session?.limits.heartbeatMs ?? 30_000;
    const tick = (): void => {
      this.#heartbeatTimer = setTimeout(() => {
        void this.#beat(tick);
      }, interval);
    };
    tick();
  }

  /** Sends one PING; a missing PONG within the request timeout drops the socket to force a resume. */
  async #beat(scheduleNext: () => void): Promise<void> {
    if (this.#state !== 'ready') {
      return;
    }
    try {
      const reply = await this.request(OP.PING, encodeBody(encodePing, { clientTime: Date.now() }));
      decodeBody(decodePong, reply.payload);
      scheduleNext();
    } catch {
      // A dead heartbeat means the link is gone; closing triggers the reconnect+resume path.
      if (this.#ws !== null) {
        try {
          this.#ws.close();
        } catch {
          // ignore
        }
      }
    }
  }

  /** Stops the heartbeat timer. */
  #stopHeartbeat(): void {
    if (this.#heartbeatTimer !== null) {
      clearTimeout(this.#heartbeatTimer);
      this.#heartbeatTimer = null;
    }
  }

  // --- helpers --------------------------------------------------------------------------------

  /** Builds a wire frame, compressing the payload when the server negotiated compression. */
  async #buildFrame(opcode: number, correlation: number, body: Uint8Array): Promise<Uint8Array> {
    let payload = body;
    let flags = 0;
    if ((this.negotiatedFeatures & FEATURE.COMPRESSION) !== 0n) {
      const deflated = await maybeDeflate(body);
      if (deflated !== null) {
        payload = deflated;
        flags |= FLAG.COMPRESSED;
      }
    }
    const frame: Frame = {
      header: { ...frameHeader(opcode, correlation), flags },
      payload,
    };
    return encodeFrame(frame);
  }

  /** Allocates the next correlation id, skipping 0 and wrapping at the u32 boundary. */
  #allocateCorrelation(): number {
    let id = this.#nextCorrelationId;
    this.#nextCorrelationId = (this.#nextCorrelationId + 1) >>> 0;
    if (this.#nextCorrelationId === 0) {
      this.#nextCorrelationId = 1;
    }
    if (id === 0) {
      id = 1;
    }
    return id;
  }

  /** Rejects and clears every pending request. */
  #rejectAllPending(error: Error): void {
    for (const pending of this.#pending.values()) {
      clearTimeout(pending.timer);
      pending.reject(error);
    }
    this.#pending.clear();
  }

  /** Clears every timer the transport owns. */
  #clearTimers(): void {
    this.#stopHeartbeat();
    if (this.#reconnectTimer !== null) {
      clearTimeout(this.#reconnectTimer);
      this.#reconnectTimer = null;
    }
    if (this.#ackTimer !== null) {
      clearTimeout(this.#ackTimer);
      this.#ackTimer = null;
    }
  }

  /** Transitions state and notifies the observer. */
  #setState(state: ConnectionState): void {
    if (this.#state === state) {
      return;
    }
    this.#state = state;
    this.#options.onStateChange?.(state);
  }
}

/** Returns a factory over the global `WebSocket`, or throws if none exists. */
function defaultWebSocketFactory(): WebSocketFactory {
  const ctor = (globalThis as { WebSocket?: new (url: string) => WebSocket }).WebSocket;
  if (ctor === undefined) {
    throw new TypeError('no global WebSocket found; pass options.webSocketFactory');
  }
  return (url: string) => new ctor(url);
}

/** Normalises a WebSocket message payload to bytes, or null when it needs async conversion. */
function toBytes(data: unknown): Uint8Array | null {
  if (data instanceof Uint8Array) {
    return data;
  }
  if (data instanceof ArrayBuffer) {
    return new Uint8Array(data);
  }
  if (ArrayBuffer.isView(data)) {
    return new Uint8Array(data.buffer, data.byteOffset, data.byteLength);
  }
  return null;
}
