package com.migo.core.domain

import com.migo.core.crypto.Content
import com.migo.core.protocol.MessageAccepted
import com.migo.core.protocol.MessageDelete
import com.migo.core.protocol.MessageEvent
import com.migo.core.protocol.MessageKind
import com.migo.core.protocol.MessageReceipt
import com.migo.core.protocol.MessageSend
import com.migo.core.protocol.Op
import com.migo.core.protocol.ReceiptKind
import com.migo.core.session.GroupCrypto
import com.migo.core.session.SessionCrypto
import com.migo.core.wire.Id
import com.migo.core.wire.newId
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock

/**
 * Sending and receiving messages: the domain that ties the two crypto layers to the wire.
 *
 * Ported from `packages/sdk/src/domains/messaging.ts`, and it has to stay a port: a message sealed
 * here is opened by the web client and the desktop client, so any divergence in *which* layer seals
 * *what* is not a style difference but a message that cannot be read.
 *
 * # The one thing to understand about the crypto here
 *
 * Message content is always sealed under a **sender key**, even in a two-person conversation. One
 * `MESSAGE_SEND` carries exactly one envelope, of [com.migo.core.crypto.SCHEME_SENDER_KEY], and the
 * server fans that single envelope out to every member device. What is sealed *pairwise*, once per
 * recipient device through the Double Ratchet, is the sender-key **distribution** -- the short
 * message that tells a device which chain key to expect. Content is broadcast; the key to the
 * broadcast is unicast.
 *
 * The alternative -- pairwise-sealing the content itself for each of N devices -- is what a naive
 * reading of "end-to-end encrypted" suggests, and it costs N envelopes per message and N ratchet
 * advances. For a group of twenty on three devices each that is sixty seals for one line of text.
 * The sender key is what makes the cost one seal plus a distribution per device *per rotation*.
 *
 * # Why a distribution travels as a message
 *
 * It rides as a [Content.ControlEvent] named [SENDER_KEY_EVENT], sent with
 * [MessageKind.KeyExchange]. There is no dedicated opcode for it, deliberately: a distribution has
 * to arrive at a device *in order relative to the content it unlocks*, and the only ordered,
 * replayable, offline-buffered channel to a device is the message stream itself. A side channel
 * would deliver distributions that outrun or lag the content, and every conversation would open with
 * a message nobody could read.
 *
 * # The consequence: content can arrive before its key
 *
 * Ordering holds per stream, not across senders, and a peer that rotates mid-conversation sends the
 * new distribution as its own message. So an event that cannot be opened is not an error; it is
 * early. Those events are buffered per sender ([MAX_PENDING_PER_SENDER]) and retried the moment that
 * sender's distribution lands. Surfacing them as failures instead would show a user a stream of
 * "could not decrypt" that resolves itself a few hundred milliseconds later.
 *
 * # Why events are serialised
 *
 * [Rpc] delivers events from one pump coroutine, and the handlers here suspend (opening a pairwise
 * envelope may fetch a prekey bundle). Doing that work on the pump would stall the socket; doing it
 * in freely-launched coroutines would let two events mutate the pending buffer at once and would
 * lose the wire order that makes "distribution then content" work in the common case. So each event
 * is launched [CoroutineStart.UNDISPATCHED] -- it runs on the pump thread only as far as its first
 * real suspension -- and takes [eventLock] first. A `Mutex` hands the lock to waiters in the order
 * they asked, and they ask in wire order, so ordering is preserved and the pump is never blocked.
 */

/**
 * The control-event name a sender-key distribution rides under.
 *
 * Byte-identical to `SENDER_KEY_EVENT` in the TypeScript SDK and the desktop client. A typo here
 * would not fail to compile anywhere; it would produce a client whose distributions every peer
 * silently ignores, and whose conversations therefore never open.
 */
const val SENDER_KEY_EVENT = "sender-key"

