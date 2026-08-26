package com.migo.core

import com.migo.core.crypto.PrekeyBundle
import com.migo.core.domain.ConversationsDomain
import com.migo.core.domain.DeviceAddress
import com.migo.core.domain.DeviceDirectory
import com.migo.core.domain.EventErrorHandler
import com.migo.core.domain.GamesDomain
import com.migo.core.domain.IncomingMessage
import com.migo.core.domain.KeyStore
import com.migo.core.domain.KeysDomain
import com.migo.core.domain.Listener
import com.migo.core.domain.ListenerSet
import com.migo.core.domain.MessageDeletion
import com.migo.core.domain.MessagingDomain
import com.migo.core.domain.NotificationsDomain
import com.migo.core.domain.PresenceDomain
import com.migo.core.domain.ProfileDomain
import com.migo.core.domain.RoomsDomain
import com.migo.core.domain.Rpc
import com.migo.core.domain.SdkError
import com.migo.core.domain.Subscription
import com.migo.core.domain.SyncDomain
import com.migo.core.domain.TypingDomain
import com.migo.core.net.DeviceRequest
import com.migo.core.net.Gateway
import com.migo.core.net.Grant
import com.migo.core.net.Rest
import com.migo.core.protocol.Acknowledged
import com.migo.core.protocol.BandwidthMode
import com.migo.core.protocol.ClientInfo
import com.migo.core.protocol.ConversationKind
import com.migo.core.protocol.ConversationListResponse
import com.migo.core.protocol.ConversationSummary
import com.migo.core.protocol.Feature
import com.migo.core.protocol.GameEvent
import com.migo.core.protocol.Hello
import com.migo.core.protocol.MessageReceipt
import com.migo.core.protocol.NotificationEvent
import com.migo.core.protocol.Op
import com.migo.core.protocol.PROTOCOL_VERSION
import com.migo.core.protocol.Platform
import com.migo.core.protocol.PresenceEvent
import com.migo.core.protocol.ResumeRequest
import com.migo.core.protocol.RoomMemberEvent
import com.migo.core.protocol.RoomStateEvent
import com.migo.core.protocol.SubscribeRequest
import com.migo.core.protocol.SubscribeResponse
import com.migo.core.protocol.SyncResponse
import com.migo.core.protocol.Topic
import com.migo.core.protocol.TopicKind
import com.migo.core.protocol.TypingEvent
import com.migo.core.session.GroupCrypto
import com.migo.core.session.GroupPersistence
import com.migo.core.session.PeerBundleSource
import com.migo.core.session.SessionCrypto
import com.migo.core.session.SessionPersistence
import com.migo.core.store.DeviceKeys
import com.migo.core.store.SavedSession
import com.migo.core.wire.Id
import com.migo.core.wire.NIL_ID
import com.migo.core.wire.parseId
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlin.random.Random
import okhttp3.OkHttpClient

/**
 * The composition root: one object that wires every layer into a usable client.
 *
 * The SDK is built in layers that know nothing of each other -- `wire` frames bytes, `crypto` runs the
 * ratchets, [Gateway] carries frames, each domain speaks one slice of the protocol. This is where they
 * meet. Mirrors `packages/sdk/src/client.ts`, and diverges from it in exactly two places, both forced
 * by this platform and both described below.
 *
 * # Why the client backs the device directory
 *
 * The server fans a message out to every *subscribed* device on a conversation topic except the
 * sender's own sending device. To seal the one-time sender-key distribution the messaging layer must
 * know that exact audience, and only the client knows it: it holds the conversation's membership and
 * this device's own identity. [recipientDevices] answers with every member's devices -- this account's
 * own other devices included, for multi-device sync -- minus the one device we send from. Membership is
 * cached, and per-user device lists are cached too, so the steady-state send path makes no extra round
 * trip: [GroupCrypto.needsDistribution] finds everyone already holds the key and nothing is fetched or
 * sealed.
 *
 * # Why the client is also the bundle source
 *
 * Enumerating a user's devices ([KeysDomain.fetchDeviceBundles]) already returns each device's full
 * bundle, and the very next thing the messaging layer does with a device that needs the key is run
 * X3DH, which needs that same bundle. Fetching twice would consume two of the peer's one-time prekeys
 * for one session. So the client caches each enumerated bundle and serves it to [SessionCrypto] exactly
 * once ([fetchBundle]), spending a single prekey per new session; a device never enumerated falls
 * through to a direct fetch.
 *
 * # The first divergence: the client owns the reconnect loop
 *
 * The web SDK has a `GatewayTransport` that survives a reconnect, so its whole object graph is built
 * once. This platform's [Gateway] is one socket and [Rpc] is bound to one [Gateway], because the
 * sequence bookkeeping that feeds the ACK watermark has to be exact and a transport that swapped
 * sockets underneath a frame counter is how that stops being true. So a reconnect here rebuilds the
 * graph, and this class owns the loop that does it: connect, pump [Rpc.deliver] until it reports the
 * connection died, back off, reconnect with a [ResumeRequest], and re-subscribe if the server could not
 * resume.
 *
 * # The second divergence: application listeners live here, not on the domains
 *
 * Because the graph is rebuilt, a handler registered on a domain object would be silently dropped at
 * the first subway tunnel. So the application-facing streams are [ListenerSet]s owned by this class and
 * bridged to each freshly built domain -- [onMessage], [onTyping], [onPresence] and the rest. Register
 * once, keep receiving across any number of reconnects. The domain objects remain reachable through the
 * accessors for everything else, and a caller that registers directly on one of those is opting into
 * the per-connection lifetime knowingly.
 *
 * What deliberately does *not* get rebuilt is the crypto: [SessionCrypto] and [GroupCrypto] are
 * constructed once, so every ratchet and sender-key chain survives a reconnect. Rebuilding those would
 * mean a dropped connection cost the user every session they had established.
 *
 * The one thing a reconnect does lose is [MessagingDomain]'s buffer of content that arrived before its
 * sender key. That is recoverable and not a leak: the crypto layer kept the key material, so the same
 * messages re-fetched by [catchUp] open on arrival.
 *
 * # Subscriptions are explicit, and survive a reset
 *
 * The gateway delivers a topic's events only to sessions that subscribed to it, so receiving a
 * conversation's messages requires subscribing to its topic. Every subscribed topic is tracked; when a
 * reconnect cannot resume, those subscriptions are gone server-side, so all of them are re-sent before
 * [MigoClientOptions.onReset] hands control to the application's own resync.
 */

