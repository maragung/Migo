package com.migo.core.net

import com.migo.core.wire.Id
import com.migo.core.wire.Writer
import kotlinx.coroutines.channels.Channel

/**
 * The realtime surface the session layer speaks, independent of what carries it.
 *
 * `Gateway` (WebSocket) and `TcpGateway` (raw TCP, the native client's default) both implement
 * this, which is what lets [com.migo.core.domain.Rpc] and the session layer above it stay
 * transport-blind: the bytes on the socket differ — one frame per WebSocket message versus a u32
 * length-prefixed record — but the contract a domain needs is the same either way, and it is this
 * interface, not either class.
 *
 * Everything here is the *negotiated-session* view, not the *connection* view: the members are
 * what a live, authenticated session offers. [connect]-time concerns (which scheme, which port,
 * TLS, DNS) belong to each transport's own factory, because they have nothing in common — a
 * WebSocket URL and a host:port pair are not the same shape and pretending they are would only
 * move the branch somewhere less honest.
 *
 * One instance is one connection. A dropped connection is not repaired in place: [sessionId] and
 * [lastFrameSeq] are what a caller needs to build the resume request for the next one, and
 * reconnection policy belongs above this layer, which is the only thing that knows whether the
 * user is still looking at the screen.
 */
interface RealtimeTransport {
    /** Frames nobody correlated: broadcasts, and replies whose waiter has gone. */
    val inbound: Channel<Inbound>

    /** The session id from WELCOME, which a resume attempt must name. */
    val sessionId: Id

    /** The highest `frame_seq` this client has seen. See [Gateway.lastFrameSeq] for the argument. */
    val lastFrameSeq: Long

    /** A fresh correlation id, for a request whose reply must be matched to it. */
    fun correlate(): Long

    /** Encodes and sends one message as one framed unit on this transport. */
    suspend fun send(opcode: Long, correlation: Long, encode: (Writer) -> Unit)

    /**
     * Sends a request and suspends until the reply carrying the same correlation arrives.
     *
     * An ERROR-flagged reply is raised as [GatewayError.Refused] rather than handed back as a
     * frame, so a caller cannot forget to check the flag and read an error body as a success
     * struct.
     */
    suspend fun request(opcode: Long, encode: (Writer) -> Unit): com.migo.core.wire.Frame

    /** Acknowledges every Critical frame seen so far, when the transport tracks an ACK watermark. */
    suspend fun acknowledge()

    /** Closes the connection politely, so the server retires the session rather than timing it out. */
    fun close()
}
