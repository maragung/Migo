package com.migo.core.net

import com.migo.core.protocol.Ack
import com.migo.core.protocol.DeliveryClass
import com.migo.core.protocol.Error as ProtocolError
import com.migo.core.protocol.Feature
import com.migo.core.protocol.Hello
import com.migo.core.protocol.OPCODES
import com.migo.core.protocol.Op
import com.migo.core.protocol.Ping
import com.migo.core.protocol.Welcome
import com.migo.core.protocol.opcodeName
import com.migo.core.wire.Compress
import com.migo.core.wire.Flags
import com.migo.core.wire.Frame
import com.migo.core.wire.Id
import com.migo.core.wire.Limits
import com.migo.core.wire.NIL_ID
import com.migo.core.wire.Reader
import com.migo.core.wire.Writer
import com.migo.core.wire.decodeFrame
import com.migo.core.wire.encodeFrame
import com.migo.core.wire.frameHeader
import com.migo.core.wire.unpackFrame
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger
import kotlinx.coroutines.CancellableContinuation
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.suspendCancellableCoroutine
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.Response
import okhttp3.WebSocket
import okhttp3.WebSocketListener
import okio.ByteString

/**
 * The gateway connection: one WebSocket, one MWP frame per binary message.
 *
 * Mirrors `clients/desktop/src/net/gateway.rs`. The two clients differ only in what their language
 * gives them for concurrency; the bytes on the socket and the state machine are the same.
 *
 * # Framing
 *
 * MWP frames are self-describing and a WebSocket message already has a length, so a frame is sent as
 * exactly one binary message with no length prefix. The length-prefixed form exists for byte streams
 * (the mesh links between nodes run over one); using it here would be four redundant bytes on every
 * message and, worse, would desynchronise a peer that reads one frame per message -- which is what
 * the server does at `migo-gateway/src/connection.rs`.
 *
 * # The handshake
 *
 * HELLO carries the access token, so a successful connection is authenticated by the time WELCOME
 * arrives -- one round trip rather than two. WELCOME reports the negotiated feature bits, which may
 * be fewer than were asked for; the client honours the intersection rather than assuming it got what
 * it requested. AUTHENTICATE exists for re-authenticating a live connection after a token refresh,
 * which is why it is still sent as a separate frame in that one case.
 *
 * # Text messages
 *
 * Rejected. A text frame on this socket means either a proxy rewriting traffic or a peer speaking
 * something that is not MWP, and brief section 178 forbids a JSON realtime path outright; accepting
 * one "just in case" is how a second, undocumented protocol gets born.
 *
 * # The heartbeat is an MWP PING, not a WebSocket ping
 *
 * This is the one thing about this class that is easy to get wrong and expensive to discover. The
 * server refreshes its liveness clock only on an *inbound MWP frame*, and it closes a session that
 * has been quiet for twice the heartbeat it advertised in WELCOME. OkHttp's own `pingInterval` sends
 * WebSocket control frames, which keep NAT bindings and proxies happy but never reach the server's
 * frame loop -- a client relying on them alone gets disconnected every minute on an idle screen and
 * looks like a network problem. So [Gateway] sends a real `PING` opcode on a timer derived from the
 * server's own advertised interval, and the WebSocket-level ping is left on as well because the two
 * solve different problems.
 */

/** A gateway failure. */
sealed class GatewayError(message: String) : Exception(message) {
    /** The socket never opened, or it failed in a way that was not a clean close. */
    object Transport : GatewayError("cannot reach the gateway")

    /** The peer closed, or this client did. */
    object Closed : GatewayError("the connection closed")

    /**
     * The server sent something this client could not parse.
     *
     * Not attributed further: the bytes came from the network and are not going in a log line
     * (brief section 174).
     */
    object Malformed : GatewayError("the gateway sent a frame this client could not read")