/** Where the connection currently is. Reported through [MigoClientOptions.onStateChange]. */
enum class ConnectionState {
    /** A socket is being opened, or the handshake is in flight. */
    Connecting,

    /** Authenticated and delivering. */
    Online,

    /** The connection died and the client is backing off before trying again. */
    Reconnecting,

    /** No connection, and none being attempted. */
    Closed,
}

/**
 * How a batch of one-time prekeys is topped up.
 *
 * When the unused pool falls to [low] or below, [MigoClient.replenishPrekeys] mints and publishes
 * [batch] fresh keys. The sum has to stay within the server's ceiling -- see [DEFAULT_REPLENISH_POLICY].
 */
class PrekeyReplenishPolicy(
    /** Publish more once the unused pool is at or below this many keys. */
    val low: Int,
    /** How many fresh one-time prekeys to mint and publish when topping up. */
    val batch: Int,
)

/**
 * The default prekey top-up: replenish a batch of sixty-four once sixteen or fewer remain.
 *
 * The two numbers are chosen against the server's cap rather than picked for roundness. `migo-keys`
 * enforces `MAX_ONE_TIME_PREKEYS = 100`, and [KeyStore.replenishOneTimePrekeys] mints only into the room
 * that is left, so a policy whose low-water plus batch exceeded the cap would quietly publish fewer keys
 * than it asked for and keep re-triggering. Sixteen plus sixty-four is eighty, comfortably under.
 */
val DEFAULT_REPLENISH_POLICY = PrekeyReplenishPolicy(low = 16, batch = 64)

/**
 * The features this build actually implements, as the HELLO bitmask.
 *
 * Announced honestly: a bit here is a promise that the code behind it exists, and the server may enable
 * behaviour on the strength of it. [Feature.MEDIA_UPLOAD] and [Feature.VOICE_MESSAGE] are absent because
 * their upload path is still specification (migo.md sections 167 and 168) -- this client can decode a
 * media or voice-note reference in a message, which is a different thing from being able to produce one.
 * [Feature.BOTS], [Feature.TRANSLATION], [Feature.ECONOMY] and [Feature.QUIC] are absent for the same
 * reason, and [Feature.TRACING] because a mobile client has no trace sink to send to.
 *
 * The negotiated set is [com.migo.core.protocol.Welcome.features], which is the *intersection*. Nothing
 * should read this constant to decide what a live session can do.
 */
val DEFAULT_CLIENT_FEATURES: ULong = Feature.COMPRESSION or
    Feature.BATCHING or
    Feature.E2E_V1 or
    Feature.GROUP_E2E_V1 or
    Feature.PRESENCE or
    Feature.TYPING or
    Feature.ROOMS or
    Feature.GAMES or
    Feature.RESUME