/**
 * How many not-yet-openable events are held per sending device.
 *
 * Bounded because the buffer is filled by remote peers: without a bound, a device that sends content
 * and never sends a distribution -- through a bug, or on purpose -- grows this map until the process
 * dies. Sixty-four is comfortably more than the handful of messages that can overtake a distribution
 * in flight, and small enough that the worst case across many senders stays trivial.
 *
 * Past the bound the *oldest* buffered event is dropped, not the newest. A held event is only worth
 * holding because it might still be shown, and the older it is the more likely the conversation has
 * moved on; dropping the newest would mean a conversation that never recovers because the arrival
 * that would have unstuck it is the one thrown away.
 */
const val MAX_PENDING_PER_SENDER = 64

/**
 * One recipient device: which account it belongs to, and which device it is.
 *
 * Both halves are needed because a pairwise seal is per *device* but a prekey bundle is fetched per
 * account-and-device, and a device id alone does not say whose it is.
 */
class DeviceAddress(
    /** The account the device belongs to. */
    val userId: Id,
    /** The device itself. */
    val deviceId: Id,
) {
    /** Ids only; there is nothing secret in either. */
    override fun toString(): String = "DeviceAddress($userId/$deviceId)"
}

/**
 * Who a conversation's messages have to reach.
 *
 * An interface rather than a method on this domain because answering it means membership lookups and
 * device lists -- state the client caches and invalidates -- and none of that belongs next to the
 * crypto. [com.migo.core.domain.MessagingDomain] only ever asks the question.
 *
 * # The contract
 *
 * The list must include the sender's **own other devices** and must exclude the **current** device.
 * Both halves matter and both are easy to get wrong: omitting the other devices produces a
 * conversation that reads correctly on the phone that sent it and is unreadable on the same user's
 * tablet, and including the current device makes it pairwise-seal a distribution to itself, which
 * fails at the X3DH because a device has no prekey bundle it can consume from itself.
 */
interface DeviceDirectory {
    /** Every device that must receive this conversation's sender-key distributions. */
    suspend fun recipientDevices(conversationId: Id): List<DeviceAddress>
}

/**
 * A decrypted message, as the interface should show it.
 *
 * The envelope is gone by the time this exists and the [content] is the parsed body, so nothing here
 * needs the crypto layers to interpret. [MessageEvent] is deliberately not passed through in its
 * place: that struct still carries the sealed envelope, and a UI holding one would be a UI holding
 * ciphertext it has no use for.
 */
class IncomingMessage(
    /** The sender's id for the message, stable across devices. */
    val messageId: Id,
    /** Which conversation it belongs to. */
    val conversationId: Id,
    /** The server's per-conversation sequence number, and the cursor a receipt names. */
    val seq: Long,
    /** The account that sent it. */
    val senderId: Id,
    /** The device that sent it, which is what the crypto is keyed by. */
    val senderDevice: Id,
    /** The server-visible kind, used for notification text and list previews. */
    val kind: MessageKind,
    /** The decrypted body. */
    val content: Content,
    /** When the server accepted it, in Unix milliseconds. */
    val createdAt: Long,
    /** The message this one replies to, when it is a reply. */
    val replyTo: Id? = null,
    /** When it was last edited, when it has been. */
    val editedAt: Long? = null,
) {
    /** Ids and metadata only. The content is described by type, never by value (section 174). */
    override fun toString(): String =
        "IncomingMessage($messageId in $conversationId, seq: $seq, from: $senderId/$senderDevice, " +
            "kind: $kind, content_type: ${content.contentType})"
}

/**
 * A message the sender withdrew.
 *
 * Carries no content, because a deletion event carries no envelope: the server dropped the body when
 * it processed the deletion, which is the point of the operation. A receiver treats this as an
 * instruction to remove what it already has.
 */