    /**
     * The server refused, with its own stable error code.
     *
     * [symbol] is what a UI localises from -- it is the stable machine identifier, and the schema is
     * explicit that [detail] is developer-facing and not to be shown verbatim. [detail] is here
     * because a bug report with it is worth ten without it, and it is empty for a server-fault error:
     * the server puts only its public face on the wire (brief section 161), so a client learns that
     * something failed and nothing about where.
     */
    class Refused(
        val code: Long,
        val symbol: String,
        val detail: String,
        val retryAfterMs: Long?,
    ) : GatewayError(if (detail.isEmpty()) symbol else detail)

    /** The socket opened but the server never completed the handshake. */
    object NoWelcome : GatewayError("the gateway did not complete the handshake")
}

/**
 * A frame the caller has not asked for: a broadcast, or a reply whose correlation nobody is waiting
 * on any more.
 *
 * Delivered on [Gateway.inbound]. The payload is already decompressed and batches are already
 * unpacked, so a consumer sees one logical frame per item and never has to know that the server
 * coalesced three of them into one WebSocket message.
 */
class Inbound(val frame: Frame)

/**
 * A live gateway connection.
 *
 * Construct it with [connect], which returns the connection together with its [Welcome] -- the
 * negotiated features and the server's frame-size limit in there govern everything sent afterwards,
 * so handing back a connection without it would invite a caller to send before it knew the rules.
 *
 * One instance is one socket. A dropped connection is not repaired in place: [sessionId] and
 * [lastFrameSeq] are what a caller needs to build the [com.migo.core.protocol.ResumeRequest] for the
 * next one, and reconnection policy belongs to the client above this layer, which is the only thing
 * that knows whether the user is still looking at the screen.
 */