/** Everything needed to construct a client. One instance drives one device's session. */
class MigoClientOptions(
    /** The REST origin for bootstrap, e.g. `https://api.migo.example`. */
    val baseUrl: String,
    /** This app's version, reported in HELLO and on the account's device list. */
    val appVersion: String,
    /**
     * The gateway WebSocket URL. Derived from [baseUrl] when null, which is what keeps a user who
     * typed one address from ending up with an API and a gateway pointing at different deployments.
     */
    val gatewayUrl: String? = null,
    /** The Android release, e.g. `"14"`. Shown on the device list so a session can be recognised. */
    val osVersion: String? = null,
    /** The device model, for the same reason. Nothing finer: see [DeviceRequest.describe]. */
    val deviceModel: String? = null,
    /** The user's language tag, for server-composed strings such as notification bodies. */
    val locale: String = "en",
    /** How much bandwidth the server may spend on this session. */
    val bandwidthMode: BandwidthMode = BandwidthMode.Auto,
    /** The features to announce. Defaults to [DEFAULT_CLIENT_FEATURES]; the server intersects. */
    val features: ULong = DEFAULT_CLIENT_FEATURES,
    /** A previously-assigned device id to re-present at sign-in, for a returning device. */
    val deviceId: Id? = null,
    /** This device's key material. A fresh one is minted if omitted; restore one to keep an identity. */
    val keyStore: KeyStore? = null,
    /** Where the 1:1 ratchets are persisted. Defaults to memory only. */
    val sessionPersistence: SessionPersistence = SessionPersistence.None,
    /** Where the sender-key chains are persisted. Defaults to memory only. */
    val groupPersistence: GroupPersistence = GroupPersistence.None,
    /** When to top up the one-time prekey pool. */
    val replenishPolicy: PrekeyReplenishPolicy = DEFAULT_REPLENISH_POLICY,
    /** The ceiling for reconnect backoff. */
    val maxReconnectDelayMs: Long = 30_000L,
    /**
     * The HTTP client for REST.
     *
     * Separate from [socketClient] on purpose: REST wants a call timeout so a sign-in either answers or
     * fails, and a WebSocket must not have one -- a call timeout on a socket would close a healthy
     * connection the moment it outlived the deadline.
     */
    val restClient: OkHttpClient? = null,
    /** The HTTP client for the gateway socket. Defaults to [Gateway.httpClient]. */
    val socketClient: OkHttpClient? = null,
    /**
     * The scope the connection's heartbeat and the messaging layer's event handling run in.
     *
     * Must outlive the connection: a scope tied to a screen would cancel the heartbeat when the user
     * navigated away, and the session would then die on the server's liveness deadline. Defaults to one
     * the client owns and [MigoClient.close] cancels.
     */
    val scope: CoroutineScope? = null,
    /** Notified when an inbound event fails to decode or a handler throws. Never fatal. */
    val onEventError: EventErrorHandler? = null,
    /** Notified when a connection attempt or a live connection failed. The client retries anyway. */
    val onConnectionError: ((Throwable) -> Unit)? = null,
    /** Notified on every connection-state transition. */
    val onStateChange: ((ConnectionState) -> Unit)? = null,
    /** Notified after a fresh (non-resumed) session has been re-subscribed, for application resync. */
    val onReset: (() -> Unit)? = null,
)

/**
 * A Migo client for one device.
 *
 * Build it with [MigoClient.create], then bring it online with [register] (a new account), [login] (an
 * existing one), or [resume] (a grant persisted from a previous run). Once connected, the domain
 * accessors expose the protocol surface and the orchestration helpers ([startConversation],
 * [loadConversations], [watchConversation], [catchUp]) wire up the two things the domains deliberately
 * leave to the composition root: subscription and membership.
 */
