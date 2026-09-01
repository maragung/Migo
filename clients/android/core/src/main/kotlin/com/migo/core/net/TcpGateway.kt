package com.migo.core.net

import com.migo.core.protocol.Ack
import com.migo.core.protocol.DeliveryClass
import com.migo.core.protocol.Feature
import com.migo.core.protocol.Hello
import com.migo.core.protocol.OPCODES
import com.migo.core.protocol.Op
import com.migo.core.protocol.Ping
import com.migo.core.protocol.Welcome
import com.migo.core.wire.Compress
import com.migo.core.wire.Flags
import com.migo.core.wire.Frame
import com.migo.core.wire.Id
import com.migo.core.wire.Limits
import com.migo.core.wire.NIL_ID
import com.migo.core.wire.Reader
import com.migo.core.wire.Writer
import com.migo.core.wire.decodeFrameLengthPrefixed
import com.migo.core.wire.encodeFrame
import com.migo.core.wire.encodeFrameLengthPrefixed
import com.migo.core.wire.frameHeader
import com.migo.core.wire.unpackFrame
import java.io.BufferedInputStream
import java.io.BufferedOutputStream
import java.net.InetSocketAddress
import java.net.Socket
import java.util.concurrent.atomic.AtomicInteger
import kotlinx.coroutines.CancellableContinuation
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.suspendCancellableCoroutine
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeout

/**
 * The TCP binding of the realtime connection — the native client's default transport.
 *
 * Mirrors `clients/desktop/src/net/tcp.rs` and the server's `TcpStreamTransport`: a browser has no
 * TCP socket API, which is why the web client speaks WebSocket, and an Android app has one, which
 * is why this is the native default (brief section 138). One TCP connection, one session, binary
 * length-prefixed frames — the structure every binary messenger since mig33 has used.
 *
 * # Framing
 *
 * TCP supplies no message boundary of its own, so the framing is the brief's stream binding: a
 * `u32` big-endian length prefix followed by one MWP frame, the same framing the server's TCP
 * listener peels off and the QUIC stream path uses. [decodeFrameLengthPrefixed] peels that prefix
 * and hands the state machine exactly the frame bytes every other transport hands it, so the
 * session logic above this class cannot tell a TCP client from a WebSocket one.
 *
 * # The handshake
 *
 * HELLO carries the access token, so a successful connection is authenticated by the time WELCOME
 * arrives — one round trip rather than two. The HELLO ORs in the `TCP_TRANSPORT` feature bit: the
 * negotiated set is the intersection, so a client that does not ask for TCP gets a WELCOME without
 * the bit even from a node that serves it. A node without the listener answers a WELCOME without
 * the bit, which is the contract, not a fault — the caller reads the negotiated features and
 * falls back to WebSocket, the honest outcome, rather than an error screen for a working server.
 *
 * # The heartbeat is an MWP PING, not a socket keep-alive
 *
 * The server refreshes its liveness clock only on an *inbound MWP frame*, and it closes a session
 * that has been quiet for twice the heartbeat it advertised in WELCOME. A TCP keep-alive probes
 * the *path*; a real `PING` opcode answers the *session* deadline. The PING runs on the interval
 * derived from WELCOME — the same derivation and the same halving as [Gateway]'s, for the same
 * deadline arithmetic — and the socket-level keep-alive is left on because the two solve different
 * problems: one keeps NAT mappings warm, the other keeps the session alive.
 *
 * # TLS
 *
 * Plain TCP carries no encryption. A production listener is fronted by TLS 1.3 (the brief's rule:
 * plaintext is for the development loopback only), and this build's Android TLS posture is
 * expressed in the endpoint's `Tcp`/`TcpTls` scheme pair — a `TcpTls` endpoint is not dialled by
 * this class, which speaks MWP over whatever socket it is handed; wrapping the socket is the
 * trust posture of the caller that owns the deployment's certificate story.
 */