class Gateway private constructor(
    private val socket: WebSocket,
    private val scope: CoroutineScope,
    private val state: SocketState,
) : RealtimeTransport {
    /** Correlation ids for request frames. Zero means "not a reply to anything", so ids start at 1. */
    private val nextCorrelation = AtomicInteger(1)

    /** Serialises writes: two coroutines encoding into the same socket must not interleave frames. */
    private val writeLock = Mutex()

    private var heartbeat: Job? = null

    /** Frames nobody correlated: broadcasts, and replies whose waiter has gone. */
    override val inbound: Channel<Inbound> get() = state.inbound

    /** The session id from WELCOME, which a resume attempt must name. */
    override val sessionId: Id get() = state.sessionId

    /**
     * The highest `frame_seq` this client has seen.
     *
     * Sequence numbers are not on the wire: the server assigns them implicitly, starting at 1, to
     * every Critical frame it puts in this session's mailbox, and both ends count independently
     * (brief sections 141 and 152). So this is a count, and the count must include *every* Critical
     * frame -- a correlated PONG, AUTHENTICATED or SUBSCRIBE_RESPONSE is Critical and sequenced
     * exactly like a broadcast. A client that counted only broadcasts would drift low by one per
     * request, and its ACK would then retire the wrong frames from the server's resume ring: not a
     * visible bug until a reconnect replays messages the user already read.
     *
     * Only WELCOME and RECONNECT_HINT bypass the mailbox and carry no sequence.
     */
    override val lastFrameSeq get() = state.lastFrameSeq

    /** A fresh correlation id, for a request whose reply must be matched to it. */
    override fun correlate(): Long {
        // Wrapping is fine and deliberate: correlation ids only have to be unique among the requests
        // currently in flight, and four billion is far beyond that. Growing to a wider type, or
        // throwing on overflow, would both be worse answers to a problem that does not exist.
        val id = nextCorrelation.getAndUpdate { if (it == Int.MAX_VALUE) 1 else it + 1 }
        return id.toLong() and 0xFFFFFFFFL
    }

    /**
     * Encodes and sends one message as one binary WebSocket message.
     *
     * [encode] is the generated `encode(Writer)` of whatever struct the opcode carries, passed as a
     * function rather than behind an interface because the generated types share no supertype -- and
     * giving them one would mean editing generated code by hand.
     *
     * Compression is attempted only above [Limits.COMPRESS_MIN_BYTES] and kept only if it actually
     * won, which is what [maybeDeflate] decides; a compressed frame that grew is a frame that cost
     * CPU on both ends to waste bytes.
     */
    override suspend fun send(opcode: Long, correlation: Long, encode: (Writer) -> Unit) {
        val writer = Writer()
        encode(writer)
        val payload = writer.finish()

        val deflated = if (state.compression) Compress.maybeDeflate(payload) else null
        val flags = if (deflated != null) Flags.COMPRESSED else 0
        val header = frameHeader(opcode, correlation).copy(flags = flags)
        val bytes = encodeFrame(Frame(header, deflated ?: payload))
        if (bytes.size > Limits.MAX_FRAME_BYTES) throw GatewayError.Malformed

        writeLock.withLock {
            if (!socket.send(ByteString.of(*bytes))) {
                // OkHttp refuses a send when the socket is closing or its outgoing buffer is full.
                // Either way this frame did not go, and pretending otherwise would leave the caller
                // waiting for a reply to a request the server never saw.
                throw GatewayError.Transport
            }
        }
    }

    /**
     * Sends a request and suspends until the reply carrying the same correlation arrives.
     *
     * The waiter is registered *before* the frame goes out. Registering afterwards is a race the
     * fast path loses: a local server can answer before the sending coroutine is scheduled again, and
     * the reply would arrive with no waiter and be delivered to [inbound] instead, leaving this call
     * to hang until its timeout.
     *
     * An ERROR-flagged reply becomes [GatewayError.Refused] here rather than being handed back as a
     * frame, because every caller would otherwise have to remember to check the flag, and the one
     * that forgot would read an error body as a success struct.
     */
    override suspend fun request(opcode: Long, encode: (Writer) -> Unit): Frame {
        val correlation = correlate()
        val waiter = state.register(correlation)
        try {
            send(opcode, correlation, encode)
            val frame = waiter.await()
            if (isError(frame)) throw refusal(frame)
            return frame
        } finally {
            state.forget(correlation)
        }
    }

    /**
     * Acknowledges every Critical frame seen so far.
     *
     * Cumulative, so one ACK retires a whole run from the server's resume ring; sending one per frame
     * would be a frame per frame in the other direction for no extra information. Called from the
     * heartbeat, which means an idle client still tells the server what it has, and the server's ring
     * does not hold frames the client read minutes ago.
     */
    override suspend fun acknowledge() {
        val seq = state.lastFrameSeq
        if (seq == 0L || seq == state.lastAcked) return
        state.lastAcked = seq
        send(Op.ACK, 0L) { w -> Ack(seq).encode(w) }
    }

    /**
     * Starts the MWP heartbeat.
     *
     * The interval comes from WELCOME rather than from a constant here, because the server is what
     * decides when a session is dead and only it knows the deadline it will enforce. Halving it is
     * deliberate: the deadline is two intervals, so one lost ping still leaves a second one inside
     * the window, and a phone whose radio slept through a tick is not disconnected for it.
     */
    internal fun startHeartbeat(intervalMs: Long) {
        val period = (intervalMs / 2).coerceAtLeast(MIN_HEARTBEAT_MS)
        heartbeat = scope.launch {
            while (isActive) {
                delay(period)
                try {
                    acknowledge()
                    // The device's own clock, not an estimate of the server's: PONG echoes this field
                    // back unchanged so the round trip can be measured without either end keeping
                    // state, and an "improved" value would be measured against a clock that never
                    // sent it.
                    send(Op.PING, correlate()) { w -> Ping(System.currentTimeMillis()).encode(w) }
                } catch (_: GatewayError) {
                    // The read side is what reports a dead connection; a failed write here means it
                    // is already reporting one. Looping on it would just spin.
                    return@launch
                }
            }
        }
    }

    /** Closes the socket politely, so the server retires the session rather than timing it out. */
    override fun close() {
        heartbeat?.cancel()
        socket.close(NORMAL_CLOSURE, null)
        state.fail(GatewayError.Closed)
    }

    companion object {
        /** WebSocket close code for a clean, intentional close. */
        private const val NORMAL_CLOSURE = 1000

        /** A floor on the heartbeat period, so a nonsense advertised interval cannot busy-loop. */
        private const val MIN_HEARTBEAT_MS = 5_000L

        /**
         * Connects, sends HELLO, and waits for WELCOME.
         *
         * [scope] owns the heartbeat and must outlive the connection; a scope tied to a screen would
         * cancel the heartbeat when the user navigated away and the session would then die on the
         * server's liveness deadline.
         */
        suspend fun connect(
            url: String,
            client: OkHttpClient,
            scope: CoroutineScope,
            hello: Hello,
        ): Pair<Gateway, Welcome> {
            val state = SocketState()
            val listener = GatewayListener(state)
            val request = Request.Builder().url(url).build()
            val socket = client.newWebSocket(request, listener)
            val gateway = Gateway(socket, scope, state)

            try {
                // The server answers HELLO with exactly one frame, and both answers carry the HELLO
                // opcode: a WELCOME, or the same opcode with the ERROR flag set. The flag is
                // therefore the discriminator, not the opcode -- branching on the opcode alone would
                // try to read an error body as a WELCOME. Anything else is a protocol violation and
                // there is nothing useful to do but give up; a client that skipped unexpected frames
                // here would proceed on a connection it had never negotiated.
                val frame = gateway.request(Op.HELLO) { w -> hello.encode(w) }
                if (frame.header.opcode != Op.HELLO) throw GatewayError.NoWelcome

                val welcome = try {
                    Welcome.decode(Reader(frame.payload))
                } catch (_: Exception) {
                    throw GatewayError.Malformed
                }
                state.adopt(welcome)
                gateway.startHeartbeat(welcome.limits.heartbeatMs)
                return gateway to welcome
            } catch (error: Throwable) {
                socket.cancel()
                state.fail(GatewayError.Closed)
                throw error
            }
        }

        /** True when a frame carries the ERROR flag, whatever its opcode. */
        fun isError(frame: Frame): Boolean = (frame.header.flags and Flags.ERROR) != 0

        /**
         * Turns an ERROR frame into a [GatewayError.Refused].
         *
         * A payload that will not even parse still has to become an error, so the fallback names the
         * opcode from the header rather than reporting success.
         */
        fun refusal(frame: Frame): GatewayError {
            val parsed = try {
                ProtocolError.decode(Reader(frame.payload))
            } catch (_: Exception) {
                null
            }
            return if (parsed != null) {
                GatewayError.Refused(
                    parsed.code,
                    parsed.symbol,
                    parsed.message.orEmpty(),
                    parsed.retryAfterMs,
                )
            } else {
                GatewayError.Refused(0L, opcodeName(frame.header.opcode), "", null)
            }
        }

        /**
         * Builds the WebSocket client for a gateway.
         *
         * `pingInterval` is set even though the MWP heartbeat above is what keeps the *session*
         * alive: control frames are what keep the *path* alive, through NATs and proxies that drop an
         * idle TCP connection without telling either end. `readTimeout` is zero because a socket
         * that is quiet is normal here -- the liveness question is answered by the heartbeat, not by
         * a read deadline.
         */
        fun httpClient(): OkHttpClient = OkHttpClient.Builder()
            .pingInterval(20, TimeUnit.SECONDS)
            .readTimeout(0, TimeUnit.MILLISECONDS)
            .connectTimeout(10, TimeUnit.SECONDS)
            .build()
    }
}