class MigoClient private constructor(
    private val options: MigoClientOptions,
    private val ownedScope: CoroutineScope?,
    private val scope: CoroutineScope,
) : DeviceDirectory, PeerBundleSource {

    private val rest = Rest(options.baseUrl, options.restClient)
    private val socketClient: OkHttpClient = options.socketClient ?: Gateway.httpClient()
    private val gatewayUrl: String = options.gatewayUrl ?: rest.gatewayUrl()

    /** This device's key material, for the caller to snapshot to a [com.migo.core.store.Vault]. */
    val keyStore: KeyStore = options.keyStore ?: KeyStore.create()

    /**
     * The 1:1 layer. Built once and shared by every connection, so ratchets survive a reconnect.
     *
     * `this` is the [PeerBundleSource]: the client is what holds the bundle cache that makes one
     * enumeration serve both the directory lookup and the X3DH that follows it.
     */
    private val sessionCrypto = SessionCrypto(keyStore, this, options.sessionPersistence)

    /** The group layer. Built once for the same reason: a reconnect must not retire a sender key. */
    private val groupCrypto = GroupCrypto(keyStore, options.groupPersistence)

    // Application-facing streams. Owned here rather than on the domains so a handler registered once
    // keeps receiving across the reconnects that rebuild them.
    private val messageListeners = ListenerSet<IncomingMessage>(Op.MESSAGE_EVENT, options.onEventError)
    private val deletionListeners = ListenerSet<MessageDeletion>(Op.MESSAGE_EVENT, options.onEventError)
    private val receiptListeners = ListenerSet<MessageReceipt>(Op.MESSAGE_RECEIPT, options.onEventError)
    private val typingListeners = ListenerSet<TypingEvent>(Op.TYPING, options.onEventError)
    private val presenceListeners = ListenerSet<PresenceEvent>(Op.PRESENCE_EVENT, options.onEventError)
    private val memberListeners =
        ListenerSet<RoomMemberEvent>(Op.ROOM_MEMBER_EVENT, options.onEventError)
    private val roomStateListeners =
        ListenerSet<RoomStateEvent>(Op.ROOM_STATE_EVENT, options.onEventError)
    private val notificationListeners =
        ListenerSet<NotificationEvent>(Op.NOTIFICATION_EVENT, options.onEventError)
    private val gameListeners = ListenerSet<GameEvent>(Op.GAME_EVENT, options.onEventError)

    /** Conversation id to its member account ids, primed from summaries; backs [recipientDevices]. */
    private val members = HashMap<Id, List<Id>>()

    /** Account id to its device ids, cached so the steady-state send path makes no round trip. */
    private val userDevices = HashMap<Id, List<Id>>()

    /** `userId|deviceId` to a bundle enumerated but not yet spent, served once to [fetchBundle]. */
    private val bundleCache = HashMap<String, PrekeyBundle>()

    /** Every topic with an active subscription, re-sent after a session reset. */
    private val subscribedTopics = HashMap<String, Topic>()

    /** Guards the four caches above, which the reconnect loop and the send path both touch. */
    private val cacheLock = Mutex()

    /** Serialises establish, reconnect and disconnect so two of them cannot race on [live]. */
    private val lifecycleLock = Mutex()

    @Volatile
    private var live: Session? = null

    @Volatile
    private var state: ConnectionState = ConnectionState.Closed

    @Volatile
    private var closing = false

    private var supervisor: Job? = null

    private val backoff = Backoff(options.maxReconnectDelayMs)

    // --- identity and lifecycle state ---

    /** This account's id, once connected. */
    val accountId: Id get() = requireConnected().accountId

    /** This device's id, once connected. */
    val deviceId: Id get() = requireConnected().deviceId

    /** The credentials the current session runs on, for the caller to persist and later [resume]. */
    val grant: Grant get() = requireConnected().grant

    /** The current connection state. */
    val connectionState: ConnectionState get() = state

    /** Whether a session is currently established. */
    val connected: Boolean get() = live != null

    // --- domain accessors (throw until connected) ---

    /** The key-directory domain: publish this device's public keys, fetch peers'. */
    val keys: KeysDomain get() = requireConnected().keys

    /** Send and receive end-to-end encrypted messages. */
    val messaging: MessagingDomain get() = requireConnected().messaging

    /** List and create conversations. */
    val conversations: ConversationsDomain get() = requireConnected().conversations

    /** Fetch conversation history to catch up on missed messages. */
    val sync: SyncDomain get() = requireConnected().sync

    /** Publish and observe typing indicators. */
    val typing: TypingDomain get() = requireConnected().typing

    /** Publish and observe presence. */
    val presence: PresenceDomain get() = requireConnected().presence

    /** Browse, join, leave and observe rooms. */
    val rooms: RoomsDomain get() = requireConnected().rooms

    /** Look up public account profiles. */
    val profile: ProfileDomain get() = requireConnected().profile

    /** Receive server-pushed notification events. */
    val notifications: NotificationsDomain get() = requireConnected().notifications

    /** Submit game actions and observe game events. */
    val games: GamesDomain get() = requireConnected().games

    // --- application-facing streams (survive reconnects) ---

    /** Registers a handler for inbound decrypted messages. */
    fun onMessage(listener: Listener<IncomingMessage>): Subscription = messageListeners.add(listener)

    /** Registers a handler for messages withdrawn by their sender. */
    fun onDeletion(listener: Listener<MessageDeletion>): Subscription = deletionListeners.add(listener)

    /** Registers a handler for delivery and read receipts. */
    fun onReceipt(listener: Listener<MessageReceipt>): Subscription = receiptListeners.add(listener)

    /** Registers a handler for other participants' typing indicators. */
    fun onTyping(listener: Listener<TypingEvent>): Subscription = typingListeners.add(listener)

    /** Registers a handler for contacts' presence changes. */
    fun onPresence(listener: Listener<PresenceEvent>): Subscription = presenceListeners.add(listener)

    /** Registers a handler for members joining, leaving and changing role in a room. */
    fun onRoomMember(listener: Listener<RoomMemberEvent>): Subscription = memberListeners.add(listener)

    /** Registers a handler for a room's own state changes. */
    fun onRoomState(listener: Listener<RoomStateEvent>): Subscription = roomStateListeners.add(listener)

    /** Registers a handler for server-pushed notifications. */
    fun onNotification(listener: Listener<NotificationEvent>): Subscription =
        notificationListeners.add(listener)

    /** Registers a handler for authoritative game events. */
    fun onGameEvent(listener: Listener<GameEvent>): Subscription = gameListeners.add(listener)

    // --- bringing the client online ---

    /**
     * Registers a new account for this device, then connects.
     *
     * Returns the grant so the caller can persist it, alongside a [snapshot] of the key material, and
     * later [resume] without registering again.
     */
    suspend fun register(username: String, password: String): Grant {
        val grant = rest.register(username, password, deviceRequest(), options.locale)
        establish(grant)
        return grant
    }

    /**
     * Signs an existing account in on this device, then connects.
     *
     * [identifier] is a username, an email, or a public id: one field, because a user does not think of
     * those as different kinds of thing, and the server decides which it is.
     */
    suspend fun login(identifier: String, password: String): Grant {
        val grant = rest.login(identifier, password, deviceRequest())
        establish(grant)
        return grant
    }

    /**
     * Connects with a grant persisted from a previous run, skipping bootstrap.
     *
     * Pair it with a restored [KeyStore] (through [MigoClientOptions.keyStore]) so the device keeps its
     * identity across restarts. An expired access token makes the handshake fail; recover with
     * [refreshWith] and try again, or fall back to [login].
     */
    suspend fun resume(grant: Grant) {
        establish(grant)
    }

    /**
     * Exchanges a persisted refresh token for a fresh grant, without a live connection.
     *
     * The path back online for a device whose stored access token has expired: refresh, then [resume]
     * with what this returns.
     */
    suspend fun refreshWith(refreshToken: String, deviceId: Id): Grant =
        rest.refresh(refreshToken, deviceId)

    /**
     * Refreshes the access token on the live session and re-authenticates it.
     *
     * Returns the new grant, which the caller should persist. Call it proactively before the access
     * token expires; the session stays up throughout, which is the point of AUTHENTICATE existing as a
     * frame rather than sign-in being the only way to present a credential.
     */
    suspend fun refreshSession(): Grant = lifecycleLock.withLock {
        val session = requireConnected()
        val refreshed = rest.refresh(session.grant.refreshToken, session.deviceId)
        val authenticate = com.migo.core.protocol.Authenticate(
            accessToken = refreshed.accessToken,
            deviceId = parseId(refreshed.deviceId),
        )
        session.rpc.call(
            Op.AUTHENTICATE,
            { w -> authenticate.encode(w) },
            { r -> com.migo.core.protocol.Authenticated.decode(r) },
        )
        session.grant = refreshed
        refreshed
    }

    /**
     * Closes the session and tears down the per-connection graph. Idempotent.
     *
     * Crypto state lives in [keyStore] and the two crypto layers, not in the connection, so a later
     * [resume] continues the same identity and the same sessions. The membership and device caches are
     * cleared because they are only an optimisation and are rebuilt on demand.
     */
    suspend fun disconnect() {
        closing = true
        val job = lifecycleLock.withLock {
            val session = live ?: return@withLock null
            live = null
            session.stopAll()
            session.gateway.close()
            supervisor
        }
        supervisor = null
        job?.cancel()
        cacheLock.withLock {
            members.clear()
            userDevices.clear()
            bundleCache.clear()
            subscribedTopics.clear()
        }
        setState(ConnectionState.Closed)
        closing = false
    }

    /**
     * Disconnects and cancels the scope this client created for itself. Not reusable afterwards.
     *
     * Only for a client that was built without [MigoClientOptions.scope]. A caller-supplied scope is the
     * caller's to cancel, and cancelling somebody else's scope would take down whatever else runs in it.
     */
    suspend fun close() {
        disconnect()
        ownedScope?.cancel()
    }

    // --- persistence ---

    /**
     * The full private state for [com.migo.core.store.Vault.save] to seal.
     *
     * [username] is threaded in because the client never sees it on a [resume] path and the vault needs
     * it to show which account a device is signed in as before anything is unlocked. Nothing here
     * crosses the network; the refresh token in it is a credential and the vault is the only place it
     * belongs.
     */
    fun snapshot(username: String): DeviceKeys {
        val session = requireConnected()
        return keyStore.export(
            SavedSession(
                serverUrl = rest.base,
                accountId = session.accountId,
                deviceId = session.deviceId,
                username = username,
                refreshToken = session.grant.refreshToken,
            ),
        )
    }

    // --- topic subscription ---

    /**
     * Subscribes to a set of topics, so the gateway begins delivering their events.
     *
     * Tracked for re-subscription after a session reset. The response's rejected set means the
     * per-session subscription cap ([com.migo.core.protocol.Limits.maxSubscriptions]) was reached, which
     * is a client that has to unsubscribe from something before it can watch more.
     */
    suspend fun subscribe(topics: List<Topic>): SubscribeResponse {
        val session = requireConnected()
        val response = sendSubscribe(session, topics)
        cacheLock.withLock {
            for (topic in response.accepted) subscribedTopics[topicKey(topic)] = topic
        }
        return response
    }

    /** Unsubscribes from a set of topics and stops tracking them. */
    suspend fun unsubscribe(topics: List<Topic>): Acknowledged {
        val session = requireConnected()
        val request = SubscribeRequest(topics)
        val acknowledged = session.rpc.call(
            Op.UNSUBSCRIBE,
            { w -> request.encode(w) },
            { r -> Acknowledged.decode(r) },
        )
        cacheLock.withLock {
            for (topic in topics) subscribedTopics.remove(topicKey(topic))
        }
        return acknowledged
    }

    /** Subscribes to a conversation's topic: the prerequisite for receiving its messages. */
    suspend fun watchConversation(conversationId: Id) {
        subscribe(listOf(Topic(TopicKind.Conversation, conversationId)))
    }

    /** Stops receiving a conversation's events. */
    suspend fun unwatchConversation(conversationId: Id) {
        unsubscribe(listOf(Topic(TopicKind.Conversation, conversationId)))
    }

    /** Subscribes to a room's topic, for its membership and state events. */
    suspend fun watchRoom(roomId: Id) {
        subscribe(listOf(Topic(TopicKind.Room, roomId)))
    }

    /** Subscribes to an account's topic, for that account's presence changes. */
    suspend fun watchUser(userId: Id) {
        subscribe(listOf(Topic(TopicKind.User, userId)))
    }

    /** Subscribes to a game's topic, for its authoritative events. */
    suspend fun watchGame(gameId: Id) {
        subscribe(listOf(Topic(TopicKind.Game, gameId)))
    }

    // --- orchestration helpers ---

    /**
     * Creates a conversation, primes its membership, and subscribes to it in one step.
     *
     * The returned summary is ready to send to. Prefer this over [ConversationsDomain.create], which
     * does neither of the other two and leaves a conversation that seals for nobody and delivers
     * nothing back.
     */
    suspend fun startConversation(
        kind: ConversationKind,
        memberIds: List<Id>,
        title: String? = null,
    ): ConversationSummary {
        val summary = conversations.create(kind, memberIds, title)
        rememberConversation(summary)
        cacheLock.withLock {
            if (members[summary.conversationId] == null) {
                members[summary.conversationId] = memberIds.toList()
            }
        }
        watchConversation(summary.conversationId)
        return summary
    }

    /**
     * Lists conversations, priming each one's membership and subscribing to all of them.
     *
     * One SUBSCRIBE for the whole page rather than one per conversation: the topics are known together,
     * and a frame per conversation on every app start is the kind of thing that shows up on a phone's
     * battery graph. Page with the response's cursor.
     */
    suspend fun loadConversations(limit: Long, cursor: String? = null): ConversationListResponse {
        val response = conversations.list(limit, cursor)
        val topics = ArrayList<Topic>(response.conversations.size)
        for (summary in response.conversations) {
            rememberConversation(summary)
            topics.add(Topic(TopicKind.Conversation, summary.conversationId))
        }
        if (topics.isNotEmpty()) subscribe(topics)
        return response
    }

    /**
     * Fetches history for a conversation and replays it through the live decryption path.
     *
     * Each event is fed to [MessagingDomain.ingest] in the order the server returned it, so a historical
     * key exchange rebuilds the sender's chain before the content it unlocks. Messages already seen live
     * are dropped rather than delivered twice: both crypto layers refuse a second decrypt of the same
     * message, which is replay protection doing its job. Returns the raw response for its paging cursors
     * and its truncation status.
     */
    suspend fun catchUp(conversationId: Id, haveSeq: Long, limit: Long = 200L): SyncResponse {
        val session = requireConnected()
        val response = session.sync.fetch(conversationId, haveSeq, limit)
        for (event in response.messages) {
            session.messaging.ingest(event)
        }
        return response
    }

    // --- membership cache priming ---

    /** Caches a summary's membership, if it carries one, so sends need no extra round trip. */
    suspend fun rememberConversation(summary: ConversationSummary) {
        val listed = summary.members ?: return
        cacheLock.withLock { members[summary.conversationId] = listed }
    }

    /** Sets a conversation's membership explicitly, for a handle that arrived without one. */
    suspend fun rememberMembers(conversationId: Id, memberIds: List<Id>) {
        cacheLock.withLock { members[conversationId] = memberIds.toList() }
    }

    /**
     * Forgets a user's cached device list, so the next send re-enumerates it.
     *
     * Call it when a device is added or removed on that account -- a cached list is what would otherwise
     * leave a new device unable to read the conversation until the next rotation.
     */
    suspend fun invalidateDevices(userId: Id) {
        cacheLock.withLock {
            userDevices.remove(userId)
            val prefix = "${userId.value}|"
            bundleCache.keys.filter { it.startsWith(prefix) }.forEach { bundleCache.remove(it) }
        }
    }

    /** Forgets a conversation's cached membership, so the next send re-reads it. */
    suspend fun invalidateConversation(conversationId: Id) {
        cacheLock.withLock { members.remove(conversationId) }
    }

    // --- DeviceDirectory ---

    /**
     * The devices a sender key must reach for a conversation, excluding this sending device.
     *
     * The audience is every member's devices unioned with this account's own -- so our other devices
     * sync -- minus the one we send from. Membership must have been primed by [startConversation],
     * [loadConversations] or [rememberMembers]; an unknown conversation throws rather than quietly
     * sealing for nobody, because a send that reached no one and reported success is the worst of the
     * available outcomes.
     */
    override suspend fun recipientDevices(conversationId: Id): List<DeviceAddress> {
        val session = requireConnected()
        val audience = cacheLock.withLock {
            val listed = members[conversationId] ?: throw SdkError(
                "membership for conversation $conversationId is unknown; call startConversation, " +
                    "loadConversations, or rememberMembers first",
            )
            LinkedHashSet(listed).apply { add(session.accountId) }
        }

        val devices = ArrayList<DeviceAddress>()
        for (userId in audience) {
            for (device in devicesFor(userId, session.keys)) {
                // Exclude only this sending device; our other devices belong in the audience for sync.
                if (device == session.deviceId) continue
                devices.add(DeviceAddress(userId, device))
            }
        }
        return devices
    }

    // --- PeerBundleSource ---

    /**
     * Fetches one device's key bundle for the 1:1 layer to run X3DH.
     *
     * Serves a bundle already enumerated by [recipientDevices] exactly once -- spending one of the
     * peer's one-time prekeys rather than a second one -- then falls through to a direct fetch for a
     * device never enumerated. The bundle is not verified here; [SessionCrypto] verifies it before any
     * key agreement, which is the single place verification must live.
     */
    override suspend fun fetchBundle(userId: Id, deviceId: Id): PrekeyBundle {
        val session = requireConnected()
        val key = bundleKey(userId, deviceId)
        val cached = cacheLock.withLock { bundleCache.remove(key) }
        if (cached != null) return cached
        return session.keys.fetchBundle(userId, deviceId)
    }

    // --- key material maintenance ---

    /** Publishes this device's current public key material. */
    suspend fun publishKeys() {
        requireConnected().keys.publish()
    }

    /**
     * Tops up the one-time prekey pool if it has run low, and republishes. Returns whether it did.
     *
     * Fetching a bundle consumes one of this device's prekeys server-side, so a device that receives
     * many first messages drains the pool; an empty pool does not break a session but it does drop the
     * one-time key's contribution to forward secrecy for every session formed while it is empty. Safe to
     * call after any inbound key exchange.
     */
    suspend fun replenishPrekeys(): Boolean {
        val session = requireConnected()
        if (keyStore.oneTimePrekeyCount() > options.replenishPolicy.low) return false
        keyStore.replenishOneTimePrekeys(options.replenishPolicy.batch)
        session.keys.publish()
        return true
    }

    /**
     * Rotates the signed prekey and republishes.
     *
     * The server expires a signed prekey after thirty days (`SIGNED_PREKEY_LIFETIME_MS`) and refuses to
     * publish one already expired, so a long-lived install has to do this. A client with no other cue
     * can call it at sign-in and let the id sequence take care of the rest.
     */
    suspend fun rotateSignedPrekey() {
        val session = requireConnected()
        keyStore.rotateSignedPrekey()
        session.keys.publish()
    }

    // --- internals ---

    /** Builds the graph, connects, publishes keys, subscribes, and starts the reconnect supervisor. */
    private suspend fun establish(grant: Grant) {
        lifecycleLock.withLock {
            if (live != null) {
                throw SdkError("already connected; call disconnect before connecting again")
            }
            closing = false
            setState(ConnectionState.Connecting)
            val session = openSession(grant, resume = null)
            live = session
            setState(ConnectionState.Online)

            session.keys.publish()
            // This account's own user topic carries self-directed events: presence sync across our
            // devices, and notifications.
            val topic = Topic(TopicKind.User, session.accountId)
            sendSubscribe(session, listOf(topic))
            cacheLock.withLock { subscribedTopics[topicKey(topic)] = topic }

            supervisor = scope.launch { supervise() }
        }
    }

    /**
     * Opens one socket and builds the per-connection graph over it.
     *
     * Inbound handlers are registered before the pump can run, so nothing pushed to us in the first
     * milliseconds is dropped for want of a subscriber.
     */
    private suspend fun openSession(grant: Grant, resume: ResumeRequest?): Session {
        val accountId = parseId(grant.accountId)
        val deviceId = parseId(grant.deviceId)
        val hello = Hello(
            protocolVersion = PROTOCOL_VERSION.toLong(),
            client = ClientInfo(
                platform = Platform.Android,
                appVersion = options.appVersion,
                osVersion = options.osVersion,
                deviceModel = options.deviceModel,
            ),
            features = options.features,
            locale = options.locale,
            bandwidthMode = options.bandwidthMode,
            accessToken = grant.accessToken,
            deviceId = deviceId,
            resume = resume,
        )
        val (gateway, welcome) = Gateway.connect(gatewayUrl, socketClient, scope, hello)
        val rpc = Rpc(gateway, options.onEventError)
        val session = Session(
            grant = grant,
            accountId = accountId,
            deviceId = deviceId,
            gateway = gateway,
            rpc = rpc,
            resumed = welcome.resumed == true,
            keys = KeysDomain(rpc, keyStore),
            messaging = MessagingDomain(
                rpc,
                scope,
                sessionCrypto,
                groupCrypto,
                this,
                options.onEventError,
            ),
            conversations = ConversationsDomain(rpc),
            sync = SyncDomain(rpc),
            typing = TypingDomain(rpc, options.onEventError),
            presence = PresenceDomain(rpc, options.onEventError),
            rooms = RoomsDomain(rpc, options.onEventError),
            profile = ProfileDomain(rpc),
            notifications = NotificationsDomain(rpc, options.onEventError),
            games = GamesDomain(rpc, options.onEventError),
        )
        session.startAll()
        bridge(session)
        return session
    }

    /**
     * Wires the freshly built domains into this client's own listener sets.
     *
     * The bridge subscriptions are not tracked, and do not need to be: they live on the discarded
     * [Rpc]'s subscriber table, which goes away with the session object.
     */
    private fun bridge(session: Session) {
        session.messaging.onMessage { messageListeners.deliver(it) }
        session.messaging.onDeletion { deletionListeners.deliver(it) }
        session.messaging.onReceipt { receiptListeners.deliver(it) }
        session.typing.onTyping { typingListeners.deliver(it) }
        session.presence.onPresence { presenceListeners.deliver(it) }
        session.rooms.onMember { memberListeners.deliver(it) }
        session.rooms.onState { roomStateListeners.deliver(it) }
        session.notifications.onNotification { notificationListeners.deliver(it) }
        session.games.onEvent { gameListeners.deliver(it) }
    }

    /**
     * Pumps the connection, and reconnects for as long as the client is meant to be online.
     *
     * [Rpc.deliver] returning normally means the socket closed cleanly; it throwing means the connection
     * died, and either way this connection is over. The distinction does not change what happens next --
     * a clean close we did not ask for still leaves an application expecting to be online.
     */
    private suspend fun supervise() {
        while (true) {
            val session = live ?: return
            try {
                session.rpc.deliver()
            } catch (cause: CancellationException) {
                throw cause
            } catch (cause: Throwable) {
                options.onConnectionError?.invoke(cause)
            }
            if (closing) return
            setState(ConnectionState.Reconnecting)
            if (!reconnect(session)) return
            setState(ConnectionState.Online)
        }
    }

    /** Retries until a session is established, or the client is asked to stop. */
    private suspend fun reconnect(previous: Session): Boolean {
        while (!closing) {
            delay(backoff.next())
            if (closing) return false
            try {
                reopen(previous)
                backoff.reset()
                return true
            } catch (cause: CancellationException) {
                throw cause
            } catch (cause: Throwable) {
                options.onConnectionError?.invoke(cause)
            }
        }
        return false
    }

    /**
     * Opens a replacement session, resuming the old one where the server allows it.
     *
     * A resume that succeeded means the server replayed from the acknowledged watermark and the
     * subscriptions are still in place, so there is nothing to redo. A fresh session means they are
     * gone, and every tracked topic is re-sent before the application's own resync runs -- in that
     * order, because a resync that fetched history before the subscriptions were back would leave a gap
     * between the last fetched message and the first delivered one.
     */
    private suspend fun reopen(previous: Session) {
        val resume = if (previous.gateway.sessionId == NIL_ID) {
            null
        } else {
            ResumeRequest(previous.gateway.sessionId, previous.gateway.lastFrameSeq)
        }
        previous.stopAll()
        previous.gateway.close()

        val session = lifecycleLock.withLock {
            if (closing) return
            val opened = openSession(previous.grant, resume)
            live = opened
            opened
        }

        if (session.resumed) return

        val topics = cacheLock.withLock { subscribedTopics.values.toList() }
        if (topics.isNotEmpty()) {
            try {
                sendSubscribe(session, topics)
            } catch (cause: CancellationException) {
                throw cause
            } catch (cause: Throwable) {
                // Reported, not rethrown: the connection is up, and a client that tore it down over a
                // failed re-subscribe would lose the requests it could still serve.
                options.onEventError?.invoke(Op.SUBSCRIBE, cause)
            }
        }
        options.onReset?.invoke()
    }

    /** The SUBSCRIBE round trip, without the tracking, for the paths that track separately. */
    private suspend fun sendSubscribe(session: Session, topics: List<Topic>): SubscribeResponse {
        val request = SubscribeRequest(topics)
        return session.rpc.call(
            Op.SUBSCRIBE,
            { w -> request.encode(w) },
            { r -> SubscribeResponse.decode(r) },
        )
    }

    /** The device list for a user, from cache or one enumeration that also warms the bundle cache. */
    private suspend fun devicesFor(userId: Id, keys: KeysDomain): List<Id> {
        cacheLock.withLock { userDevices[userId] }?.let { return it }

        val bundles = keys.fetchDeviceBundles(userId)
        val deviceIds = ArrayList<Id>(bundles.size)
        cacheLock.withLock {
            for (entry in bundles) {
                deviceIds.add(entry.deviceId)
                bundleCache[bundleKey(userId, entry.deviceId)] = entry.bundle
            }
            userDevices[userId] = deviceIds
        }
        return deviceIds
    }

    /** The device descriptor sent to bootstrap. */
    private fun deviceRequest(): DeviceRequest = DeviceRequest.describe(
        deviceId = options.deviceId,
        appVersion = options.appVersion,
        osVersion = options.osVersion,
        deviceModel = options.deviceModel,
    )

    private fun setState(next: ConnectionState) {
        if (state == next) return
        state = next
        options.onStateChange?.invoke(next)
    }

    private fun requireConnected(): Session =
        live ?: throw SdkError("not connected; call register, login, or resume first")

    companion object {
        /** Builds a client. Nothing touches the network until [register], [login] or [resume]. */
        fun create(options: MigoClientOptions): MigoClient {
            val supplied = options.scope
            val owned = if (supplied == null) {
                CoroutineScope(SupervisorJob() + Dispatchers.IO)
            } else {
                null
            }
            return MigoClient(options, owned, supplied ?: owned!!)
        }
    }
}

