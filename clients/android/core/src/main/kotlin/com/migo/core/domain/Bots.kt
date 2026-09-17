package com.migo.core.domain

import com.migo.core.protocol.Acknowledged
import com.migo.core.protocol.BotCommand
import com.migo.core.protocol.BotEvent
import com.migo.core.protocol.BotListReq
import com.migo.core.protocol.BotListResponse
import com.migo.core.protocol.BotPause
import com.migo.core.protocol.BotRegister
import com.migo.core.protocol.BotRotate
import com.migo.core.protocol.BotScopes
import com.migo.core.protocol.BotView
import com.migo.core.protocol.Op
import com.migo.core.wire.Id

/**
 * The bots domain: registering a bot, managing the ones you own, and talking to one.
 *
 * A port of `packages/sdk/src/domains/bots.ts`. Section 41 asks for a bot surface a developer can
 * build against, and section 49 asks that a bot be reportable *as a bot* -- two halves of the same
 * idea, that a bot is an account its owner runs and not a person. This is the developer half: it is
 * the only place in this client where an account registers something that will speak for it, and the
 * only place where the permissions that something holds are set.
 *
 * # A bot is an account, and this is its control panel
 *
 * Registering writes three rows at once -- an account, a profile, and a bot row -- because an account
 * with no bot row cannot be signed into and a bot row with no account has nothing to post under. The
 * credentials are the bot's own: it authenticates by bearer token, never by a passphrase, which is why
 * [register] and [rotate] are the only two calls anywhere in this client that return a secret, and why
 * each returns it exactly once.
 *
 * # The token is shown once, and rotation is the recovery
 *
 * Only a keyed tag of the token is stored, so the node cannot reprint it. An owner who loses the reply
 * to [register] or [rotate] has lost that credential and must rotate again -- which is a property of
 * the design rather than an oversight, because the alternative is a store that can be made to hand out
 * a live credential a second time. Treat the value in [BotView.token] as write-once: put it in a
 * secret store before anything else can fail.
 *
 * # Permissions start at nothing
 *
 * A freshly registered bot holds nothing ([NO_SCOPES]) -- section 41's minimum, which is no authority
 * at all -- and its owner widens it deliberately with [setScopes]. This is the opposite of a default
 * that grants something convenient: a bot that starts able to read messages is a bot that was never
 * asked whether it should.
 *
 * # Reversible control, rather than deletion
 *
 * [setPaused] stops a bot without destroying it. A paused bot refuses to authenticate, so it stops
 * speaking, but its row, its scopes, its webhook and its message history all survive and [setPaused]
 * with `false` brings it back. That is what makes pausing the answer to a bot that is misbehaving --
 * including the answer this client's own reporting path offers, since a misbehaving bot that its owner
 * pauses is a misbehaving bot that has stopped.
 *
 * # Talking to a bot
 *
 * [command] delivers an instruction to the bot's registered webhook and returns as soon as the node
 * has accepted it. The bot's substantive answer does not come back through this call: the bot speaks
 * through its own account, on the ordinary messaging path, like every other participant, and a client
 * sees it as a message in a conversation. [onBotEvent] carries the other direction -- what the node
 * tells the *owner* about their bot, such as a delivery failure.
 */