/**
 * Everything the OkHttp listener thread and the coroutine side share.
 *
 * OkHttp delivers callbacks on its own thread, so the reply table and the sequence counter are held
 * under one lock rather than in atomics: the count and the routing decision for a frame have to be
 * one step, and two independent atomics would let a reader see a count that did not yet include the
 * frame it was handed. The alternative -- posting each frame onto a coroutine and doing the
 * bookkeeping there -- would let the sequence count fall behind the frames that produced it, and the
 * ACK watermark has to be exact.
 */
private class SocketState {
    /** Frames with no waiter. Unlimited: dropping a broadcast is dropping a message. */
    val inbound = Channel<Inbound>(Channel.UNLIMITED)

    private val lock = Any()
    private val slots = HashMap<Long, Slot>()
    private var failure: GatewayError? = null

    @Volatile var sessionId = NIL_ID
    @Volatile var compression = false
    @Volatile var lastFrameSeq = 0L
    @Volatile var lastAcked = 0L

    /**
     * Records what WELCOME settled, before the first ordinary frame can be read.
     *
     * Compression is switched on from the *negotiated* features, never from what HELLO asked for. The
     * session uses the intersection, so a client that deflated on the strength of its own request
     * would be sending frames a server without the feature will not inflate.
     */
    fun adopt(welcome: Welcome) {
        sessionId = welcome.sessionId
        compression = (welcome.features and Feature.COMPRESSION) != 0uL &&
            Compress.isCompressionAvailable()
        // A resumed session continues the server's sequence, and the client must continue counting
        // from where the server says it stopped rather than from zero.
        welcome.resumeFromSeq?.let { lastFrameSeq = it; lastAcked = it }
    }

