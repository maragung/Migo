/**
 * The composition root: one object that wires every layer into a usable client.
 *
 * The SDK is built in layers that know nothing of each other — `@migo/wire` frames bytes, `@migo/crypto`
 * runs the ratchets, the transport resumes a socket, each domain speaks one slice of the protocol. This
 * module is where they meet. {@link MigoClient} owns the lifecycle (bootstrap over REST, then a resumable
 * gateway session), constructs the two crypto layers over a single {@link KeyStore}, hands every domain the
 * shared {@link Rpc}, and supplies the two seams the messaging layer cannot fill itself: which devices a
 * broadcast must reach ({@link DeviceDirectory}) and how to fetch a peer's key bundle ({@link
 * PeerBundleSource}).
 *
 * # Why the client backs the device directory
 *
 * The server fans a message out to every *subscribed* device on a conversation topic except the sender's
 * own sending device. To seal the one-time sender-key distribution the messaging layer must therefore know
 * that exact audience, and only the client knows it: it holds the conversation's membership and this
 * device's own identity. {@link recipientDevices} answers with every member's devices — this account's own
 * other devices included, for multi-device sync — minus the one device we are sending from. Membership is
 * cached (primed whenever the client sees a {@link ConversationSummary}), and per-user device lists are
 * cached too, so the steady-state send path makes no extra round trip: {@link
 * GroupCrypto.needsDistribution} finds everyone already holds the key and nothing is fetched or sealed.
 *
 * # Why the client is also the bundle source
 *
 * Enumerating a user's devices ({@link KeysDomain.fetchDeviceBundles}) already returns each device's full
 * bundle, and the very next thing the messaging layer does with a device that needs the key is run X3DH,
 * which needs that same bundle. Fetching twice would consume two of the peer's one-time prekeys for one
 * session. So the client caches each enumerated bundle and serves it to {@link SessionCrypto} exactly once
 * ({@link fetchBundle}), spending a single prekey per new session; a device we never enumerated falls
 * through to a direct fetch.
 *
 * # Subscriptions are explicit, and survive a reset
 *
 * The gateway delivers a topic's events only to sessions that have subscribed to it (the hub keys its
 * fan-out by subscriber set), so receiving a conversation's messages requires subscribing to its topic.
 * The client tracks every topic it subscribed to; when a reconnect cannot resume and the session is fresh
 * ({@link TransportOptions.onReset}), those subscriptions are gone server-side, so the client re-subscribes
 * them all before handing control to the application's own resync.
 */

import type { Id } from '@migo/wire';
import type { PrekeyBundle } from '@migo/crypto';
import {
  OP,
  TopicKind,
  encodeSubscribeRequest,
  decodeSubscribeResponse,
  decodeAcknowledged,
  encodePing,
  decodePong,
} from '@migo/protocol';
import type {
  Acknowledged,
  ConversationSummary,
  ConversationListResponse,
  SubscribeResponse,
  SyncResponse,
  Topic,
} from '@migo/protocol';

import { BootstrapClient } from './rest.js';
import type { DeviceDescriptor, FetchLike, Grant, LoginParams, RegisterParams } from './rest.js';
import { DEFAULT_CLIENT_FEATURES, GatewayTransport } from './transport.js';
import type {
  ConnectionState,
  HelloParams,
  TransportOptions,
  WebSocketFactory,
} from './transport.js';
import { SdkError } from './errors.js';
import { SessionCrypto } from './session-crypto.js';
import type { PeerBundleSource } from './session-crypto.js';
import { GroupCrypto } from './group-crypto.js';
import { Rpc } from './domains/rpc.js';
import type { EventErrorHandler } from './domains/rpc.js';
import { KeysDomain, KeyStore } from './domains/keys.js';
import { MessagingDomain } from './domains/messaging.js';
import type { CreateConversationOptions } from './domains/conversations.js';
import { ConversationsDomain } from './domains/conversations.js';
import { SyncDomain } from './domains/sync.js';
import { TypingDomain } from './domains/typing.js';
import { PresenceDomain } from './domains/presence.js';
import { RoomsDomain } from './domains/rooms.js';
import { ProfileDomain } from './domains/profile.js';
import { NotificationsDomain } from './domains/notifications.js';
import { GamesDomain } from './domains/games.js';
import type { DeviceAddress, DeviceDirectory } from './domains/messaging.js';
import type { ConversationKind } from '@migo/protocol';

