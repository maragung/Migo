package com.migo.core.domain

import com.migo.core.protocol.Acknowledged
import com.migo.core.protocol.GameAction
import com.migo.core.protocol.GameEvent
import com.migo.core.protocol.Op
import com.migo.core.wire.Id
import java.util.concurrent.ConcurrentHashMap

/**
 * In-room games: submitting a move, and receiving the authoritative result.
 *
 * A port of `packages/sdk/src/domains/games.ts`. The one domain whose model is deliberately
 * server-authoritative rather than peer-to-peer.
 *
 * # The server owns the game state, on purpose
 *
 * A client sends an *intent* ([submit]) and learns the *outcome* ([onEvent]). It never computes the
 * new state itself and never trusts a peer's claim about it. That is the only arrangement that
 * survives a player who edits their client: a game whose rules ran on the participants' devices is a
 * game whose rules are whatever the most modified device says they are.
 *
 * This is also why nothing here is encrypted. A game runs in a room, the room is not end-to-end
 * encrypted (see [RoomsDomain]), and a referee cannot referee a game it cannot see. The line is that
 * private messages are sealed and public game state is not, and conflating the two would mean either a
 * server that reads private chats or a game nobody can adjudicate.
 *
 * # Ordering comes from `stateVersion`, not from arrival
 *
 * [GameEvent.stateVersion] increments once per accepted action. A client renders in that order and
 * treats a gap as a missed event -- refetch the state rather than guessing at the intermediate steps.
 * A client that animated purely on arrival order would show two players' moves swapped whenever the
 * network reordered them, which in a game is not a cosmetic problem.
 *
 * # Why an action id exists, and why this class mints it
 *
 * [GameAction.actionId] makes a submission idempotent: the server records the id and answers a repeat
 * with the original outcome instead of applying the move twice. Without it, a reconnect during a
 * submit would be a coin flip between a lost move and a doubled one, and in a turn-based game a
 * doubled move is a lost game.
 *
 * The ids are minted here rather than by the caller because they must be *per game* and monotonic, and
 * a caller counting them would be a caller that resets the counter when it recreates its game object.
 * They live in memory only, which is the right lifetime: the server scopes them to a game session, so
 * a fresh process starting again at 1 is correct, not a collision.
 *
 * For a deliberate retry of a submission whose answer was lost, pass the [submit] `actionId` back
 * explicitly -- that is the whole point of the field, and a retry that let a fresh id be minted would
 * be a second move.
 */
class GamesDomain(
    private val rpc: Rpc,
    onEventError: EventErrorHandler? = null,
) {
    private val listeners = ListenerSet<GameEvent>(Op.GAME_EVENT, onEventError)

    /**
     * The next action id per game, keyed by the game id's text.
     *
     * A [ConcurrentHashMap] rather than a plain map behind a lock: [ConcurrentHashMap.compute] is
     * atomic per key, which is exactly the scope of the invariant -- two coroutines submitting for the
     * same game must not be handed the same id, and two submitting for different games have no reason
     * to contend. The reference uses a plain `Map` because JavaScript has one thread; Kotlin does not
     * get to make that assumption.
     *
     * Keyed by [Id.value] rather than by [Id] because a value class is boxed the moment it becomes a
     * generic type argument, and the box would be allocated on every lookup for no gain.
     */
    private val nextActionId = ConcurrentHashMap<String, Long>()

    @Volatile
    private var subscription: Subscription? = null

    /** Begins delivering game events to registered handlers. Idempotent. */
    fun start() {
        if (subscription != null) return
        subscription = rpc.on(Op.GAME_EVENT, { r -> GameEvent.decode(r) }) { event, _ ->
            listeners.deliver(event)
        }
    }

    /** Stops delivery. Registered handlers and the action-id counters are kept for a later [start]. */
    fun stop() {
        val live = subscription ?: return
        subscription = null
        live.cancel()
    }

    /**
     * Registers a handler for authoritative game events.
     *
     * [GameEvent.payload] is a game-specific byte string this SDK does not interpret; [GameEvent.text]
     * is an optional line the server composed for a client that has no renderer for this game, so a
     * generic room UI can still show something meaningful.
     */
    fun onEvent(listener: Listener<GameEvent>): Subscription = listeners.add(listener)

    /**
     * Submits an action to a game.
     *
     * The returned [Acknowledged] says the server accepted the submission, not that the move
     * succeeded: the outcome arrives as a [GameEvent], possibly after other players' moves. A client
     * that treated the acknowledgement as the result would be rendering its own guess.
     *
     * [action] is the game's verb and [args] its operands, both game-defined. Pass [actionId] only to
     * retry a submission whose answer was lost; leaving it null mints a fresh one.
     */
    suspend fun submit(
        gameId: Id,
        roomId: Id,
        action: String,
        args: List<String>? = null,
        actionId: Long? = null,
    ): Acknowledged {
        val id = actionId ?: allocate(gameId)
        val request = GameAction(gameId, roomId, id, action, args)
        return rpc.call(
            Op.GAME_ACTION,
            { w -> request.encode(w) },
            { r -> Acknowledged.decode(r) },
        )
    }

    /**
     * The action id this client last used for a game, or null if it has submitted none.
     *
     * For a client that wants to persist its counter across a restart, and for a test that wants to
     * assert the sequence. Reading it does not advance it.
     */
    fun lastActionId(gameId: Id): Long? = nextActionId[gameId.value]?.minus(1L)

    /**
     * Forgets a game's action-id counter.
     *
     * Called when a game ends or a room is left. Keeping the counter would leak a slot per game played
     * for the lifetime of the process, and the server scopes ids to a game session anyway, so there is
     * nothing to preserve.
     */
    fun forget(gameId: Id) {
        nextActionId.remove(gameId.value)
    }

    /** Hands out the next action id for a game, atomically with respect to that game's key. */
    private fun allocate(gameId: Id): Long {
        var assigned = 1L
        nextActionId.compute(gameId.value) { _, current ->
            assigned = current ?: 1L
            assigned + 1L
        }
        return assigned
    }
}