class MessageDeletion(
    /** The message being withdrawn. */
    val messageId: Id,
    /** Which conversation it was in. */
    val conversationId: Id,
    /** The sequence number of the deletion event itself. */
    val seq: Long,
    /** Who withdrew it. */
    val senderId: Id,
    /** Which of their devices did. */
    val senderDevice: Id,
    /** When the server accepted the deletion. */
    val createdAt: Long,
) {
    /** Ids only. */
    override fun toString(): String =
        "MessageDeletion($messageId in $conversationId, seq: $seq, by: $senderId/$senderDevice)"
}

/**
 * The per-send choices a caller may make.
 *
 * Defaults are the safe ones, so a caller that passes nothing gets padding on. [pad] is offered at
 * all only because a body already at a bucket boundary gains nothing from it; turning it off leaks
 * the exact plaintext length to anyone counting bytes, so it stays opt-out rather than opt-in.
 */
class SendOptions(
    /** Whether to pad the body to a length bucket before sealing. */
    val pad: Boolean = true,
    /** The message this one replies to. */
    val replyTo: Id? = null,
    /** How long until the server expires the message, in milliseconds. */
    val expiresInMs: Long? = null,
)

/**
 * The messaging domain.
 *
 * Owns nothing but its own listeners and pending buffer: the crypto state lives in [sessionCrypto]
 * and [groupCrypto], and membership lives behind [directory]. That is what makes [stop] cheap and
 * [start] repeatable -- there is no session state here to rebuild.
 */