/** The handshake parameters minus the credentials the client fills from a {@link Grant}. */
export type ClientHello = Omit<HelloParams, 'accessToken' | 'deviceId'>;

/**
 * How a batch of one-time prekeys is topped up. When the pool falls to {@link low} or below, the client
 * mints and publishes {@link batch} fresh keys after any operation that consumes them.
 */
export interface PrekeyReplenishPolicy {
  /** Publish more once the unused pool is at or below this many keys. */
  low: number;
  /** How many fresh one-time prekeys to mint and publish when topping up. */
  batch: number;
}

/** Everything needed to construct a client. One instance drives one device's session. */
export interface MigoClientOptions {
  /** The REST origin for bootstrap (register, login, refresh), e.g. `https://api.migo.example`. */
  baseUrl: string;
  /** The gateway WebSocket URL, e.g. `wss://node.migo.example/ws`. */
  gatewayUrl: string;
  /** The handshake parameters; the access token and device id are supplied from the grant. */
  hello: ClientHello;
  /** The human-readable device name recorded on the account's device list. */
  deviceDisplayName: string;
  /** A previously-assigned device id to re-present on login, for a returning device. */
  deviceId?: Id;
  /** This device's key material; a fresh one is minted if omitted. Restore one to keep an identity. */
  keyStore?: KeyStore;
  /** The `WebSocket` implementation, for a non-browser host. */
  webSocketFactory?: WebSocketFactory;
  /** The `fetch` implementation for REST, for a non-browser host. */
  fetch?: FetchLike;
  /** Milliseconds to wait for a gateway reply before timing out. */
  requestTimeoutMs?: number;
  /** Milliseconds between heartbeats; defaults to the server's negotiated value. */
  heartbeatMs?: number;
  /** The ceiling for reconnect backoff. */
  maxReconnectDelayMs?: number;
  /** When to top up the one-time prekey pool; defaults to {@link DEFAULT_REPLENISH_POLICY}. */
  replenishPolicy?: PrekeyReplenishPolicy;
  /** Notified when an inbound event fails to decode or a handler throws; never fatal. */
  onEventError?: EventErrorHandler;
  /** Notified on every connection-state transition. */
  onStateChange?: (state: ConnectionState) => void;
  /** Notified after a fresh (non-resumed) session has been re-subscribed, for application resync. */
  onReset?: () => void;
}

/** The default prekey top-up: replenish a full batch once fewer than sixteen remain. */
export const DEFAULT_REPLENISH_POLICY: PrekeyReplenishPolicy = { low: 16, batch: 64 };

/**
 * The live object graph for one connected session.
 *
 * Held together so the whole set appears and disappears atomically: it exists only between a successful
 * {@link MigoClient.register}/{@link MigoClient.login}/{@link MigoClient.resume} and {@link
 * MigoClient.disconnect}. The domains are reachable through the client's getters, which read this.
 */
interface Connected {
  grant: Grant;
  transport: GatewayTransport;
  rpc: Rpc;
  keys: KeysDomain;
  sessionCrypto: SessionCrypto;
  groupCrypto: GroupCrypto;
  messaging: MessagingDomain;
  conversations: ConversationsDomain;
  sync: SyncDomain;
  typing: TypingDomain;
  presence: PresenceDomain;
  rooms: RoomsDomain;
  profile: ProfileDomain;
  notifications: NotificationsDomain;
  games: GamesDomain;
}

/**
 * A Migo client for one device.
 *
 * Construct it with {@link MigoClient.create}, then bring it online with {@link register} (a new account),
 * {@link login} (an existing one), or {@link resume} (a grant persisted from a previous run). Once
 * connected, the domain getters expose the protocol surface, and the orchestration helpers on this class
 * ({@link startConversation}, {@link loadConversations}, {@link watchConversation}, {@link catchUp}) wire
 * the pieces the domains deliberately leave to the composition root — subscription and membership.
 */