    /**
     * Claims a correlation id, before the request that uses it is written.
     *
     * The slot exists from this moment, which is the whole point: a reply that lands while the sending
     * coroutine has not been scheduled again finds a slot to sit in rather than falling through to
     * [inbound], where the caller would never see it and would wait out its timeout instead.
     */
    fun register(correlation: Long): Waiter {
        val slot = Slot()
        synchronized(lock) {
            failure?.let { slot.failure = it }
            slots[correlation] = slot
        }
        return Waiter(this, correlation, slot)
    }

    fun forget(correlation: Long) {
        synchronized(lock) { slots.remove(correlation) }
    }

    /** Parks a caller on its slot, or hands it what already arrived. */
    fun park(slot: Slot, continuation: CancellableContinuation<Frame>) {
        synchronized(lock) {
            val arrived = slot.frame
            val dead = slot.failure
            when {
                arrived != null -> continuation.resumeWith(Result.success(arrived))
                dead != null -> continuation.resumeWith(Result.failure(dead))
                else -> {
                    slot.continuation = continuation
                    return
                }
            }
        }
    }

    /**
     * Routes one decoded frame.
     *
     * The sequence count is incremented for every Critical frame, including correlated replies, and
     * under the same lock that decides where the frame goes -- see [Gateway.lastFrameSeq] for why
     * undercounting here is a bug that only shows up after a reconnect.
     */
    fun deliver(frame: Frame) {
        val waiting = synchronized(lock) {
            if (isSequenced(frame.header.opcode)) lastFrameSeq += 1
            val correlation = frame.header.correlation
            if (correlation == 0L) {
                null
            } else {
                val slot = slots.remove(correlation)
                if (slot == null) {
                    null
                } else {
                    val continuation = slot.continuation
                    if (continuation == null) {
                        // The caller has claimed the id but has not parked yet. Leave the frame where
                        // it will find it, and put the slot back so the lookup succeeds.
                        slot.frame = frame
                        slots[correlation] = slot
                        return
                    }
                    slot.continuation = null
                    continuation
                }
            }
        }
        if (waiting != null) {
            if (waiting.isActive) {
                waiting.resumeWith(Result.success(frame))
            } else {
                // Cancelled between parking and now: nobody is going to read this, and a broadcast
                // channel is the wrong place for somebody else's reply.
                return
            }
            return
        }
        inbound.trySend(Inbound(frame))
    }