class BotsDomain(
    private val rpc: Rpc,
    onEventError: EventErrorHandler? = null,
) {
    private val listeners = ListenerSet<BotEvent>(Op.BOT_EVENT, onEventError)

    @Volatile
    private var subscription: Subscription? = null

    /** Begins delivering inbound bot events to registered handlers. Idempotent. */
    fun start() {
        if (subscription != null) return
        subscription = rpc.on(Op.BOT_EVENT, { r -> BotEvent.decode(r) }) { event, _ ->
            listeners.deliver(event)
        }
    }

    /** Stops delivering bot events. Registered handlers are kept for a later [start]. */
    fun stop() {
        val live = subscription ?: return
        subscription = null
        live.cancel()
    }

    /** Registers a handler for inbound bot events. Returns a handle that unsubscribes it. */
    fun onBotEvent(listener: Listener<BotEvent>): Subscription = listeners.add(listener)

    /**
     * Registers a bot owned by the calling account and returns its view, including its token.
     *
     * `username` is the backing account's handle and is validated exactly as a person's would be, so
     * it is lowercase letters, digits, dots and underscores only, and it is taken. `displayName` is
     * what clients show beside the bot's messages -- the value that comes back in [BotView.name] --
     * and the two are genuinely different fields: the handle is unique and rarely seen, the display
     * name is neither.
     *
     * The bot holds no permissions. Call [setScopes] to grant any.
     *
     * The returned `token` is the only time this value exists in a readable form. Store it before
     * doing anything else with it.
     */
    suspend fun register(username: String, displayName: String): BotView {
        val request = BotRegister(username, displayName)
        return rpc.call(Op.BOT_REGISTER, { w -> request.encode(w) }, { r -> BotView.decode(r) })
    }

    /**
     * Lists every bot the calling account owns.
     *
     * The request names no owner because the session is the owner, so there is no id to get wrong and
     * no way to ask about somebody else's bots. The order is the node's -- oldest first -- and is
     * stable across calls, which is what lets a management screen render this answer directly without
     * sorting it and re-sorting it as rows change.
     *
     * Neither `paused` nor `scopes` is ever absent in an answer from a node that understands this
     * call; both are optional on the type because the wire tags them rather than fixing their
     * position, so a node built before pausing existed would omit them. Read `paused != true` rather
     * than `paused == false` if you must run against such a node.
     */
    suspend fun list(): List<BotView> {
        val request = BotListReq()
        val response = rpc.call(
            Op.BOT_LIST,
            { w -> request.encode(w) },
            { r -> BotListResponse.decode(r) },
        )
        return response.bots
    }

    /**
     * Mints a fresh token for a bot, invalidating the old one, and returns the view that carries it.
     *
     * This is the recovery path after a leak or a lost token, and it is immediate: the previous token
     * stops authenticating the moment this returns. There is no grace period and no overlap, because
     * an overlap is a window in which the credential you are trying to kill still works.
     *
     * Rotation changes only the credential. The returned view is the bot as it already was -- same
     * name, same scopes, same paused state -- returned so a caller can prove which bot it rotated
     * rather than because anything in it moved.
     *
     * As with [register], the returned `token` is readable exactly once.
     */
    suspend fun rotate(botId: Id): BotView {
        val request = BotRotate(botId)
        return rpc.call(Op.BOT_ROTATE, { w -> request.encode(w) }, { r -> BotView.decode(r) })
    }

    /**
     * Pauses a bot or resumes it, and returns the bot as it now stands.
     *
     * A paused bot refuses to authenticate -- its token stops working -- while its row, its scopes,
     * its webhook and its history all survive. Resuming is this same call with `false`, so a client
     * needs one control that reflects the current state rather than two that could disagree.
     *
     * The flag is carried rather than implied by the call, so pausing an already-paused bot is a no-op
     * that answers with the same view rather than an error: a client whose earlier pause succeeded but
     * whose reply was lost can retry safely.
     */
    suspend fun setPaused(botId: Id, paused: Boolean): BotView {
        val request = BotPause(botId, paused)
        return rpc.call(Op.BOT_PAUSE, { w -> request.encode(w) }, { r -> BotView.decode(r) })
    }

    /**
     * Replaces a bot's permissions with exactly the set given, and returns the updated view.
     *
     * A replacement and not a delta. Send every scope the bot should hold -- empty to take them all
     * away -- so the call is idempotent in the way a checkbox list is: two clients editing the same
     * bot each send the set they were shown, and the last one wins with a complete set rather than
     * merging into a union neither owner chose.
     *
     * Every slug must be from [BOT_SCOPES]; the node refuses one it does not define rather than
     * ignoring it, so a typo surfaces here as a rejection instead of as a bot that quietly holds less
     * than it was granted. The list is copied onto the wire, so a caller mutating its own list
     * afterwards cannot rewrite what was sent.
     */
    suspend fun setScopes(botId: Id, scopes: List<String>): BotView {
        val request = BotScopes(botId, scopes.toList())
        return rpc.call(Op.BOT_SCOPES, { w -> request.encode(w) }, { r -> BotView.decode(r) })
    }

    /**
     * Delivers an instruction to a bot's registered webhook.
     *
     * Returns once the node has handed the command to the webhook, and not when the bot has acted on
     * it: the bot's answer arrives later, as a message from its own account, on the ordinary messaging
     * path. A bot with no webhook registered, or one that is paused, is refused here -- the first as a
     * validation error, the second as `NOT_FOUND`, which is the same answer an unknown bot id gets so
     * that a caller cannot use this call to discover which bots exist.
     */
    suspend fun command(botId: Id, command: String, args: List<String>? = null): Acknowledged {
        val request = BotCommand(botId, command, args?.toList())
        return rpc.call(Op.BOT_COMMAND, { w -> request.encode(w) }, { r -> Acknowledged.decode(r) })
    }

    companion object {
        /**
         * Every permission a bot can hold, as the slugs the wire carries.
         *
         * The closed set from section 41, and the vocabulary a permission picker is built from. These
         * strings are load-bearing and are never reworded once shipped: an operator's notes, an
         * export, and a stored row all name a permission by exactly this string, so a client must send
         * these and refuse to invent others. The node refuses a slug it does not define rather than
         * dropping it, so a typo here is a rejected call and not a bot that silently holds less than
         * it was granted.
         *
         * Declared here, in the client, rather than derived from the generated protocol because the
         * wire carries strings: the protocol knows the field is a list of strings and cannot know
         * which strings mean something.
         */
        val BOT_SCOPES: List<String> = listOf(
            "read_messages",
            "send_messages",
            "moderate",
            "manage_games",
            "read_members",
            "send_announcements",
        )

        /**
         * The permission set a freshly registered bot holds: nothing.
         *
         * Named rather than left as an empty list literal because it is the answer to a question every
         * caller of [register] asks -- "what can it do so far?" -- and because a caller building a
         * picker wants the empty selection to be a named thing rather than something that reads like
         * an oversight.
         */
        val NO_SCOPES: List<String> = emptyList()
    }
}