export class MigoClient implements DeviceDirectory, PeerBundleSource {
  readonly #options: MigoClientOptions;
  readonly #bootstrap: BootstrapClient;
  readonly #keyStore: KeyStore;
  readonly #replenishPolicy: PrekeyReplenishPolicy;

  /** Conversation id to its member account ids, primed from summaries; backs {@link recipientDevices}. */
  readonly #members = new Map<Id, Id[]>();
  /** Account id to its device ids, cached so the steady-state send path makes no round trip. */
  readonly #userDevices = new Map<Id, Id[]>();
  /** `${userId}|${deviceId}` to a bundle enumerated but not yet spent, served once to {@link fetchBundle}. */
  readonly #bundleCache = new Map<string, PrekeyBundle>();
  /** Every topic we have an active subscription to, re-sent after a session reset. */
  readonly #subscribedTopics = new Map<string, Topic>();

  #ctx: Connected | null = null;

  private constructor(options: MigoClientOptions) {
    this.#options = options;
    const bootstrapOptions = options.fetch !== undefined ? { fetch: options.fetch } : {};
    this.#bootstrap = new BootstrapClient(options.baseUrl, bootstrapOptions);
    this.#keyStore = options.keyStore ?? KeyStore.create();
    this.#replenishPolicy = options.replenishPolicy ?? DEFAULT_REPLENISH_POLICY;
  }

  /** Builds a client. No network activity happens until {@link register}, {@link login}, or {@link resume}. */
  static create(options: MigoClientOptions): MigoClient {
    return new MigoClient(options);
  }

  // --- identity and lifecycle state ---

  /** This device's key material, for the caller to snapshot to secure local storage between runs. */
  get keyStore(): KeyStore {
    return this.#keyStore;
  }

  /** This account's id, once connected. */
  get accountId(): Id {
    return this.#requireConnected().grant.accountId;
  }

  /** This device's id, once connected. */
  get deviceId(): Id {
    return this.#requireConnected().grant.deviceId;
  }

  /** The credentials the current session was established with, for the caller to persist and later {@link resume}. */
  get grant(): Grant {
    return this.#requireConnected().grant;
  }

  /** The current connection state, or `'closed'` when not connected. */
  get connectionState(): ConnectionState {
    return this.#ctx?.transport.state ?? 'closed';
  }

  /** Whether a session is currently established. */
  get connected(): boolean {
    return this.#ctx !== null;
  }

  // --- domain accessors (throw until connected) ---

  /** The key-directory domain: publish our public keys, fetch peers'. */
  get keys(): KeysDomain {
    return this.#requireConnected().keys;
  }

  /** Send and receive end-to-end encrypted messages. */
  get messaging(): MessagingDomain {
    return this.#requireConnected().messaging;
  }

  /** List and create conversations. */
  get conversations(): ConversationsDomain {
    return this.#requireConnected().conversations;
  }

  /** Fetch conversation history to catch up on missed messages. */
  get sync(): SyncDomain {
    return this.#requireConnected().sync;
  }

  /** Publish and observe typing indicators. */
  get typing(): TypingDomain {
    return this.#requireConnected().typing;
  }

  /** Publish and observe presence. */
  get presence(): PresenceDomain {
    return this.#requireConnected().presence;
  }

  /** Browse, join, leave, and observe rooms. */
  get rooms(): RoomsDomain {
    return this.#requireConnected().rooms;
  }

  /** Look up public account profiles. */
  get profile(): ProfileDomain {
    return this.#requireConnected().profile;
  }

  /** Receive server-pushed notification events. */
  get notifications(): NotificationsDomain {
    return this.#requireConnected().notifications;
  }

  /** Submit game actions and observe game events. */
  get games(): GamesDomain {
    return this.#requireConnected().games;
  }

  // --- bringing the client online ---

  /**
   * Registers a new account for this device, then connects.
   *
   * The device descriptor is filled from the client's {@link ClientHello} and display name. Resolves with
   * the grant so the caller can persist it (alongside {@link keyStore}'s snapshot) and later {@link resume}
   * without re-registering.
   */
  async register(params: Omit<RegisterParams, 'device'>): Promise<Grant> {
    const grant = await this.#bootstrap.register({ ...params, device: this.#deviceDescriptor() });
    await this.#establish(grant);
    return grant;
  }