class TcpGateway private constructor(
    private val socket: Socket,
    private val scope: CoroutineScope,
    private val state: SocketState,
) : RealtimeTransport {
    /** Correlation ids for request frames. Zero means "not a reply to anything", so ids start at 1. */
    private val nextCorrelation = AtomicInteger(1)

    /** Serialises writes: two coroutines encoding into the same socket must not interleave frames. */
    private val writeLock = Mutex()

    /** The streams, opened once and buffered; writes and reads each go through their own side. */
    private val output = BufferedOutputStream(socket.getOutputStream(), WRITE_BUFFER)
    private val input = BufferedInputStream(socket.getInputStream(), READ_BUFFER)

    private var heartbeat: Job? = null

    /** Frames nobody correlated: broadcasts, and replies whose waiter has gone. */
    override val inbound: Channel<Inbound> get() = state.inbound

    /** The session id from WELCOME, which a resume attempt must name. */
    override val sessionId: Id get() = state.sessionId

    /**
     * The highest `frame_seq` this client has seen. See [Gateway.lastFrameSeq] for the full
     * argument: the count must include every Critical frame, correlated replies included, or the
     * ACK watermark drifts and a reconnect replays messages the user already read.
     */
    override val lastFrameSeq get() = state.lastFrameSeq

    /** A fresh correlation id, for a request whose reply must be matched to it. */
    override fun correlate(): Long {
        // Wrapping is fine and deliberate: correlation ids only have to be unique among the requests
        // currently in flight, and four billion is far beyond that. Mirrors [Gateway.correlate].
        val id = nextCorrelation.getAndUpdate { if (it == Int.MAX_VALUE) 1 else it + 1 }
        return id.toLong() and 0xFFFFFFFFL
    }

    /**
     * Encodes and sends one message as one length-prefixed record.
     *
     * [encode] is the generated `encode(Writer)` of whatever struct the opcode carries, passed as a
     * function for the same reason [Gateway.send] states: the generated types share no supertype,
     * and giving them one would mean editing generated code by hand.
     *
     * Compression is attempted only above [Limits.COMPRESS_MIN_BYTES] and kept only if it actually
     * won — the same rule [Gateway.send] applies, for the same reason.
     */
    override suspend fun send(opcode: Long, correlation: Long, encode: (Writer) -> Unit) {
        val writer = Writer()
        encode(writer)
        val payload = writer.finish()

        val deflated = if (state.compression) Compress.maybeDeflate(payload) else null
        val flags = if (deflated != null) Flags.COMPRESSED else 0
        val header = frameHeader(opcode, correlation).copy(flags = flags)
        val body = encodeFrame(Frame(header, deflated ?: payload))
        if (body.size > Limits.MAX_FRAME_BYTES) throw GatewayError.Malformed

        // One frame out as one length-prefixed record: the mirror of the receive path, and the
        // reason the server's reader never has to guess where a frame ends.
        val wire = encodeFrameLengthPrefixed(Frame(header, deflated ?: payload))
        writeLock.withLock {
            withContext(Dispatchers.IO) {
                try {
                    output.write(wire)
                    output.flush()
                } catch (_: Exception) {
                    // The read side is what reports a dead connection to the waiters; a failed write
                    // means it is already reporting one, and the throwable names hosts and ports
                    // (brief section 174), so it is not passed on.
                    throw GatewayError.Transport
                }
            }
        }
    }

    /**
     * Sends a request and suspends until the reply carrying the same correlation arrives.
     *
     * The waiter is registered *before* the frame goes out — the same race [Gateway.request]
     * documents, with the same answer: a local server can answer before the sending coroutine is
     * scheduled again, and the reply would arrive with no waiter and be delivered to [inbound]
     * instead, leaving this call to hang until its timeout.
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
            if (Gateway.isError(frame)) throw Gateway.refusal(frame)
            return frame
        } finally {
            state.forget(correlation)
        }
    }

    /** Acknowledges every Critical frame seen so far. Cumulative, as [Gateway.acknowledge] is. */
    override suspend fun acknowledge() {
        val seq = state.lastFrameSeq
        if (seq == 0L || seq == state.lastAcked) return
        state.lastAcked = seq
        send(Op.ACK, 0L) { w -> Ack(seq).encode(w) }
    }

    /**
     * Starts the MWP heartbeat, on the interval WELCOME advertised — the same derivation and the
     * same halving as [Gateway.startHeartbeat], for the same deadline arithmetic.
     */
    internal fun startHeartbeat(intervalMs: Long) {
        val period = (intervalMs / 2).coerceAtLeast(MIN_HEARTBEAT_MS)
        heartbeat = scope.launch {
            while (isActive) {
                delay(period)
                try {
                    acknowledge()
                    // The device's own clock, not an estimate of the server's: PONG echoes this
                    // field back unchanged so the round trip can be measured without either end
                    // keeping state.
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
        try {
            socket.close()
        } catch (_: Exception) {
            // Best-effort: a peer already gone is not an error worth surfacing.
        }
        state.fail(GatewayError.Closed)
    }

    companion object {
        /** A floor on the heartbeat period, so a nonsense advertised interval cannot busy-loop. */
        private const val MIN_HEARTBEAT_MS = 5_000L

        /** Write-side buffer. One record per frame, so 4 KiB covers a normal frame in one write. */
        private const val WRITE_BUFFER = 4 * 1024

        /** Read-side buffer. The length prefix is peeled before anything is buffered for a body. */
        private const val READ_BUFFER = 4 * 1024

        /**
         * How long the TCP connect itself may take. Generous for a path that crosses the open
         * internet, short enough that a fallback happens in seconds, not minutes.
         */
        private const val CONNECT_TIMEOUT_MS = 10_000

        /**
         * How long the WELCOME may take after the HELLO is written. The same budget the desktop
         * client's step constant buys: a server that has not answered by now is not going to.
         */
        private const val HANDSHAKE_TIMEOUT_MS = 10_000L

        /**
         * Connects, sends HELLO, and waits for WELCOME.
         *
         * [scope] owns the heartbeat and the read pump, and must outlive the connection — the same
         * contract [Gateway.connect] states: a scope tied to a screen would cancel the heartbeat
         * when the user navigated away and the session would then die on the server's liveness
         * deadline.
         */
        suspend fun connect(
            host: String,
            port: Int,
            scope: CoroutineScope,
            hello: Hello,
        ): Pair<TcpGateway, Welcome> {
            val state = SocketState()
            val socket = withContext(Dispatchers.IO) {
                try {
                    Socket().apply {
                        tcpNoDelay = true
                        connect(InetSocketAddress(host, port), CONNECT_TIMEOUT_MS)
                    }
                } catch (_: Exception) {
                    // The throwable is not passed on: it names hosts and ports, and the caller's
                    // only decision is whether to fall back — which [GatewayError.Transport]
                    // already answers.
                    throw GatewayError.Transport
                }
            }
            val gateway = TcpGateway(socket, scope, state)

            // The read pump starts before the HELLO is written, so the WELCOME's reply has a reader
            // waiting before the request is even sent — the same ordering discipline the OkHttp
            // listener gives [Gateway] for free.
            try {
                gateway.pump()
                val frame = withTimeout(HANDSHAKE_TIMEOUT_MS) {
                    // The HELLO carries the transport's own feature bit: the negotiated set is the
                    // intersection, so a client that does not ask for TCP gets a WELCOME without it
                    // even from a node that serves it — which is the contract, not a fault.
                    val tcpHello = hello.copy(features = hello.features or Feature.TCP_TRANSPORT)
                    gateway.request(Op.HELLO) { w -> tcpHello.encode(w) }
                }
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
                gateway.close()
                throw error
            }
        }
    }

    /**
     * The read pump: one coroutine draining the socket, peeling records, delivering frames.
     *
     * Bytes land in [banked], which holds exactly the record currently being reassembled and the
     * whole records after it. The ceiling is enforced by [decodeFrameLengthPrefixed] before any
     * oversized body is buffered, so a hostile prefix is refused without allocating for it.
     */
    private fun pump() {
        scope.launch(Dispatchers.IO) {
            val chunk = ByteArray(READ_BUFFER)
            try {
                while (isActive) {
                    val read = try {
                        input.read(chunk)
                    } catch (_: Exception) {
                        state.fail(GatewayError.Transport)
                        return@launch
                    }
                    if (read < 0) {
                        // The peer closed its side: a clean end, the same shape a WebSocket close
                        // reads back as.
                        state.fail(GatewayError.Closed)
                        return@launch
                    }
                    banked.write(chunk, 0, read)

                    // Peel every whole record the bank now holds, in order.
                    while (true) {
                        val decoded = try {
                            decodeFrameLengthPrefixed(banked.toByteArray())
                        } catch (_: Exception) {
                            state.fail(GatewayError.Malformed)
                            return@launch
                        }
                        if (decoded == null) break
                        banked.discard(decoded.consumed)
                        deliver(decoded.frame)
                    }
                }
            } catch (_: Exception) {
                state.fail(GatewayError.Transport)
            }
        }
    }

    /** Decompresses, unpacks batches, and hands the resulting frames to the state machine. */
    private fun deliver(frame: Frame) {
        val inflated = if ((frame.header.flags and Flags.COMPRESSED) != 0) {
            try {
                Frame(frame.header, Compress.inflateRaw(frame.payload))
            } catch (_: Exception) {
                state.fail(GatewayError.Malformed)
                return
            }
        } else {
            frame
        }
        // A batch is a transport optimisation, not a message: unpacking it here means nothing
        // above this line has to know the server ever grouped frames.
        val frames = if ((inflated.header.flags and Flags.BATCH) != 0) {
            try {
                unpackFrame(inflated)
            } catch (_: Exception) {
                state.fail(GatewayError.Malformed)
                return
            }
        } else {
            listOf(inflated)
        }
        for (one in frames) {
            state.deliver(one)
        }
    }

    /**
     * The banked receive buffer: a growable byte sink that the pump appends to and the record
     * peeler consumes from the front.
     *
     * An `ArrayDeque<Byte>` was considered and rejected: boxing every byte and copying the queue
     * out to a `ByteArray` on every peel is quadratic in the record size, and a frame can be a
     * quarter megabyte. This holds the bytes contiguously and compacts only what was consumed.
     */
    private val banked = Bank()

    /** A contiguous, growable front-consuming byte buffer. */
    private class Bank {
        private var bytes = ByteArray(READ_BUFFER)
        private var used = 0

        /** Appends [len] bytes of [from]. */
        fun write(from: ByteArray, off: Int, len: Int) {
            if (used + len > bytes.size) {
                var grown = bytes.size
                while (grown < used + len) grown = grown shl 1
                bytes = bytes.copyOf(grown)
            }
            from.copyInto(bytes, used, off, off + len)
            used += len
        }

        /** Drops the first [count] bytes. */
        fun discard(count: Int) {
            if (count >= used) {
                used = 0
                return
            }
            bytes.copyInto(bytes, 0, count, used)
            used -= count
        }

        /** The banked bytes, copied out for [decodeFrameLengthPrefixed]. */
        fun toByteArray(): ByteArray = bytes.copyOf(used)
    }

    /**
     * Everything the read pump and the coroutine side share — the same shape and the same locking
     * argument as [Gateway]'s `SocketState`: the sequence count and the routing decision for a
     * frame have to be one step, and the ACK watermark has to be exact.
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
         * Compression is switched on from the *negotiated* features, never from what HELLO asked
         * for — the same intersection rule [Gateway]'s state documents.
         */
        fun adopt(welcome: Welcome) {
            sessionId = welcome.sessionId
            compression = (welcome.features and Feature.COMPRESSION) != 0uL &&
                Compress.isCompressionAvailable()
            // A resumed session continues the server's sequence, and the client must continue
            // counting from where the server says it stopped rather than from zero.
            welcome.resumeFromSeq?.let { lastFrameSeq = it; lastAcked = it }
        }

        /** Claims a correlation id, before the request that uses it is written. */
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
         * The sequence count is incremented for every Critical frame, including correlated
         * replies, and under the same lock that decides where the frame goes — see
         * [TcpGateway.lastFrameSeq] for why undercounting here is a bug that only shows up after
         * a reconnect.
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
                            // The caller has claimed the id but has not parked yet. Leave the frame
                            // where it will find it, and put the slot back so the lookup succeeds.
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
                    // Cancelled between parking and now: nobody is going to read this, and a
                    // broadcast channel is the wrong place for somebody else's reply.
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
         * Whether an opcode's frames are counted toward the ACK watermark — the same table and
         * the same argument as [Gateway]'s `isSequenced`.
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
}