    /** Wakes everyone with the same failure and closes the stream. */
    fun fail(error: GatewayError) {
        val parked = synchronized(lock) {
            if (failure == null) failure = error
            val waiting = ArrayList<CancellableContinuation<Frame>>(slots.size)
            for (slot in slots.values) {
                val continuation = slot.continuation
                if (continuation != null) {
                    slot.continuation = null
                    waiting.add(continuation)
                } else {
                    slot.failure = error
                }
            }
            waiting
        }
        for (continuation in parked) {
            if (continuation.isActive) continuation.resumeWith(Result.failure(error))
        }
        inbound.close(error)
    }

    /**
     * Whether an opcode's frames are counted toward the ACK watermark.
     *
     * The server assigns a sequence to every Critical frame it pushes into the session mailbox. Two
     * frames never go through that mailbox: WELCOME, written straight to the socket during the
     * handshake, and RECONNECT_HINT, which the server sends as it is tearing the session down and
     * which therefore cannot be part of a sequence the client would ever acknowledge. Everything else
     * the server sends -- correlated replies included -- is counted.
     *
     * Coalescable and Droppable frames carry no sequence either. Their classes come from the generated
     * table rather than a list written out here, so a new opcode cannot be silently miscounted.
     */
    private fun isSequenced(opcode: Long): Boolean {
        if (opcode == Op.HELLO || opcode == Op.RECONNECT_HINT) return false
        val meta = OPCODES[opcode] ?: return false
        return meta.cls == DeliveryClass.Critical
    }
}

/** One outstanding reply: whichever of the caller and the frame arrives first waits for the other. */
private class Slot {
    var continuation: CancellableContinuation<Frame>? = null
    var frame: Frame? = null
    var failure: GatewayError? = null
}

/** A claimed correlation id, awaitable exactly once. */
private class Waiter(
    private val state: SocketState,
    private val correlation: Long,
    private val slot: Slot,
) {
    suspend fun await(): Frame = suspendCancellableCoroutine { continuation ->
        continuation.invokeOnCancellation { state.forget(correlation) }
        state.park(slot, continuation)
    }
}

/**
 * The OkHttp callback surface, kept to translation only.
 *
 * Everything it receives is either turned into a decoded frame or into a failure; no policy lives
 * here, because this runs on OkHttp's thread and policy that blocks it stalls the socket.
 */
private class GatewayListener(private val state: SocketState) : WebSocketListener() {
    override fun onMessage(webSocket: WebSocket, bytes: ByteString) {
        val frames = try {
            val frame = decodeFrame(bytes.toByteArray())
            val inflated = if ((frame.header.flags and Flags.COMPRESSED) != 0) {
                Frame(frame.header, Compress.inflateRaw(frame.payload))
            } else {
                frame
            }
            // A batch is a transport optimisation, not a message: unpacking it here means nothing
            // above this line has to know the server ever grouped frames.
            if ((inflated.header.flags and Flags.BATCH) != 0) unpackFrame(inflated) else listOf(inflated)
        } catch (_: Exception) {
            state.fail(GatewayError.Malformed)
            webSocket.cancel()
            return
        }
        for (frame in frames) {
            state.deliver(frame)
        }
    }

    /** See the class note on text messages: this is not MWP, and it is not tolerated. */
    override fun onMessage(webSocket: WebSocket, text: String) {
        state.fail(GatewayError.Malformed)
        webSocket.cancel()
    }

    override fun onClosing(webSocket: WebSocket, code: Int, reason: String) {
        webSocket.close(code, null)
        state.fail(GatewayError.Closed)
    }

    override fun onClosed(webSocket: WebSocket, code: Int, reason: String) {
        state.fail(GatewayError.Closed)
    }

    /**
     * A failure at any layer below MWP.
     *
     * The throwable is not passed on. It names hosts, ports and TLS internals, and the caller's only
     * decision is whether to retry -- which [GatewayError.Transport] already answers.
     */
    override fun onFailure(webSocket: WebSocket, t: Throwable, response: Response?) {
        state.fail(GatewayError.Transport)
    }
}