class MessagingDomain(
    private val rpc: Rpc,
    /**
     * Where inbound event handling runs.
     *
     * Injected rather than created here so it dies with the client that owns it. A domain that made
     * its own scope would keep decrypting after sign-out, which is both a leak and a correctness
     * problem: the crypto layers would still be advancing ratchets for an account that is gone.
     */
    private val scope: CoroutineScope,
    private val sessionCrypto: SessionCrypto,
    private val groupCrypto: GroupCrypto,
    private val directory: DeviceDirectory,
    private val onEventError: EventErrorHandler? = null,
) {
    private val messageListeners = ListenerSet<IncomingMessage>(Op.MESSAGE_EVENT, onEventError)
    private val deletionListeners = ListenerSet<MessageDeletion>(Op.MESSAGE_EVENT, onEventError)
    private val receiptListeners = ListenerSet<MessageReceipt>(Op.MESSAGE_RECEIPT, onEventError)

    /** Serialises inbound event handling; see the module note on ordering. */
    private val eventLock = Mutex()

    /** Events held for a sender whose distribution has not arrived. Guarded by [eventLock]. */
    private val pending = HashMap<String, ArrayList<MessageEvent>>()

    /**
     * The live subscriptions, or null when stopped.
     *
     * One volatile reference to an immutable list rather than a mutable list behind a lock: the only
     * writers are [start] and [stop], which are lifecycle calls, and a single reference swap is
     * enough to make "started" a state a reader cannot observe half of.
     */
    @Volatile
    private var subscriptions: List<Subscription>? = null

    /**
     * Subscribes to the message and receipt streams.
     *
     * Idempotent: calling it twice would otherwise register a second pair of handlers and deliver
     * every message to every listener twice, which is the kind of bug that looks like a server
     * problem. A reconnect does not need it -- [Rpc] subscriptions outlive a socket.
     */
    fun start() {
        if (subscriptions != null) return
        subscriptions = listOf(
            rpc.on(Op.MESSAGE_EVENT, { r -> MessageEvent.decode(r) }) { event, _ ->
                // UNDISPATCHED plus a FIFO mutex is what keeps wire order without blocking the
                // pump. See the module note.
                scope.launch(start = CoroutineStart.UNDISPATCHED) { handleEvent(event) }
            },
            rpc.on(Op.MESSAGE_RECEIPT, { r -> MessageReceipt.decode(r) }) { receipt, _ ->
                // Nothing to decrypt: a receipt is a sequence watermark, so it needs no coroutine.
                receiptListeners.deliver(receipt)
            },
        )
    }

    /**
     * Unsubscribes from both streams.
     *
     * Keeps the crypto state and the pending buffer. A stop is a pause -- a backgrounded app, a
     * reconnect the caller wants to drive itself -- and discarding ratchet state on one would mean
     * re-running X3DH against every peer on resume, burning a one-time prekey each time. Discarding
     * the buffer would silently drop messages that were one distribution away from being readable.
     */
    fun stop() {
        val live = subscriptions ?: return
        subscriptions = null
        live.forEach { it.cancel() }
    }

    /** Registers a handler for decrypted messages. */
    fun onMessage(listener: Listener<IncomingMessage>): Subscription = messageListeners.add(listener)

    /** Registers a handler for withdrawn messages. */
    fun onDeletion(listener: Listener<MessageDeletion>): Subscription =
        deletionListeners.add(listener)

    /** Registers a handler for delivery and read receipts. */
    fun onReceipt(listener: Listener<MessageReceipt>): Subscription = receiptListeners.add(listener)

    /**
     * Seals a body once and sends it to the conversation.
     *
     * # Why the order of operations is rotate, distribute, seal
     *
     * [GroupCrypto.sealContent] rotates a chain that has reached its bound rather than refusing to
     * seal, which is the right behaviour for it and a trap for a caller that distributes first: the
     * rotation clears the record of who holds the current chain, so the distributions just sent name
     * a chain that no longer exists, and every recipient buffers the message waiting for a
     * distribution that already came and went. Checking [GroupCrypto.needsRotation] first makes the
     * rotation happen *before* the distribution pass, so the keys handed out are the keys used.
     *
     * The distribution pass is not conditional on there being a new member, because
     * [GroupCrypto.needsDistribution] already answers that per device and answers `true` when there
     * is no chain at all. On a conversation whose members all hold the current chain it is a
     * membership lookup and nothing else.
     *
     * # Why the plaintext is zeroed
     *
     * [Content.encode] allocates a fresh buffer holding the body in the clear. Zeroing it after the
     * seal means a heap dump taken later does not hand over messages the process already sent. It
     * cannot be complete -- the caller's own [Content] still holds the same text -- but the encoded
     * buffer is the copy nothing else has a reference to, so it is the copy that would otherwise sit
     * in the heap until a collector happened to reuse the page.
     */
    suspend fun send(
        conversationId: Id,
        content: Content,
        options: SendOptions = SendOptions(),
    ): MessageAccepted {
        if (groupCrypto.needsRotation(conversationId)) {
            groupCrypto.rotate(conversationId)
        }
        distribute(conversationId)

        val plaintext = content.encode(options.pad)
        val sealed = try {
            groupCrypto.sealContent(conversationId, plaintext)
        } finally {
            plaintext.fill(0)
        }

        val request = MessageSend(
            messageId = newId(),
            conversationId = conversationId,
            kind = kindForContent(content),
            envelope = sealed.envelope,
            replyTo = options.replyTo,
            expiresInMs = options.expiresInMs,
            senderKeyId = sealed.senderKeyId,
        )
        return rpc.call(
            Op.MESSAGE_SEND,
            { w -> request.encode(w) },
            { r -> MessageAccepted.decode(r) },
        )
    }

    /**
     * Gives every member device that still needs it the current sender-key distribution.
     *
     * Public because a client that has just added a member should distribute at that moment rather
     * than on the next send: the alternative is a first message that every new member buffers.
     *
     * # Why each send is awaited
     *
     * One `MESSAGE_SEND` per device, each awaited before the next. Firing them concurrently would be
     * faster and would also mean [GroupCrypto.markDistributed] running for a device whose send
     * failed, since there would be no ordering between the reply and the mark. A device wrongly
     * marked as holding the chain is never sent the distribution again, and is silently unable to
     * read the conversation until the next rotation.
     *
     * # Why a failure is not caught here
     *
     * It propagates, and the devices already done stay done -- [GroupCrypto.markDistributed] has
     * already recorded them. So a retry resumes rather than restarts, and the message the caller was
     * trying to send is not sent, which is the honest outcome: it would not have been readable by
     * the devices that never got the key.
     */
    suspend fun distribute(conversationId: Id) {
        for (device in directory.recipientDevices(conversationId)) {
            if (!groupCrypto.needsDistribution(conversationId, device.deviceId)) continue

            // These bytes carry a chain key in the clear -- the one thing in this file that is
            // secret and not already inside a crypto layer. Sealed immediately, then zeroed, along
            // with the encoded body that embeds a copy of them.
            val distribution = groupCrypto.distributionFor(conversationId)
            val plaintext = Content.ControlEvent(SENDER_KEY_EVENT, distribution).encode()
            val sealed = try {
                sessionCrypto.seal(conversationId, device.userId, device.deviceId, plaintext)
            } finally {
                plaintext.fill(0)
                distribution.fill(0)
            }

            val request = MessageSend(
                messageId = newId(),
                conversationId = conversationId,
                kind = MessageKind.KeyExchange,
                envelope = sealed.envelope,
            )
            rpc.call(
                Op.MESSAGE_SEND,
                { w -> request.encode(w) },
                { r -> MessageAccepted.decode(r) },
            )
            groupCrypto.markDistributed(conversationId, device.deviceId)
        }
    }

    /**
     * Withdraws a message.
     *
     * [forEveryone] is the difference between "remove it from my copy" and "remove it from
     * everyone's". The server enforces who may do the latter; this client does not pre-judge it,
     * because a client-side check would have to duplicate the server's rule and the two would drift.
     */
    suspend fun deleteMessage(
        conversationId: Id,
        messageId: Id,
        forEveryone: Boolean,
    ): MessageAccepted {
        val request = MessageDelete(messageId, conversationId, forEveryone)
        return rpc.call(
            Op.MESSAGE_DELETE,
            { w -> request.encode(w) },
            { r -> MessageAccepted.decode(r) },
        )
    }

    /**
     * Reports delivery or reading up to [seq].
     *
     * A notify rather than a call, and that is not laziness about the reply. A receipt is a
     * *watermark*: it says "everything up to here", so a lost one is corrected by the next one a
     * moment later, and there is nothing a caller could usefully do with an acknowledgement. Making
     * it a request would add a round trip and a pending-reply slot to the most frequent frame a
     * reading client sends.
     *
     * [MessageReceipt.userId] and [MessageReceipt.at] are left unset on purpose: they are the
     * server's to fill when it fans the receipt out, and a client that set them would be asserting
     * facts about itself that the server has to overwrite anyway.
     */
    suspend fun sendReceipt(conversationId: Id, kind: ReceiptKind, seq: Long) {
        val receipt = MessageReceipt(conversationId, kind, seq)
        rpc.notify(Op.MESSAGE_RECEIPT) { w -> receipt.encode(w) }
    }

    /**
     * Starts a fresh outbound chain for a conversation.
     *
     * For a membership change: someone left, so the chain they hold must stop being the chain that
     * seals anything. Not needed for the message bound, which [send] handles.
     */
    fun rotateSenderKey(conversationId: Id) {
        groupCrypto.rotate(conversationId)
    }

    /**
     * Drops crypto state for a conversation, or for one device within it.
     *
     * Both layers, always. Forgetting the ratchet but keeping the sender key would leave a device
     * able to read broadcasts from a peer it can no longer be given a new distribution by, and
     * forgetting the sender key alone would leave the reverse. Neither half is a state worth being
     * able to reach.
     */
    suspend fun forget(conversationId: Id, deviceId: Id? = null) {
        sessionCrypto.forget(conversationId, deviceId)
        groupCrypto.forget(conversationId, deviceId)
    }

    /**
     * Replays a historical event through the live handling path.
     *
     * The seam the sync domain uses. Fetched history is the same events in the same shapes, so
     * replaying them here rather than decrypting them separately means one implementation of "open
     * this, or hold it until its key arrives" instead of two that must agree.
     *
     * Two things the caller owes: ascending [MessageEvent.seq], so a distribution is replayed before
     * the content it unlocks and the pending buffer is a fallback rather than the normal path; and
     * de-duplication, because the ratchet and the sender-key chain both refuse a second decrypt of
     * the same message -- replay protection working as intended, which a caller that replayed twice
     * would see as history that will not open.
     */
    suspend fun ingest(event: MessageEvent) {
        handleEvent(event)
    }

    private suspend fun handleEvent(event: MessageEvent) {
        eventLock.withLock { routeEvent(event) }
    }

    /**
     * Decides what an inbound event is.
     *
     * Deletion is checked first, before the kind, because a deletion event keeps the kind of the
     * message it withdraws: a deleted voice note arrives as `Voice` with `deleted` set and no
     * envelope worth opening. Branching on the kind first would send it to the content path, which
     * would try to decrypt a body the server has already dropped.
     */
    private suspend fun routeEvent(event: MessageEvent) {
        if (event.deleted == true) {
            deletionListeners.deliver(
                MessageDeletion(
                    messageId = event.messageId,
                    conversationId = event.conversationId,
                    seq = event.seq,
                    senderId = event.senderId,
                    senderDevice = event.senderDevice,
                    createdAt = event.createdAt,
                ),
            )
            return
        }
        if (event.kind == MessageKind.KeyExchange) {
            onKeyExchange(event)
            return
        }
        onContent(event)
    }

    /**
     * Handles a pairwise-sealed key-exchange message.
     *
     * # Why a failed open is swallowed
     *
     * A key exchange is broadcast to the conversation like any other message but is sealed for one
     * device. Every *other* device of ours therefore receives a well-formed prekey envelope it
     * cannot open, and that is the expected case, not an error -- there is one such arrival per
     * sibling device per distribution. Reporting it would fill the error channel with the protocol
     * working correctly, and would train whoever reads that channel to ignore it.
     *
     * A body that opens but does not parse is different: that is a peer encoding content this build
     * cannot read, and it goes to [onEventError].
     */
    private suspend fun onKeyExchange(event: MessageEvent) {
        val plaintext = try {
            sessionCrypto.open(
                event.conversationId,
                event.senderId,
                event.senderDevice,
                event.envelope,
            )
        } catch (_: Throwable) {
            return
        }

        val content = try {
            Content.decode(plaintext)
        } catch (cause: Throwable) {
            onEventError?.invoke(Op.MESSAGE_EVENT, cause)
            return
        } finally {
            plaintext.fill(0)
        }

        // Any other control event, or a sender-key event with no body, is something a newer build
        // sends and this one has no handler for. Ignored rather than reported: an unknown signal is
        // forward compatibility, and the sender is a peer we already trust.
        if (content !is Content.ControlEvent) return
        if (content.event != SENDER_KEY_EVENT) return
        val distribution = content.data ?: return

        try {
            groupCrypto.acceptDistribution(event.conversationId, event.senderDevice, distribution)
        } catch (cause: Throwable) {
            onEventError?.invoke(Op.MESSAGE_EVENT, cause)
            return
        } finally {
            distribution.fill(0)
        }

        drainPending(event.conversationId, event.senderDevice)
    }

    /**
     * Handles a broadcast content message.
     *
     * Both reasons for holding an event are the same situation seen at different times: no
     * distribution yet at all, or a distribution for a chain the sender has since rotated away from.
     * A failed [GroupCrypto.open] is therefore buffered rather than reported -- the crypto layer
     * cannot tell "wrong chain" from "tampered", but a peer sealing garbage and a peer that rotated
     * are distinguished by what happens next, and holding the event costs nothing while a real
     * forgery is dropped when the buffer rolls over.
     */
    private suspend fun onContent(event: MessageEvent) {
        if (!groupCrypto.hasReceiver(event.conversationId, event.senderDevice)) {
            buffer(event)
            return
        }
        val plaintext = try {
            groupCrypto.open(event.conversationId, event.senderDevice, event.envelope)
        } catch (_: Throwable) {
            buffer(event)
            return
        }
        emitContent(event, plaintext)
    }

    /** Parses an opened body and hands it to the listeners. Zeroes [plaintext] either way. */
    private fun emitContent(event: MessageEvent, plaintext: ByteArray) {
        val content = try {
            Content.decode(plaintext)
        } catch (cause: Throwable) {
            onEventError?.invoke(Op.MESSAGE_EVENT, cause)
            return
        } finally {
            plaintext.fill(0)
        }
        messageListeners.deliver(
            IncomingMessage(
                messageId = event.messageId,
                conversationId = event.conversationId,
                seq = event.seq,
                senderId = event.senderId,
                senderDevice = event.senderDevice,
                kind = event.kind,
                content = content,
                createdAt = event.createdAt,
                replyTo = event.replyTo,
                editedAt = event.editedAt,
            ),
        )
    }

    /** Holds an event until its sender's distribution arrives. Called under [eventLock]. */
    private fun buffer(event: MessageEvent) {
        val key = pendingKey(event.conversationId, event.senderDevice)
        val queue = pending.getOrPut(key) { ArrayList() }
        queue.add(event)
        while (queue.size > MAX_PENDING_PER_SENDER) {
            queue.removeAt(0)
        }
    }

    /**
     * Retries everything held for one sender, now that its distribution has landed.
     *
     * An event that still will not open goes back in the buffer rather than being dropped: the
     * distribution just accepted may be for a newer chain than some of what is held, and the older
     * chain's distribution may yet arrive out of order. The queue is removed first and rebuilt from
     * what failed, so a sender whose backlog all opens leaves no empty entry behind.
     */
    private fun drainPending(conversationId: Id, senderDevice: Id) {
        val key = pendingKey(conversationId, senderDevice)
        val queue = pending.remove(key) ?: return
        val stillPending = ArrayList<MessageEvent>()
        for (held in queue) {
            val plaintext = try {
                groupCrypto.open(conversationId, senderDevice, held.envelope)
            } catch (_: Throwable) {
                stillPending.add(held)
                continue
            }
            emitContent(held, plaintext)
        }
        if (stillPending.isNotEmpty()) {
            pending[key] = stillPending
        }
    }
}