  /**
   * Logs an existing account in on this device, then connects.
   *
   * Resolves with the grant, which carries the (possibly newly assigned) device id; persist it with the
   * key-store snapshot to {@link resume} later.
   */
  async login(params: Omit<LoginParams, 'device'>): Promise<Grant> {
    const grant = await this.#bootstrap.login({ ...params, device: this.#deviceDescriptor() });
    await this.#establish(grant);
    return grant;
  }

  /**
   * Connects using a grant persisted from a previous run, skipping bootstrap.
   *
   * Pair this with a restored {@link KeyStore} (passed as {@link MigoClientOptions.keyStore}) so the device
   * keeps its identity across restarts. If the grant's access token has expired the transport's first
   * request will fail; refresh it with {@link refreshSession} first, or fall back to {@link login}.
   */
  async resume(grant: Grant): Promise<void> {
    await this.#establish(grant);
  }

  /**
   * Refreshes the access token using the stored refresh token and re-authenticates the live session.
   *
   * Resolves with the new grant. Call it before {@link resume} when a persisted access token may have
   * expired, or proactively before {@link Grant.accessExpiresAtMs}.
   */
  async refreshSession(): Promise<Grant> {
    const current = this.#requireConnected();
    const refreshed = await this.#bootstrap.refresh({
      refreshToken: current.grant.refreshToken,
      deviceId: current.grant.deviceId,
    });
    await current.transport.reauthenticate(refreshed.accessToken, refreshed.deviceId);
    this.#ctx = { ...current, grant: refreshed };
    return refreshed;
  }

  /**
   * Closes the session and tears down the object graph.
   *
   * Idempotent. Crypto state lives in the {@link KeyStore} and the crypto layers, not in the transport, so
   * a later {@link resume} on a client built with the same key store continues the same identity. The
   * membership and device caches are cleared, since they are only an optimisation and are rebuilt on demand.
   */
  disconnect(): Promise<void> {
    const ctx = this.#ctx;
    if (ctx === null) {
      return Promise.resolve();
    }
    ctx.messaging.stop();
    ctx.typing.stop();
    ctx.presence.stop();
    ctx.rooms.stop();
    ctx.notifications.stop();
    ctx.games.stop();
    ctx.transport.close();
    this.#ctx = null;
    this.#members.clear();
    this.#userDevices.clear();
    this.#bundleCache.clear();
    this.#subscribedTopics.clear();
    return Promise.resolve();
  }

  // --- topic subscription ---

  /**
   * Subscribes to a set of topics, so the gateway begins delivering their events.
   *
   * Tracked for re-subscription after a session reset. Returns the server's accepted and rejected sets; a
   * rejected topic means the per-session subscription cap was hit.
   */
  async subscribe(topics: Topic[]): Promise<SubscribeResponse> {
    const ctx = this.#requireConnected();
    const response = await ctx.rpc.call(
      OP.SUBSCRIBE,
      encodeSubscribeRequest,
      decodeSubscribeResponse,
      { topics },
    );
    for (const topic of response.accepted) {
      this.#subscribedTopics.set(topicKey(topic), topic);
    }
    return response;
  }

  /** Unsubscribes from a set of topics and stops tracking them. */
  async unsubscribe(topics: Topic[]): Promise<Acknowledged> {
    const ctx = this.#requireConnected();
    const acknowledged = await ctx.rpc.call(
      OP.UNSUBSCRIBE,
      encodeSubscribeRequest,
      decodeAcknowledged,
      { topics },
    );
    for (const topic of topics) {
      this.#subscribedTopics.delete(topicKey(topic));
    }
    return acknowledged;
  }

  /** Subscribes to a conversation's topic, the prerequisite for receiving its messages and receipts. */
  async watchConversation(conversationId: Id): Promise<void> {
    await this.subscribe([{ kind: TopicKind.Conversation, id: conversationId }]);
  }

  /** Stops receiving a conversation's events. */
  async unwatchConversation(conversationId: Id): Promise<void> {
    await this.unsubscribe([{ kind: TopicKind.Conversation, id: conversationId }]);
  }

  /** Subscribes to a room's topic, for its membership and state events. */
  async watchRoom(roomId: Id): Promise<void> {
    await this.subscribe([{ kind: TopicKind.Room, id: roomId }]);
  }

  /** Subscribes to a user's topic, for that account's presence changes. */
  async watchUser(userId: Id): Promise<void> {
    await this.subscribe([{ kind: TopicKind.User, id: userId }]);
  }

  // --- orchestration helpers (wire subscription and membership to the domains) ---

  /**
   * Creates a conversation, primes its membership, and subscribes to it in one step.
   *
   * The returned summary is ready to send to: its membership is cached for {@link recipientDevices} and its
   * topic is subscribed so replies arrive. Prefer this over {@link ConversationsDomain.create} directly,
   * which does neither.
   */
  async startConversation(
    kind: ConversationKind,
    members: Id[],
    options: CreateConversationOptions = {},
  ): Promise<ConversationSummary> {
    const summary = await this.conversations.create(kind, members, options);
    this.rememberConversation(summary);
    if (this.#members.get(summary.conversationId) === undefined) {
      this.rememberMembers(summary.conversationId, members);
    }
    await this.watchConversation(summary.conversationId);
    return summary;
  }

  /**
   * Lists conversations, priming each one's membership and subscribing to it.
   *
   * After this returns, every listed conversation can be sent to and will deliver inbound events. Page with
   * the returned {@link ConversationListResponse.nextCursor}.
   */
  async loadConversations(limit: number, cursor?: string): Promise<ConversationListResponse> {
    const response = await this.conversations.list(limit, cursor);
    const topics: Topic[] = [];
    for (const summary of response.conversations) {
      this.rememberConversation(summary);
      topics.push({ kind: TopicKind.Conversation, id: summary.conversationId });
    }
    if (topics.length > 0) {
      await this.subscribe(topics);
    }
    return response;
  }

  /**
   * Fetches history for a conversation and replays it through the live decryption path.
   *
   * Each fetched event is fed to {@link MessagingDomain.ingest} in the order the server returned it, so a
   * historical key exchange rebuilds the sender's session before the content it unlocks. De-duplicate
   * against live delivery by sequence number: the ratchet rejects decrypting the same message twice, so a
   * message already seen live and then re-seen here is dropped rather than delivered twice. Resolves with
   * the raw {@link SyncResponse} for its paging cursors and truncation status.
   */
  async catchUp(conversationId: Id, haveSeq: number, limit = 200): Promise<SyncResponse> {
    const ctx = this.#requireConnected();
    const response = await ctx.sync.fetch(conversationId, haveSeq, limit);
    for (const event of response.messages) {
      ctx.messaging.ingest(event);
    }
    return response;
  }

  // --- membership cache priming ---

  /** Caches a summary's membership, if it carries one, so sends to it need no extra round trip. */
  rememberConversation(summary: ConversationSummary): void {
    if (summary.members !== undefined) {
      this.#members.set(summary.conversationId, summary.members);
    }
  }

  /** Explicitly sets a conversation's membership, for a handle that arrived without one (e.g. a room). */
  rememberMembers(conversationId: Id, members: Id[]): void {
    this.#members.set(conversationId, members);
  }

  /** Forgets a user's cached device list, so the next send re-enumerates it (e.g. after a key change). */
  invalidateDevices(userId: Id): void {
    this.#userDevices.delete(userId);
  }

  /** Forgets a conversation's cached membership, so the next send re-reads it. */
  invalidateConversation(conversationId: Id): void {
    this.#members.delete(conversationId);
  }

  // --- DeviceDirectory ---

  /**
   * The devices a sender key must reach for a conversation, excluding our own sending device.
   *
   * The audience is every member's devices unioned with this account's own — so our other devices sync —
   * minus the one device we send from. Membership must have been primed ({@link rememberConversation} or
   * the orchestration helpers); an unknown conversation is a programming error and throws rather than
   * silently sealing for no one.
   */
  async recipientDevices(conversationId: Id): Promise<DeviceAddress[]> {
    const ctx = this.#requireConnected();
    const members = this.#members.get(conversationId);
    if (members === undefined) {
      throw new SdkError(
        `migo: membership for conversation ${conversationId} is unknown; ` +
          'call startConversation, loadConversations, or rememberMembers first',
      );
    }
    const audience = new Set<Id>(members);
    audience.add(ctx.grant.accountId);

    const devices: DeviceAddress[] = [];
    for (const userId of audience) {
      for (const deviceId of await this.#devicesFor(userId, ctx.keys)) {
        // Exclude only this sending device; our other devices belong in the audience for sync.
        if (deviceId === ctx.grant.deviceId) {
          continue;
        }
        devices.push({ userId, deviceId });
      }
    }
    return devices;
  }

  // --- PeerBundleSource ---

  /**
   * Fetches one device's key bundle for the 1:1 layer to run X3DH.
   *
   * Serves a bundle already enumerated by {@link recipientDevices} exactly once — spending one of the
   * peer's one-time prekeys rather than a second one — then falls through to a direct fetch for a device we
   * never enumerated. The bundle is not verified here; {@link SessionCrypto} verifies it before any key
   * agreement, which is the single place verification must live.
   */
  async fetchBundle(userId: Id, deviceId: Id): Promise<PrekeyBundle> {
    const ctx = this.#requireConnected();
    const cacheKey = bundleKey(userId, deviceId);
    const cached = this.#bundleCache.get(cacheKey);
    if (cached !== undefined) {
      this.#bundleCache.delete(cacheKey);
      return cached;
    }
    return ctx.keys.fetchBundle(userId, deviceId);
  }

  // --- key material maintenance ---

  /** Publishes this device's current public key material. */
  async publishKeys(): Promise<void> {
    await this.#requireConnected().keys.publish();
  }

  /**
   * Tops up the one-time prekey pool if it has run low, and republishes.
   *
   * Fetching a bundle consumes one of our prekeys server-side, so a device that receives many first
   * messages drains the pool; this mints and publishes a fresh batch once it falls to the policy's low-water
   * mark. Safe to call after any inbound key exchange. Returns whether it published.
   */
  async replenishPrekeys(): Promise<boolean> {
    const ctx = this.#requireConnected();
    if (this.#keyStore.oneTimePrekeyCount() > this.#replenishPolicy.low) {
      return false;
    }
    this.#keyStore.replenishOneTimePrekeys(this.#replenishPolicy.batch);
    await ctx.keys.publish();
    return true;
  }

  // --- internals ---

  /** Builds the object graph, connects, publishes keys, and starts every inbound stream. */
  async #establish(grant: Grant): Promise<void> {
    if (this.#ctx !== null) {
      throw new SdkError('migo: already connected; call disconnect before connecting again');
    }
    const transport = new GatewayTransport(this.#transportOptions(grant));
    await transport.connect();

    const rpc = new Rpc(transport, this.#options.onEventError);
    // Server-initiated heartbeats arrive as a PING on the wire (brief 139
    // reuses the opcode for both directions). The client must answer each
    // one with a PONG or the server closes the session as
    // `heartbeat_timeout` and inbound events stop arriving. A frame that
    // is the reply itself is also a PING and the codec hands the handler
    // a decoded Pong, so we read the result and drop it.
    rpc.on(OP.PING, decodePong, () => {
      void rpc.call(OP.PING, encodePing, decodePong, { clientTime: Date.now() }).catch(() => {
        // The server is welcome to drop the session if it cannot tolerate
        // a missing PONG; the next keepalive will close the loop and
        // the caller will see the disconnect.
      });
    });
    const keys = new KeysDomain(rpc, this.#keyStore);
    const sessionCrypto = new SessionCrypto(this.#keyStore, this);
    const groupCrypto = new GroupCrypto(this.#keyStore);

    const ctx: Connected = {
      grant,
      transport,
      rpc,
      keys,
      sessionCrypto,
      groupCrypto,
      messaging: new MessagingDomain(
        rpc,
        sessionCrypto,
        groupCrypto,
        this,
        this.#options.onEventError,
      ),
      conversations: new ConversationsDomain(rpc),
      sync: new SyncDomain(rpc),
      typing: new TypingDomain(rpc, this.#options.onEventError),
      presence: new PresenceDomain(rpc, this.#options.onEventError),
      rooms: new RoomsDomain(rpc, this.#options.onEventError),
      profile: new ProfileDomain(rpc),
      notifications: new NotificationsDomain(rpc, this.#options.onEventError),
      games: new GamesDomain(rpc, this.#options.onEventError),
    };
    this.#ctx = ctx;

    // Register inbound handlers before anything can be pushed to us.
    ctx.messaging.start();
    ctx.typing.start();
    ctx.presence.start();
    ctx.rooms.start();
    ctx.notifications.start();
    ctx.games.start();

    await keys.publish();
    // Our own user topic carries self-directed events: presence sync across our devices, notifications.
    await this.subscribe([{ kind: TopicKind.User, id: grant.accountId }]);
  }

  /** The device list for a user, from cache or a single enumeration that also warms the bundle cache. */
  async #devicesFor(userId: Id, keys: KeysDomain): Promise<Id[]> {
    const cached = this.#userDevices.get(userId);
    if (cached !== undefined) {
      return cached;
    }
    const bundles = await keys.fetchDeviceBundles(userId);
    const deviceIds: Id[] = [];
    for (const entry of bundles) {
      deviceIds.push(entry.deviceId);
      this.#bundleCache.set(bundleKey(userId, entry.deviceId), entry.bundle);
    }
    this.#userDevices.set(userId, deviceIds);
    return deviceIds;
  }

  /** Re-subscribes every tracked topic after a fresh session, then hands off to the app's resync. */
  #handleReset(): void {
    const topics = Array.from(this.#subscribedTopics.values());
    if (topics.length > 0 && this.#ctx !== null) {
      // Fire-and-forget: a failure here is routed to the event-error sink, not thrown into the transport.
      this.#ctx.rpc
        .call(OP.SUBSCRIBE, encodeSubscribeRequest, decodeSubscribeResponse, { topics })
        .catch((cause: unknown) => this.#options.onEventError?.(OP.SUBSCRIBE, cause));
    }
    this.#options.onReset?.();
  }

  /** The transport options assembled from the client options plus the grant's credentials. */
  #transportOptions(grant: Grant): TransportOptions {
    const hello: HelloParams = {
      ...this.#options.hello,
      features: this.#options.hello.features ?? DEFAULT_CLIENT_FEATURES,
      accessToken: grant.accessToken,
      deviceId: grant.deviceId,
    };
    const options: TransportOptions = {
      url: this.#options.gatewayUrl,
      hello,
      onStateChange: (state) => this.#options.onStateChange?.(state),
      onReset: () => this.#handleReset(),
    };
    if (this.#options.webSocketFactory !== undefined) {
      options.webSocketFactory = this.#options.webSocketFactory;
    }
    if (this.#options.heartbeatMs !== undefined) {
      options.heartbeatMs = this.#options.heartbeatMs;
    }
    if (this.#options.requestTimeoutMs !== undefined) {
      options.requestTimeoutMs = this.#options.requestTimeoutMs;
    }
    if (this.#options.maxReconnectDelayMs !== undefined) {
      options.maxReconnectDelayMs = this.#options.maxReconnectDelayMs;
    }
    return options;
  }

  /** The device descriptor sent to bootstrap, built from the hello parameters and display name. */
  #deviceDescriptor(): DeviceDescriptor {
    const device: DeviceDescriptor = {
      platform: this.#options.hello.platform,
      displayName: this.#options.deviceDisplayName,
      appVersion: this.#options.hello.appVersion,
    };
    if (this.#options.deviceId !== undefined) {
      device.deviceId = this.#options.deviceId;
    }
    if (this.#options.hello.osVersion !== undefined) {
      device.osVersion = this.#options.hello.osVersion;
    }
    if (this.#options.hello.deviceModel !== undefined) {
      device.deviceModel = this.#options.hello.deviceModel;
    }
    return device;
  }

  /** Throws if no session is established, otherwise returns the live object graph. */
  #requireConnected(): Connected {
    if (this.#ctx === null) {
      throw new SdkError('migo: not connected; call register, login, or resume first');
    }
    return this.#ctx;
  }
}

/** The tracking key for a subscribed topic. */
function topicKey(topic: Topic): string {
  return `${topic.kind}:${topic.id}`;
}

/** The cache key for one device's bundle. */
function bundleKey(userId: Id, deviceId: Id): string {
  return `${userId}|${deviceId}`;
}