/**
 * The live object graph for one connection.
 *
 * Held together so the whole set appears and disappears at once. Everything in it is bound to one
 * [Gateway]; the pieces that must outlive a connection -- the key store and the two crypto layers --
 * live on [MigoClient] instead.
 */
private class Session(
    var grant: Grant,
    val accountId: Id,
    val deviceId: Id,
    val gateway: Gateway,
    val rpc: Rpc,
    /** Whether WELCOME said the previous session was resumed, so subscriptions are still in place. */
    val resumed: Boolean,
    val keys: KeysDomain,
    val messaging: MessagingDomain,
    val conversations: ConversationsDomain,
    val sync: SyncDomain,
    val typing: TypingDomain,
    val presence: PresenceDomain,
    val rooms: RoomsDomain,
    val profile: ProfileDomain,
    val notifications: NotificationsDomain,
    val games: GamesDomain,
) {
    /** Registers every inbound handler. Called before the pump starts. */
    fun startAll() {
        messaging.start()
        typing.start()
        presence.start()
        rooms.start()
        notifications.start()
        games.start()
    }

    /** Unregisters them. The stateless domains have nothing to stop. */
    fun stopAll() {
        messaging.stop()
        typing.stop()
        presence.stop()
        rooms.stop()
        notifications.stop()
        games.stop()
    }
}

/**
 * Exponential backoff with jitter, for the reconnect loop.
 *
 * The jitter is not decoration. A node restarting disconnects every session it held at the same
 * instant, and a fixed schedule would bring all of them back in one synchronised wave -- which is the
 * load that keeps the node down. Spreading each client's retry across the interval is what turns a
 * thundering herd into an arrival rate.
 */
private class Backoff(private val ceilingMs: Long) {
    private var current = INITIAL_MS

    /** The next delay, and advances the schedule. */
    fun next(): Long {
        val base = current
        current = (current * 2).coerceAtMost(ceilingMs.coerceAtLeast(INITIAL_MS))
        return base + Random.nextLong(base / 2 + 1)
    }

    /** Back to the start, after a connection succeeded. */
    fun reset() {
        current = INITIAL_MS
    }

    private companion object {
        /** Short enough that a momentary radio glitch is invisible to the user. */
        const val INITIAL_MS = 500L
    }
}

/** The tracking key for a subscribed topic. */
private fun topicKey(topic: Topic): String = "${topic.kind.wire}:${topic.id.value}"

/** The cache key for one device's bundle. */
private fun bundleKey(userId: Id, deviceId: Id): String = "${userId.value}|${deviceId.value}"