/**
 * The buffer key: a conversation and the device that sent into it.
 *
 * Per sending device, not per conversation, because a distribution unblocks exactly one sender. A
 * per-conversation buffer would retry every held event on every distribution, and one peer's bound
 * would be shared by all of them -- so a single noisy sender could evict another sender's messages.
 */
private fun pendingKey(conversationId: Id, senderDevice: Id): String =
    "$conversationId|$senderDevice"

/**
 * The server-visible kind for a body.
 *
 * The server never sees the content, so this is the only thing it has to work with: it drives the
 * notification text, the conversation-list preview, and any per-kind policy. It is metadata by
 * design and is deliberately coarse.
 *
 * A [Content.Reaction] maps to [MessageKind.Text] rather than to a kind of its own. It is
 * user-authored content, and the alternative -- a `Reaction` kind -- would tell the server which
 * messages are reactions, which is exactly the traffic-analysis signal that sealing the body is
 * meant to withhold. A [Content.ControlEvent] maps to [MessageKind.System]: not user-authored, and
 * nothing a client should show in a transcript.
 */
private fun kindForContent(content: Content): MessageKind = when (content) {
    is Content.Text -> MessageKind.Text
    is Content.MediaRef -> MessageKind.Media
    is Content.VoiceNoteRef -> MessageKind.Voice
    is Content.Reaction -> MessageKind.Text
    is Content.ControlEvent -> MessageKind.System
    // Unreachable from `send`: `Content.encode` refuses an unsupported body, and it runs first.
    // Present because the `when` must be exhaustive, and `Unknown` is the honest answer for a body
    // this build could not have composed.
    is Content.Unsupported -> MessageKind.Unknown
}
