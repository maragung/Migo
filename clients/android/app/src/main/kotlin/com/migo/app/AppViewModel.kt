package com.migo.app

import android.app.Application
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import com.migo.app.model.ActivityCategory
import com.migo.app.model.ActivityRow
import com.migo.app.model.AppState
import com.migo.app.model.ChainNetworkChoice
import com.migo.app.model.ChainTxRow
import com.migo.app.model.ChatMessage
import com.migo.app.model.ChatState
import com.migo.app.model.ConversationRow
import com.migo.app.model.PreparedChainTx
import com.migo.app.model.TrackingChainTx
import com.migo.app.model.parseAvaxAmount
import com.migo.app.session.MigoSession
import com.migo.app.session.SessionHooks
import com.migo.core.ConnectionState
import com.migo.core.account.AVALANCHE_MAINNET
import com.migo.core.account.Eip1559Tx
import com.migo.core.account.EvmWallet
import com.migo.core.account.FUJI_TESTNET
import com.migo.core.account.Network
import com.migo.core.account.eip55
import com.migo.core.account.parseAddress
import com.migo.core.crypto.Content
import com.migo.core.domain.IncomingMessage
import com.migo.core.domain.MessageDeletion
import com.migo.core.domain.SendOptions
import com.migo.core.domain.Subscription
import com.migo.core.net.ChainClient
import com.migo.core.net.TrackOptions
import com.migo.core.net.TrackOutcome
import com.migo.core.net.TrackResult
import com.migo.core.protocol.ConversationKind
import com.migo.core.protocol.ConversationSummary
import com.migo.core.protocol.InboxItem
import com.migo.core.protocol.LedgerEntryWire
import com.migo.core.protocol.NotificationEvent
import com.migo.core.protocol.ReceiptKind
import com.migo.core.protocol.RoomJoinResponse
import com.migo.core.protocol.RoomSummary
import com.migo.core.protocol.TypingEvent
import com.migo.core.protocol.TypingState
import com.migo.core.store.ServerEndpoint
import com.migo.core.store.Settings
import com.migo.core.store.TxRecord
import com.migo.core.wire.Id
import com.migo.core.wire.WireError
import com.migo.core.wire.parseId
import java.math.BigInteger
import java.util.concurrent.ConcurrentHashMap
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch

/**
 * Everything the screens are allowed to know, and every action they are allowed to take.
 *
 * The screens are pure functions of [state] -- they read one immutable object and call methods here.
 * That is the same discipline the desktop client keeps, and it exists for a specific reason: message
 * state arrives on SDK threads, from a socket, while a person is scrolling. A screen that reached into
 * the client for the current list would be reading a structure mid-mutation. One [MutableStateFlow]
 * that only this class writes gives every screen a consistent snapshot, and gives Compose a single
 * thing to recompose on.
 *
 * # Where the password is not
 *
 * No action here stores a password, and [AppState] has no field for one. The sign-in form holds it in
 * a local `remember` for as long as the form is on screen and passes it to [signIn] as an argument.
 * A password on the state object would be a password in every recomposition, in the saved-state bundle
 * if anyone added one, and in whatever a future `toString()` prints.
 *
 * # Where the plaintext is
 *
 * In memory, in [ChatState.messages], for as long as a chat is open, and nowhere else. There is no
 * message database in this build: closing a chat drops its plaintext, and reopening it fetches the
 * ciphertext again and decrypts it again. That is a deliberate omission rather than a finished
 * decision -- a persistent local store has to be sealed under a Keystore-held key exactly like the
 * ratchets are (section 4779), and doing that badly is worse than not having it.
 */
class AppViewModel(application: Application) : AndroidViewModel(application) {

    private val settings = Settings.open(application)

    private val _state = MutableStateFlow<AppState>(AppState.Starting)

    /** The single source of truth for every screen. */
    val state: StateFlow<AppState> = _state.asStateFlow()

    private var session: MigoSession? = null
    private val subscriptions = ArrayList<Subscription>()

    /**
     * Display names by account id, filled as conversations and messages arrive.
     *
     * Concurrent because the SDK's listeners run on its own dispatcher while the UI thread reads it.
     * Nothing in it is secret: these are the names the profile endpoint hands to anyone who asks.
     */
    private val names = ConcurrentHashMap<Id, String>()

    init {
        viewModelScope.launch { bootstrap() }
    }

    // --- authentication ---

    /**
     * Replaces the working endpoint. Called by the form's "Use this server" button
     * after validation.
     *
     * The form holds the typed text in its own `rememberSaveable` state and only
     * calls this once the user has clicked "Use this server". A keystroke never
     * reaches the view model -- a partial host (one that does not yet satisfy the
     * `ServerEndpoint.init` check) is never constructed.
     */
    fun setServerEndpoint(endpoint: ServerEndpoint) = signedOut {
        it.copy(serverEndpoint = endpoint, failure = null)
    }

    /** Records what was typed in the account field. */
    fun setIdentifier(text: String) = signedOut { it.copy(identifier = text, failure = null) }

    /** Signs in an existing account, or creates one when [create] is set. */
    fun signIn(password: String, create: Boolean) {
        val form = _state.value as? AppState.SignedOut ?: return
        if (form.busy) return
        val endpoint = form.serverEndpoint
        val identifier = form.identifier.trim()
        if (identifier.isEmpty() || password.isEmpty()) {
            signedOut { it.copy(failure = "Fill in both fields first.") }
            return
        }

        signedOut { it.copy(busy = true, failure = null) }
        viewModelScope.launch {
            try {
                val opened = if (create) {
                    MigoSession.register(
                        getApplication(),
                        BuildConfig.VERSION_NAME,
                        endpoint,
                        identifier,
                        password,
                        hooks(),
                    )
                } else {
                    MigoSession.signIn(
                        getApplication(),
                        BuildConfig.VERSION_NAME,
                        endpoint,
                        identifier,
                        password,
                        hooks(),
                    )
                }
                settings.update { it.copy(serverEndpoint = endpoint, onboardingComplete = true) }
                attach(opened)
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (failure: Exception) {
                signedOut { it.copy(busy = false, failure = readable(failure)) }
            }
        }
    }

    /**
     * Signs out and forgets this device's keys.
     *
     * Irreversible by design: the identity and every ratchet go, so signing back in is a new device
     * to every peer. That is what signing out has to mean when the alternative is leaving decryptable
     * material on a phone somebody else is about to hold.
     */
    fun signOut() {
        val leaving = session ?: return
        session = null
        detach()
        _state.value = AppState.Starting
        viewModelScope.launch {
            val endpoint = settings.current().serverEndpoint
            try {
                leaving.signOut()
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (_: Exception) {
                // Local material is gone either way; a server that could not be told does not make
                // this device any less signed out.
            }
            names.clear()
            _state.value = AppState.SignedOut(endpoint)
        }
    }

    // --- conversations ---

    /** Reloads the conversation list from the server. */
    fun refreshConversations() {
        val live = session ?: return
        signedIn { it.copy(loading = true, failure = null) }
        viewModelScope.launch {
            try {
                val response = live.client.loadConversations(CONVERSATION_PAGE)
                learnNames(live, response.conversations)
                val rows = response.conversations.map { row(live, it) }
                signedIn { it.copy(loading = false, conversations = rows) }
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (failure: Exception) {
                signedIn { it.copy(loading = false, failure = readable(failure)) }
            }
        }
    }

    /**
     * Starts a direct conversation with the account whose id is [peer].
     *
     * An id rather than a name because there is no directory endpoint yet: the search opcodes are
     * still spec in the brief, so asking for a username here would be a field that cannot work.
     */
    fun startDirect(peer: String) {
        val live = session ?: return
        val peerId = try {
            parseId(peer.trim())
        } catch (_: WireError) {
            signedIn { it.copy(failure = "That is not a valid account id.") }
            return
        }

        viewModelScope.launch {
            try {
                val summary = live.client.startConversation(
                    ConversationKind.Direct,
                    listOf(live.client.accountId, peerId),
                )
                learnNames(live, listOf(summary))
                val fresh = row(live, summary)
                signedIn { current ->
                    val absent = current.conversations.none {
                        it.conversationId == fresh.conversationId
                    }
                    val rows = if (absent) {
                        listOf(fresh) + current.conversations
                    } else {
                        current.conversations
                    }
                    current.copy(conversations = rows, failure = null)
                }
                open(fresh.conversationId, fresh.title)
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (failure: Exception) {
                signedIn { it.copy(failure = readable(failure)) }
            }
        }
    }

    /**
     * Opens a chat and loads its recent history.
     *
     * The catch-up call hands every event back through the SDK's decrypt path, so the messages arrive
     * on the ordinary listener rather than as a return value. The screen shows a spinner until they do.
     */
    fun open(conversationId: Id, title: String) {
        val live = session ?: return
        signedIn { current ->
            val row = current.conversations.find { it.conversationId == conversationId }
            current.copy(
                open = ChatState(
                    conversationId = conversationId,
                    title = title,
                    roomId = row?.roomId,
                    loading = true,
                ),
                conversations = current.conversations.map {
                    if (it.conversationId == conversationId) it.copy(unread = 0) else it
                },
            )
        }
        viewModelScope.launch {
            try {
                live.client.watchConversation(conversationId)
                val response = live.client.catchUp(conversationId, HISTORY_FROM, HISTORY_LIMIT)
                inChat(conversationId) { it.copy(loading = false) }
                if (response.toSeq > 0) {
                    live.client.messaging.sendReceipt(conversationId, ReceiptKind.Read, response.toSeq)
                }
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (failure: Exception) {
                inChat(conversationId) { it.copy(loading = false) }
                signedIn { it.copy(failure = readable(failure)) }
            }
        }
    }

    /** Closes the open chat, which is what drops its decrypted messages. */
    fun closeChat() = signedIn { it.copy(open = null) }

    // --- composing ---

    /** Records the draft, and tells the conversation somebody is typing. */
    fun setDraft(text: String) {
        val current = (_state.value as? AppState.SignedIn)?.open ?: return
        val wasEmpty = current.draft.isEmpty()
        inChat(current.conversationId) { it.copy(draft = text) }
        if (wasEmpty == text.isEmpty()) return
        val live = session ?: return
        val next = if (text.isEmpty()) TypingState.Stop else TypingState.Start
        viewModelScope.launch {
            // A typing indicator that could not be sent is not worth a message to the user, and not
            // worth failing a keystroke over.
            try {
                live.client.typing.set(current.conversationId, next)
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (_: Exception) {
                // Ignored on purpose.
            }
        }
    }

    /**
     * Seals and sends the draft.
     *
     * The draft is cleared before the send completes, because a person who pressed send has moved on
     * and a field that empties only on acknowledgement feels broken on a slow link. The message
     * appears as pending and is replaced when the server accepts it.
     */
    fun send() {
        val live = session ?: return
        val chat = (_state.value as? AppState.SignedIn)?.open ?: return
        val text = chat.draft.trim()
        if (text.isEmpty() || chat.sending) return

        inChat(chat.conversationId) { it.copy(draft = "", sending = true) }
        viewModelScope.launch {
            try {
                val accepted = live.client.messaging.send(
                    chat.conversationId,
                    Content.Text(text),
                    SendOptions(),
                )
                val mine = ChatMessage(
                    messageId = accepted.messageId,
                    seq = accepted.seq,
                    mine = true,
                    author = live.username,
                    text = text,
                    at = accepted.createdAt,
                )
                inChat(chat.conversationId) {
                    it.copy(sending = false, messages = merge(it.messages, mine))
                }
                bump(chat.conversationId, text, accepted.createdAt, unread = false)
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (failure: Exception) {
                // The text goes back in the field: losing what somebody wrote is worse than showing
                // them the error twice.
                inChat(chat.conversationId) { it.copy(sending = false, draft = text) }
                signedIn { it.copy(failure = readable(failure)) }
            }
        }
    }

    /** Clears the error banner. */
    fun dismissFailure() {
        _state.update { current ->
            when (current) {
                is AppState.SignedIn -> current.copy(failure = null)
                is AppState.SignedOut -> current.copy(failure = null)
                AppState.Starting -> current
            }
        }
    }

    // --- sections ---

    /**
     * Switches the shell to a section, loading its data on first entry.
     *
     * A section never yet visited holds nulls; this is the moment they become a read. Re-entering a
     * section keeps what it holds (the conversations list refreshes through its own control), so a
     * tour through the tab strip costs one read per section, not one per visit.
     */
    fun selectSection(section: AppState.Section) {
        signedIn { it.copy(section = section) }
        when (section) {
            AppState.Section.ROOMS -> if (signedInState?.rooms?.rooms == null) loadRooms()
            AppState.Section.FEED -> if (!spaceLoaded()) loadSpace()
            AppState.Section.FRIENDS -> if (!friendsLoaded()) loadFriends()
            AppState.Section.SEARCH -> Unit
            AppState.Section.WALLET -> if (!walletLoaded()) loadWallet()
            AppState.Section.ALERTS -> if (!alertsLoaded()) loadAlerts()
            AppState.Section.CHATS, AppState.Section.GAMES, AppState.Section.PROFILE -> Unit
        }
    }

    /** The Rooms directory read, with whatever query is held. */
    fun loadRooms() {
        val live = session ?: return
        val query = signedInState?.rooms?.query?.trim().orEmpty()
        signedIn { it.copy(rooms = it.rooms.copy(loading = true)) }
        viewModelScope.launch {
            try {
                val response = live.client.rooms.list(30, query = query.ifEmpty { null })
                signedIn { it.copy(rooms = it.rooms.copy(loading = false, rooms = response.rooms)) }
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (failure: Exception) {
                signedIn { it.copy(rooms = it.rooms.copy(loading = false), failure = readable(failure)) }
            }
        }
    }

    /**
     * Records the directory's query text and re-reads the page after a pause.
     *
     * Debounced like the Search field: one round trip per question, not per keystroke, against a
     * rate-limited endpoint.
     */
    fun setRoomsQuery(text: String) {
        signedIn { it.copy(rooms = it.rooms.copy(query = text)) }
        roomsJob?.cancel()
        roomsJob = viewModelScope.launch {
            delay(SEARCH_DEBOUNCE_MS)
            if (session != null) loadRooms()
        }
    }

    private var roomsJob: Job? = null

    /**
     * Joins a room and opens its conversation.
     *
     * The join reply is the one moment the wire names both halves — the room and the conversation —
     * so the row is noted into the conversation list exactly as a started direct chat is, and the
     * thread opens on top of the shell.
     */
    fun joinRoom(room: RoomSummary) {
        val live = session ?: return
        if (signedInState?.rooms?.joining?.contains(room.roomId) == true) return
        signedIn { it.copy(rooms = it.rooms.copy(joining = it.rooms.joining + room.roomId)) }
        viewModelScope.launch {
            try {
                val joined: RoomJoinResponse = live.client.rooms.join(room.roomId)
                noteRoom(joined)
                signedIn { current ->
                    current.copy(
                        section = AppState.Section.CHATS,
                        rooms = current.rooms.copy(joining = current.rooms.joining - room.roomId),
                    )
                }
                open(joined.conversationId, joined.room.name)
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (failure: Exception) {
                signedIn { it.copy(rooms = it.rooms.copy(joining = it.rooms.joining - room.roomId), failure = readable(failure)) }
            }
        }
    }

    /** Projects a join (or create) reply into the conversation list, keeping the room id for leave. */
    private fun noteRoom(joined: RoomJoinResponse) {
        val fresh = ConversationRow(
            conversationId = joined.conversationId,
            title = joined.room.name,
            kind = ConversationKind.Room,
            roomId = joined.room.roomId,
            preview = null,
            unread = 0,
            updatedAt = 0,
        )
        signedIn { current ->
            val absent = current.conversations.none { it.conversationId == fresh.conversationId }
            current.copy(
                conversations = if (absent) {
                    listOf(fresh) + current.conversations
                } else {
                    current.conversations.map {
                        if (it.conversationId == fresh.conversationId) it.copy(roomId = fresh.roomId) else it
                    }
                },
            )
        }
    }

    /**
     * Creates a room and opens its conversation — creation is entry, so the reply is a join
     * handle and the flow is the join flow's, projected the same way.
     */
    fun createRoom(slug: String, name: String, kind: com.migo.core.protocol.RoomKind, topic: String?) {
        val live = session ?: return
        viewModelScope.launch {
            try {
                val joined: RoomJoinResponse = live.client.rooms.create(slug, name, kind, topic)
                noteRoom(joined)
                signedIn { it.copy(section = AppState.Section.CHATS) }
                open(joined.conversationId, joined.room.name)
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (failure: Exception) {
                signedIn { it.copy(failure = readable(failure)) }
            }
        }
    }

    /**
     * Leaves a room: the server closes the conversation for this account, and the list stops
     * offering it. The open chat (if it is this room's) closes with it.
     */
    fun leaveRoom(conversationId: Id, roomId: Id) {
        val live = session ?: return
        viewModelScope.launch {
            try {
                live.client.rooms.leave(roomId)
                signedIn { current ->
                    current.copy(
                        conversations = current.conversations.filterNot { it.conversationId == conversationId },
                        open = if (current.open?.conversationId == conversationId) null else current.open,
                    )
                }
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (failure: Exception) {
                signedIn { it.copy(failure = readable(failure)) }
            }
        }
    }

    /** The Space stream's durable halves: the inbox and the statement, merged newest first. */
    fun loadSpace() {
        val live = session ?: return
        signedIn { it.copy(space = it.space.copy(loading = true)) }
        viewModelScope.launch {
            val inbox = runCatching { live.client.notifications.listNotifications(50) }.getOrDefault(emptyList())
            val ledger = runCatching { live.client.economy.getLedger(20) }.getOrDefault(emptyList())
            val rows = (inbox.map { inboxRow(it) } + ledger.map { ledgerRow(it) })
                .sortedByDescending { it.at }
            signedIn { it.copy(space = it.space.copy(loading = false, rows = rows)) }
        }
    }

    /** The Friends section's read: the graph and the suggestions, together. */
    fun loadFriends() {
        val live = session ?: return
        signedIn { it.copy(friends = it.friends.copy(loading = true)) }
        viewModelScope.launch {
            try {
                val entries = live.client.social.listAllRelationships()
                val suggested = runCatching { live.client.social.suggestions(8) }.getOrDefault(emptyList())
                signedIn { it.copy(friends = it.friends.copy(loading = false, entries = entries, suggestions = suggested)) }
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (failure: Exception) {
                signedIn { it.copy(friends = it.friends.copy(loading = false), failure = readable(failure)) }
            }
        }
    }

    /** Sends a friend request to one account. */
    fun friendRequest(userId: Id) = socialAction(userId) { it.friendRequest(userId) }

    /** Answers a pending friend request. */
    fun friendRespond(userId: Id, accept: Boolean) = socialAction(userId) { it.friendRespond(userId, accept) }

    /** Blocks an account; the graph re-reads after, exactly as the web client does. */
    fun blockUser(userId: Id) = socialAction(userId) { it.blockUser(userId) }

    private fun socialAction(userId: Id, action: suspend (com.migo.core.domain.SocialDomain) -> Unit) {
        val live = session ?: return
        if (signedInState?.friends?.busy?.contains(userId) == true) return
        signedIn { it.copy(friends = it.friends.copy(busy = it.friends.busy + userId)) }
        viewModelScope.launch {
            try {
                action(live.client.social)
                loadFriends()
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (failure: Exception) {
                signedIn { it.copy(friends = it.friends.copy(busy = it.friends.busy - userId), failure = readable(failure)) }
            }
        }
    }

    /** Records the search field; the read is debounced, not per keystroke. */
    fun setSearchQuery(text: String) {
        signedIn { it.copy(search = it.search.copy(query = text)) }
        searchJob?.cancel()
        if (text.isBlank()) {
            signedIn { it.copy(search = it.search.copy(people = null, rooms = null, loading = false)) }
            return
        }
        searchJob = viewModelScope.launch {
            delay(SEARCH_DEBOUNCE_MS)
            runSearch(text.trim())
        }
    }

    private var searchJob: Job? = null

    private suspend fun runSearch(query: String) {
        val live = session ?: return
        signedIn { it.copy(search = it.search.copy(loading = true)) }
        val people = runCatching { live.client.social.search(query, 10) }.getOrDefault(emptyList())
        val rooms = runCatching {
            live.client.rooms.list(10, query = query).rooms
        }.getOrDefault(emptyList())
        // The query may have moved on while the wire answered; only the answer to the current text lands.
        if (signedInState?.search?.query?.trim() != query) return
        signedIn { it.copy(search = it.search.copy(loading = false, people = people, rooms = rooms)) }
    }

    /** Starts a direct conversation with an account already held as an id (a search result). */
    fun startDirectWith(peer: Id) {
        val live = session ?: return
        viewModelScope.launch {
            try {
                val summary = live.client.startConversation(
                    ConversationKind.Direct,
                    listOf(live.client.accountId, peer),
                )
                learnNames(live, listOf(summary))
                val fresh = row(live, summary)
                signedIn { current ->
                    val absent = current.conversations.none { it.conversationId == fresh.conversationId }
                    current.copy(
                        section = AppState.Section.CHATS,
                        conversations = if (absent) listOf(fresh) + current.conversations else current.conversations,
                    )
                }
                open(fresh.conversationId, fresh.title)
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (failure: Exception) {
                signedIn { it.copy(failure = readable(failure)) }
            }
        }
    }

    /** The Wallet's combined read: balance, statement, progression, badges, leaders, catalogue. */
    fun loadWallet() {
        val live = session ?: return
        signedIn { it.copy(wallet = it.wallet.copy(loading = true)) }
        viewModelScope.launch {
            val wallet = runCatching { live.client.economy.getBalance() }.getOrNull()
            val ledger = runCatching { live.client.economy.getLedger(10) }.getOrDefault(emptyList())
            val progression = runCatching { live.client.economy.getProgression(live.client.accountId) }.getOrNull()
            val badges = runCatching { live.client.economy.getBadges(live.client.accountId) }.getOrDefault(emptyList())
            val leaders = runCatching { live.client.economy.getLeaderboard("xp", 10) }.getOrDefault(emptyList())
            val catalogue = runCatching { live.client.economy.getGiftCatalogue() }.getOrDefault(emptyList())
            signedIn {
                it.copy(
                    wallet = it.wallet.copy(
                        loading = false,
                        balance = wallet?.balance,
                        points = wallet?.points,
                        ledger = ledger,
                        progression = progression,
                        badges = badges,
                        leaders = leaders,
                        catalogue = catalogue,
                    ),
                )
            }
        }
    }

    /**
     * Sends a gift; the wallet re-reads after, because the server's arithmetic is the only arithmetic
     * worth showing.
     */
    fun sendGift(sku: String, recipient: Id) {
        val live = session ?: return
        viewModelScope.launch {
            try {
                live.client.economy.sendGift(sku, recipient)
                loadWallet()
                signedIn { it.copy(failure = null) }
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (failure: Exception) {
                signedIn { it.copy(failure = readable(failure)) }
            }
        }
    }

    // --- the chain wallet (§184) ---------------------------------------------------

    /**
     * What a device without the root is told, in one sentence, wherever the AVAX wallet is asked
     * for. Additional devices have no wallet here at all — the address is a function of the root —
     * and pretending otherwise would be a wallet surface that cannot send.
     */
    private val noRootOnDevice =
        "This device does not hold the account root, so it has no AVAX wallet; open the wallet on the device that holds the account backup."

    /** The two names the surface offers, resolved to the pinned `Network` constant each carries. */
    private fun networkOf(choice: ChainNetworkChoice): Network = when (choice) {
        ChainNetworkChoice.MAINNET -> AVALANCHE_MAINNET
        ChainNetworkChoice.FUJI -> FUJI_TESTNET
    }

    /** The session's tracked list as the Activity surface draws it. The list is kept newest first. */
    private fun chainActivity(live: MigoSession): List<ChainTxRow> = live.trackedTxs.map { record ->
        ChainTxRow(
            txHash = "0x" + hexOf(record.txHash),
            network = networkName(record.chainId),
            to = eip55(record.to),
            valueWei = record.valueWei,
            feeWei = record.feeWei,
            gasLimit = record.gasLimit,
            at = record.atUnix * 1000,
            outcome = record.outcome,
            block = record.block,
            gasUsed = record.gasUsed,
        )
    }

    /** A chain id as its network's name; one this build cannot name labels itself honestly. */
    private fun networkName(chainId: Long): String = when (chainId) {
        AVALANCHE_MAINNET.chainId -> AVALANCHE_MAINNET.name
        FUJI_TESTNET.chainId -> FUJI_TESTNET.name
        else -> "chain $chainId"
    }

    /**
     * Lowercase hex for the public material the chain surface shows: transaction hashes and
     * nothing else. The core's own `hexOf` is internal to it, and widening the SDK's surface for
     * two display sites is the worse trade.
     */
    private fun hexOf(bytes: ByteArray): String {
        val digits = "0123456789abcdef"
        val out = StringBuilder(bytes.size * 2)
        for (b in bytes) {
            val value = b.toInt() and 0xff
            out.append(digits[value ushr 4]).append(digits[value and 0x0f])
        }
        return out.toString()
    }

    /**
     * Switches the AVAX surface to a network, by name — never a URL, §184's rule against
     * self-supplied RPCs.
     *
     * Everything the other network's RPC said is cleared, because a balance from one chain is a
     * lie beside another's name. The address and the Activity list survive the switch: the address
     * is the same wallet on every network, and the list is the account's history, not one
     * network's.
     */
    fun selectChainNetwork(choice: ChainNetworkChoice) {
        val current = signedInState?.wallet?.chain ?: return
        if (current.network == choice) return
        signedIn {
            it.copy(
                wallet = it.wallet.copy(
                    chain = it.wallet.chain.copy(
                        network = choice,
                        balance = null,
                        error = null,
                        prepared = null,
                        prepareError = null,
                        sendError = null,
                    ),
                ),
            )
        }
    }

    /**
     * Wallet 0's balance on the chosen network, by explicit refresh only.
     *
     * A pull, never a poll: the balance shown is whatever the last refresh answered, and an error
     * stays on screen because "could not check" and "zero" are different facts.
     */
    fun refreshChainBalance() {
        val live = session ?: return
        val choice = signedInState?.wallet?.chain?.network ?: return
        signedIn { it.copy(wallet = it.wallet.copy(chain = it.wallet.chain.copy(error = null))) }
        viewModelScope.launch {
            val root = live.client.keyStore.root
            if (root == null) {
                signedIn {
                    it.copy(wallet = it.wallet.copy(chain = it.wallet.chain.copy(address = null, error = noRootOnDevice)))
                }
                return@launch
            }
            val wallet = EvmWallet.fromRoot(root, 0)
            try {
                val balance = ChainClient(networkOf(choice)).getBalance(wallet.address())
                // Only the network still on screen may answer: a balance from a network the user
                // has since switched away from is a lie beside the other network's name.
                if (signedInState?.wallet?.chain?.network != choice) return@launch
                signedIn {
                    it.copy(
                        wallet = it.wallet.copy(
                            chain = it.wallet.chain.copy(
                                address = wallet.addressChecksummed(),
                                balance = balance,
                            ),
                        ),
                    )
                }
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (failure: Exception) {
                if (signedInState?.wallet?.chain?.network != choice) return@launch
                signedIn {
                    it.copy(
                        wallet = it.wallet.copy(
                            chain = it.wallet.chain.copy(
                                address = wallet.addressChecksummed(),
                                error = readable(failure),
                            ),
                        ),
                    )
                }
            }
        }
    }

    /**
     * Builds one AVAX transfer from the RPC's own answers, and nothing else.
     *
     * Parse failures happen before a single RPC leaves: a bad recipient or a bad amount is a form
     * problem, and the network is not asked to confirm the shape of a text field. The fees, the
     * gas and the nonce are the three lines the confirm screen quotes, so all three are asked
     * before the prepared transaction exists — a prepared transaction with a guessed field is a
     * confirmation screen that lies about one of its lines.
     */
    fun prepareChainSend(recipient: String, amount: String) {
        val live = session ?: return
        val choice = signedInState?.wallet?.chain?.network ?: return
        signedIn {
            it.copy(wallet = it.wallet.copy(chain = it.wallet.chain.copy(prepared = null, prepareError = null)))
        }
        val to = try {
            parseAddress(recipient.trim())
        } catch (failure: Exception) {
            signedIn {
                it.copy(wallet = it.wallet.copy(chain = it.wallet.chain.copy(prepareError = failure.message)))
            }
            return
        }
        val value = parseAvaxAmount(amount) ?: run {
            signedIn {
                it.copy(
                    wallet = it.wallet.copy(
                        chain = it.wallet.chain.copy(prepareError = "The amount is not a valid AVAX amount, e.g. 1.5"),
                    ),
                )
            }
            return
        }
        val root = live.client.keyStore.root
        if (root == null) {
            signedIn { it.copy(wallet = it.wallet.copy(chain = it.wallet.chain.copy(prepareError = noRootOnDevice))) }
            return
        }
        viewModelScope.launch {
            try {
                val wallet = EvmWallet.fromRoot(root, 0)
                val client = ChainClient(networkOf(choice))
                val fees = client.getFees()
                val gasLimit = client.estimateGas(to, value, ByteArray(0))
                val nonce = client.getNonce(wallet.address())
                if (signedInState?.wallet?.chain?.network != choice) return@launch
                signedIn {
                    it.copy(
                        wallet = it.wallet.copy(
                            chain = it.wallet.chain.copy(
                                prepared = PreparedChainTx(
                                    network = choice,
                                    chainId = networkOf(choice).chainId,
                                    from = wallet.addressChecksummed(),
                                    to = eip55(to),
                                    valueWei = value,
                                    maxPriorityFeePerGas = fees.maxPriorityFeePerGas,
                                    maxFeePerGas = fees.maxFeePerGas,
                                    gasLimit = gasLimit,
                                    nonce = nonce,
                                ),
                            ),
                        ),
                    )
                }
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (failure: Exception) {
                if (signedInState?.wallet?.chain?.network != choice) return@launch
                signedIn {
                    it.copy(wallet = it.wallet.copy(chain = it.wallet.chain.copy(prepareError = readable(failure))))
                }
            }
        }
    }

    /** Records the mainnet acknowledgement: real money, said before the button that spends unlocks. */
    fun setChainAcknowledged(acknowledged: Boolean) {
        signedIn {
            it.copy(wallet = it.wallet.copy(chain = it.wallet.chain.copy(mainnetAcknowledged = acknowledged)))
        }
    }

    /** Drops a prepared transaction without sending it. */
    fun cancelChainPrepare() {
        signedIn {
            it.copy(
                wallet = it.wallet.copy(
                    chain = it.wallet.chain.copy(prepared = null, prepareError = null, mainnetAcknowledged = false),
                ),
            )
        }
    }

    /**
     * Signs and broadcasts exactly the transaction the confirm screen displayed.
     *
     * Every field is re-derived from the prepared struct the screen sent back: the recipient is
     * re-parsed (an EIP-55 checksum that survived a tamper fails here), the sender is checked
     * against this device's own wallet 0, and the chain id comes from the named network — never
     * from a field a screen could have edited. What is signed is what was shown (spec #40).
     *
     * The record is written at broadcast, not at settle: a crash mid-tracking loses the ending,
     * never the fact that value left. Acceptance is what the broadcast reports — the tracker below
     * is the only thing that can say CONFIRMED (spec #41).
     */
    fun confirmChainSend(tx: PreparedChainTx) {
        val live = session ?: return
        if (signedInState?.wallet?.chain?.tracking != null) return
        val to = try {
            parseAddress(tx.to.trim())
        } catch (failure: Exception) {
            signedIn { it.copy(wallet = it.wallet.copy(chain = it.wallet.chain.copy(sendError = failure.message))) }
            return
        }
        val root = live.client.keyStore.root
        if (root == null) {
            signedIn { it.copy(wallet = it.wallet.copy(chain = it.wallet.chain.copy(sendError = noRootOnDevice))) }
            return
        }
        val wallet = EvmWallet.fromRoot(root, 0)
        // The `from` on screen must be this device's wallet 0: a prepared transaction carried over
        // from another device, or an older derivation, is refused rather than signed with the
        // wrong key for the right-looking screen.
        if (tx.from != wallet.addressChecksummed()) {
            signedIn {
                it.copy(
                    wallet = it.wallet.copy(
                        chain = it.wallet.chain.copy(
                            sendError = "The prepared transaction names a different sender; prepare it again here",
                        ),
                    ),
                )
            }
            return
        }
        val network = networkOf(tx.network)
        val body = Eip1559Tx(
            chainId = network.chainId,
            nonce = tx.nonce,
            maxPriorityFeePerGas = tx.maxPriorityFeePerGas,
            maxFeePerGas = tx.maxFeePerGas,
            gasLimit = tx.gasLimit,
            to = to,
            value = tx.valueWei,
            data = ByteArray(0),
        )
        viewModelScope.launch {
            try {
                val signedTx = body.sign(wallet)
                val txHash = ChainClient(network).broadcast(signedTx)

                live.trackedTxs.add(
                    0,
                    TxRecord(
                        txHash = signedTx.txHash.copyOf(),
                        chainId = body.chainId,
                        to = to,
                        valueWei = body.value,
                        feeWei = body.maxFeePerGas.multiply(BigInteger.valueOf(body.gasLimit)),
                        gasLimit = body.gasLimit,
                        atUnix = System.currentTimeMillis() / 1000,
                        outcome = "PENDING",
                        block = null,
                        gasUsed = null,
                    ),
                )
                // Acceptance, not confirmation — the surface's tracking line says BROADCAST and
                // only the tracker below can upgrade that.
                signedIn {
                    it.copy(
                        wallet = it.wallet.copy(
                            chain = it.wallet.chain.copy(
                                tracking = TrackingChainTx(txHash, "BROADCAST"),
                                prepared = null,
                                prepareError = null,
                                mainnetAcknowledged = false,
                                activity = chainActivity(live),
                            ),
                        ),
                    )
                }
                // The vault write is a save of what already happened, so its failure is a banner
                // rather than a send error: the transaction is out, and the user should know the
                // record of it may not survive the next launch.
                try {
                    live.persist()
                } catch (failure: Exception) {
                    signedIn { it.copy(failure = "The vault could not be saved: ${readable(failure)}") }
                }

                val result = try {
                    ChainClient(network).track(
                        txHash,
                        TrackOptions(onState = { state ->
                            signedIn { current ->
                                val tracking = current.wallet.chain.tracking
                                if (tracking != null && tracking.txHash == txHash) {
                                    current.copy(
                                        wallet = current.wallet.copy(
                                            chain = current.wallet.chain.copy(tracking = tracking.copy(state = state)),
                                        ),
                                    )
                                } else {
                                    current
                                }
                            }
                        }),
                    )
                } catch (failure: Exception) {
                    // An endpoint that cannot be asked at all is still an unresolved ending, and
                    // EXPIRED is the honest name for one this client watched for its whole
                    // deadline.
                    TrackResult(TrackOutcome.Expired, txHash = txHash)
                }
                settleChainTx(live, txHash, result)
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (failure: Exception) {
                signedIn { it.copy(wallet = it.wallet.copy(chain = it.wallet.chain.copy(sendError = readable(failure)))) }
            }
        }
    }

    /**
     * A tracker finished: the record's ending is written where the vault will next read it, and
     * the ending says itself — including the unresolved one.
     */
    private suspend fun settleChainTx(live: MigoSession, txHash: String, result: TrackResult) {
        val outcome = when (result.outcome) {
            TrackOutcome.Confirmed -> "CONFIRMED"
            TrackOutcome.Reverted -> "REVERTED"
            TrackOutcome.Dropped -> "DROPPED"
            TrackOutcome.Expired -> "EXPIRED"
        }
        val shortHash = txHash.take(16)
        val index = live.trackedTxs.indexOfFirst { "0x" + hexOf(it.txHash) == txHash }
        if (index >= 0) {
            val old = live.trackedTxs.removeAt(index)
            live.trackedTxs.add(
                index,
                TxRecord(
                    old.txHash,
                    old.chainId,
                    old.to,
                    old.valueWei,
                    old.feeWei,
                    old.gasLimit,
                    old.atUnix,
                    outcome,
                    result.blockNumber ?: old.block,
                    result.gasUsed ?: old.gasUsed,
                ),
            )
        }
        signedIn { current ->
            val tracking = current.wallet.chain.tracking
            current.copy(
                wallet = current.wallet.copy(
                    chain = current.wallet.chain.copy(
                        tracking = if (tracking?.txHash == txHash) null else tracking,
                        activity = chainActivity(live),
                    ),
                ),
                failure = when (outcome) {
                    "CONFIRMED" -> current.failure
                    "EXPIRED" -> "AVAX send expired without an answer · $shortHash"
                    else -> "AVAX send ${outcome.lowercase()} · $shortHash"
                },
            )
        }
        try {
            live.persist()
        } catch (cancelled: CancellationException) {
            throw cancelled
        } catch (failure: Exception) {
            signedIn { it.copy(failure = "The vault could not be saved: ${readable(failure)}") }
        }
    }

    /** The Alerts inbox read. */
    fun loadAlerts() {
        val live = session ?: return
        signedIn { it.copy(alerts = it.alerts.copy(loading = true)) }
        viewModelScope.launch {
            try {
                val items = live.client.notifications.listNotifications(50)
                signedIn { it.copy(alerts = it.alerts.copy(loading = false, items = items)) }
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (failure: Exception) {
                signedIn { it.copy(alerts = it.alerts.copy(loading = false), failure = readable(failure)) }
            }
        }
    }

    /**
     * Marks everything at or before the newest rendered item read.
     *
     * One watermark call, exactly like the web client: a notification landing mid-flight is left for
     * the next acknowledgement rather than raced.
     */
    fun markAllRead() {
        val live = session ?: return
        val newest = signedInState?.alerts?.items?.maxOfOrNull { it.at } ?: return
        signedIn { it.copy(alerts = it.alerts.copy(acknowledging = true)) }
        viewModelScope.launch {
            try {
                live.client.notifications.acknowledgeNotifications(newest)
                loadAlerts()
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (failure: Exception) {
                signedIn { it.copy(alerts = it.alerts.copy(acknowledging = false), failure = readable(failure)) }
            }
        }
    }

    // The lazy-load questions: has a section's first read landed? A section answers false while
    // its read is in flight or was never started, so re-entry re-reads only what never arrived.
    private val signedInState: AppState.SignedIn?
        get() = _state.value as? AppState.SignedIn

    private fun spaceLoaded(): Boolean = signedInState?.space?.loading == false && signedInState?.space?.rows?.isNotEmpty() == true

    private fun friendsLoaded(): Boolean = signedInState?.friends?.loading == false && signedInState?.friends?.entries?.isNotEmpty() == true

    private fun walletLoaded(): Boolean = signedInState?.wallet?.balance != null

    private fun alertsLoaded(): Boolean = signedInState?.alerts?.loading == false && signedInState?.alerts?.items?.isNotEmpty() == true

    // --- lifecycle ---

    /**
     * Closes the socket when the last screen goes away for good.
     *
     * The close runs on the application's scope, not [viewModelScope]: that one is already cancelled
     * by the time this is called, so a coroutine launched there would never run and the connection
     * would be left for the server to time out. Nothing is destroyed here -- this is a process
     * shutting down, not somebody signing out.
     */
    override fun onCleared() {
        detach()
        val leaving = session
        session = null
        val app = getApplication<MigoApplication>()
        leaving?.let { closing ->
            app.scope.launch {
                try {
                    closing.close()
                } catch (_: Exception) {
                    // The process is going away; there is nobody to tell.
                }
            }
        }
    }

    // --- internals ---

    private suspend fun bootstrap() {
        val stored = settings.current()
        val fallback = stored.serverEndpoint
        try {
            val resumed = MigoSession.resumeStored(getApplication(), BuildConfig.VERSION_NAME, hooks())
            if (resumed == null) {
                _state.value = AppState.SignedOut(fallback)
            } else {
                attach(resumed)
            }
        } catch (cancelled: CancellationException) {
            throw cancelled
        } catch (failure: Exception) {
            _state.value = AppState.SignedOut(fallback, failure = readable(failure))
        }
    }

    private fun attach(opened: MigoSession) {
        session = opened
        names[opened.client.accountId] = opened.username
        _state.value = AppState.SignedIn(
            username = opened.username,
            accountId = opened.client.accountId,
            connection = opened.client.connectionState,
        )
        subscriptions.add(opened.client.onMessage { arrived(it) })
        subscriptions.add(opened.client.onDeletion { removed(it) })
        subscriptions.add(opened.client.onTyping { typing(it) })
        // A pushed notification is a cue to reconcile every inbox-shaped surface, and a friend
        // event a cue to re-read the graph -- the same reconcile-don't-trust rule each section
        // applies on its own refresh button.
        subscriptions.add(opened.client.onNotification { pushed(it) })
        subscriptions.add(opened.client.onFriendEvent {
            if ((_state.value as? AppState.SignedIn)?.section == AppState.Section.FRIENDS) loadFriends()
        })
        refreshConversations()
        // The wallet's combined read also fills the banner's $MIG balance, so the session starts
        // with it -- the desktop client issues its wallet command at sign-in for the same reason.
        loadWallet()
        // The AVAX Activity list rides the session's own tracked records: no read, no server call
        // -- the vault already answered it when it handed back the session.
        signedIn {
            it.copy(wallet = it.wallet.copy(chain = it.wallet.chain.copy(activity = chainActivity(opened))))
        }
    }

    /** A pushed notification: the Feed stream and the Alerts inbox re-read, the digest follows. */
    private fun pushed(event: NotificationEvent) {
        val current = _state.value as? AppState.SignedIn ?: return
        when (current.section) {
            AppState.Section.FEED -> loadSpace()
            AppState.Section.ALERTS -> loadAlerts()
            else -> Unit
        }
    }

    /** An inbox row as a stream row: category and headline from the kind's own words. */
    private fun inboxRow(item: InboxItem): ActivityRow {
        val kind = item.kind
        val spaced = kind.replace('_', ' ').replaceFirstChar { it.uppercase() }
        val category = when {
            kind.contains("friend") -> ActivityCategory.SOCIAL
            kind.contains("gift") || kind.contains("coin") || kind.contains("ledger") -> ActivityCategory.ECONOMY
            kind.contains("game") -> ActivityCategory.GAMES
            kind.contains("room") -> ActivityCategory.ROOMS
            else -> ActivityCategory.SOCIAL
        }
        return ActivityRow(key = "notif-" + item.id.value, category = category, title = item.title ?: spaced, at = item.at)
    }

    /** A ledger line as a stream row: the money-side fact, signed by its reason. */
    private fun ledgerRow(entry: LedgerEntryWire): ActivityRow {
        val credits = entry.reason == "grant" ||
            entry.reason == "gift_reputation" ||
            entry.reason == "refund" ||
            entry.reason == "game_payout"
        val signed = if (credits) "+" else "-"
        val label = entry.reason.replace('_', ' ').replaceFirstChar { it.uppercase() }
        return ActivityRow(
            key = "ledger-" + entry.txId.value,
            category = ActivityCategory.ECONOMY,
            title = label + " " + signed + entry.amount + " MIG",
            at = entry.at,
        )
    }

    private fun detach() {
        subscriptions.forEach { it.cancel() }
        subscriptions.clear()
    }

    private fun hooks() = SessionHooks(
        onState = { next -> signedIn { it.copy(connection = next) } },
        onError = { failure ->
            // Only worth a banner once the client has given up on the current attempt; it keeps
            // retrying on its own, and a message per attempt would flicker.
            if ((_state.value as? AppState.SignedIn)?.connection == ConnectionState.Reconnecting) {
                signedIn { it.copy(failure = readable(failure)) }
            }
        },
    )

    private fun arrived(message: IncomingMessage) {
        val live = session ?: return
        val mine = message.senderId == live.client.accountId
        val body = describe(message.content)
        val entry = ChatMessage(
            messageId = message.messageId,
            seq = message.seq,
            mine = mine,
            author = if (mine) live.username else names[message.senderId] ?: shortId(message.senderId),
            text = body.text,
            at = message.createdAt,
            unsupported = body.placeholder,
        )
        val onScreen = opened(message.conversationId)
        signedIn { current ->
            val chat = current.open
            val here = chat != null && chat.conversationId == message.conversationId
            current.copy(
                open = if (here) chat.copy(messages = merge(chat.messages, entry)) else chat,
                conversations = bumped(
                    current.conversations,
                    message.conversationId,
                    body.text,
                    message.createdAt,
                    unread = !mine && !here,
                ),
            )
        }
        if (onScreen && !mine) {
            viewModelScope.launch {
                try {
                    live.client.messaging.sendReceipt(
                        message.conversationId,
                        ReceiptKind.Read,
                        message.seq,
                    )
                } catch (cancelled: CancellationException) {
                    throw cancelled
                } catch (_: Exception) {
                    // A read receipt is a courtesy, not a delivery guarantee.
                }
            }
        }
    }

    private fun removed(deletion: MessageDeletion) {
        inChat(deletion.conversationId) { chat ->
            chat.copy(messages = chat.messages.filterNot { it.messageId == deletion.messageId })
        }
    }

    private fun typing(event: TypingEvent) {
        val who = event.userId ?: return
        inChat(event.conversationId) { chat ->
            val next = when (event.state) {
                TypingState.Start -> chat.typing + who
                else -> chat.typing - who
            }
            chat.copy(typing = next)
        }
    }

    private suspend fun learnNames(live: MigoSession, summaries: List<ConversationSummary>) {
        val wanted = LinkedHashSet<Id>()
        for (summary in summaries) {
            summary.members?.forEach { if (!names.containsKey(it)) wanted.add(it) }
            summary.lastMessage?.let { if (!names.containsKey(it.senderId)) wanted.add(it.senderId) }
        }
        if (wanted.isEmpty()) return
        try {
            for (profile in live.client.profile.fetch(wanted.toList().take(PROFILE_BATCH))) {
                names[profile.userId] = profile.displayName.ifBlank { profile.username }
            }
        } catch (cancelled: CancellationException) {
            throw cancelled
        } catch (_: Exception) {
            // Short ids are a worse label than a display name, not a broken screen.
        }
    }

    /**
     * One list row.
     *
     * The preview is a label rather than the last message's text, and deliberately so: the list
     * endpoint returns that message as an envelope, and opening it would mean running the ratchet
     * for a conversation nobody has opened, out of order, once per row. The text appears when the
     * chat is opened and the messages are decrypted in sequence.
     */
    private fun row(live: MigoSession, summary: ConversationSummary): ConversationRow {
        val preview = if (summary.lastMessage != null) "Encrypted message" else null
        return ConversationRow(
            conversationId = summary.conversationId,
            title = title(live, summary),
            kind = summary.kind,
            preview = preview,
            unread = (summary.lastSeq - summary.readSeq).coerceAtLeast(0),
            updatedAt = summary.lastMessage?.createdAt ?: 0L,
        )
    }

    /**
     * The best name this client can give a conversation.
     *
     * A stored title wins. Failing that a direct conversation is named after the other member, which
     * is what a person expects and what the server cannot do for us: the title would have to be the
     * same for both sides. Anything else falls back to a short id, which is at least stable.
     */
    private fun title(live: MigoSession, summary: ConversationSummary): String {
        summary.title?.takeIf { it.isNotBlank() }?.let { return it }
        val others = summary.members?.filter { it != live.client.accountId } ?: emptyList()
        if (summary.kind == ConversationKind.Direct && others.size == 1) {
            return names[others[0]] ?: shortId(others[0])
        }
        if (others.isNotEmpty()) {
            return others.joinToString(limit = 3) { names[it] ?: shortId(it) }
        }
        return shortId(summary.conversationId)
    }

    private fun opened(conversationId: Id): Boolean =
        (_state.value as? AppState.SignedIn)?.open?.conversationId == conversationId

    private fun bump(conversationId: Id, preview: String, at: Long, unread: Boolean) {
        signedIn { current ->
            current.copy(
                conversations = bumped(current.conversations, conversationId, preview, at, unread),
            )
        }
    }

    /**
     * Moves a conversation to the top with a fresh preview.
     *
     * A row for a conversation not in the list is not invented here. The list is what the server said
     * it was, and a message in an unlisted conversation means the list is stale, which a refresh
     * fixes with the title and membership a fabricated row would not have.
     */
    private fun bumped(
        rows: List<ConversationRow>,
        conversationId: Id,
        preview: String,
        at: Long,
        unread: Boolean,
    ): List<ConversationRow> {
        val index = rows.indexOfFirst { it.conversationId == conversationId }
        if (index < 0) return rows
        val existing = rows[index]
        // An event older than what the row already shows still counts toward the unread badge, but it
        // must not overwrite a newer preview: reconnects deliver out of order, and a row that jumped
        // backwards would look like the conversation had un-happened.
        val newer = at >= existing.updatedAt
        val updated = existing.copy(
            preview = if (newer) preview else existing.preview,
            updatedAt = maxOf(existing.updatedAt, at),
            unread = if (unread) existing.unread + 1 else 0,
        )
        val remaining = ArrayList<ConversationRow>(rows.size)
        remaining.add(updated)
        rows.forEachIndexed { position, row -> if (position != index) remaining.add(row) }
        return remaining
    }

    /**
     * Adds a message in sequence order, replacing any earlier copy of the same one.
     *
     * Both halves matter. The server may echo a send back to its own sender, and catch-up may overlap
     * what a live event already delivered, so an append-only list would show duplicates. And events
     * do not arrive in order after a reconnect, so position comes from [ChatMessage.seq] rather than
     * from arrival.
     */
    private fun merge(existing: List<ChatMessage>, entry: ChatMessage): List<ChatMessage> {
        val without = existing.filterNot { it.messageId == entry.messageId }
        val at = without.indexOfFirst { it.seq > entry.seq }
        val next = ArrayList<ChatMessage>(without.size + 1)
        if (at < 0) {
            next.addAll(without)
            next.add(entry)
        } else {
            next.addAll(without.subList(0, at))
            next.add(entry)
            next.addAll(without.subList(at, without.size))
        }
        return next
    }

    /**
     * What to draw for a body, and whether this build understood it.
     *
     * Media, voice notes and reactions decode correctly here but have no screen yet, so they are
     * labelled rather than rendered. A newer peer's content type reaches [Content.Unsupported] and is
     * labelled too, which is the whole reason that variant exists instead of a decode failure.
     */
    private fun describe(content: Content): Rendered = when (content) {
        is Content.Text -> Rendered(content.text, placeholder = false)
        is Content.MediaRef -> Rendered(content.caption?.takeIf { it.isNotBlank() } ?: "Photo")
        is Content.VoiceNoteRef -> Rendered("Voice note")
        is Content.Reaction -> Rendered(content.emoji)
        is Content.ControlEvent -> Rendered("Updated the conversation")
        is Content.Unsupported -> Rendered("Message this version cannot show")
    }

    /** A body reduced to what a row shows: the text, and whether it stands in for something. */
    private class Rendered(val text: String, val placeholder: Boolean = true)

    /** The first eight characters of an id: enough to tell two apart, short enough to read. */
    private fun shortId(id: Id): String = id.value.take(8)

    /**
     * Every error shown to a person, and every error not shown.
     *
     * The SDK's messages are already written for a user, and the server's are its `public_message()`,
     * which is the only string it puts on the wire (section 161). What is refused is a bare exception
     * class name, which tells somebody nothing and is how a stack detail ends up on a screen.
     */
    /**
     * A failure as the user reads it.
     *
     * The two auth-side refusals a person can actually act on get plain words instead of the
     * server's wire vocabulary, which the message either carries bare (`RATE_LIMITED`) or not
     * at all (`CAPTCHA_REQUIRED`): a wait is a wait, and "the server asked for a human check"
     * means "wait a moment and try again" on a build with no captcha UI yet.
     */
    private fun readable(failure: Throwable): String {
        if (failure is com.migo.core.net.RestError.Server) {
            if (failure.symbol == "AUTH_LOCKED") {
                val seconds = failure.retryAfterMs?.div(1000)
                return if (seconds != null && seconds > 0) {
                    "Account temporarily locked. Try again in $seconds seconds."
                } else {
                    "Account temporarily locked. Try again later."
                }
            }
            if (failure.symbol == "RATE_LIMITED") {
                val seconds = failure.retryAfterMs?.div(1000)
                return if (seconds != null && seconds > 0) {
                    "Too many requests. Wait $seconds seconds and try again."
                } else {
                    "Too many requests. Wait a moment and try again."
                }
            }
            if (failure.symbol == "CAPTCHA_REQUIRED") {
                return "Too many failed attempts from this network. Wait a moment and try again."
            }
            if (failure.symbol == "INVALID_CAPTCHA" || failure.symbol == "CAPTCHA_EXPIRED") {
                return "The human check was wrong or expired. Start the sign-up again."
            }
            // An answer without the envelope -- a proxy's error page, or a framework-level
            // rejection this server never wrote -- has no symbol and no public message. The
            // HTTP status is the one fact it does carry, and it is all a person can act on;
            // hiding it behind "something went wrong" would make two different failures
            // indistinguishable.
            if (failure.symbol.isBlank() && failure.publicMessage.isBlank()) {
                return "The server answered with an error (HTTP ${failure.code}). Try again."
            }
            failure.publicMessage.takeIf { it.isNotBlank() }?.let { return it }
        }
        return failure.message?.takeIf { it.isNotBlank() } ?: "Something went wrong. Try again."
    }

    private inline fun signedOut(transform: (AppState.SignedOut) -> AppState.SignedOut) {
        _state.update { current ->
            if (current is AppState.SignedOut) transform(current) else current
        }
    }

    private inline fun signedIn(transform: (AppState.SignedIn) -> AppState.SignedIn) {
        _state.update { current ->
            if (current is AppState.SignedIn) transform(current) else current
        }
    }

    private inline fun inChat(conversationId: Id, transform: (ChatState) -> ChatState) {
        signedIn { current ->
            val chat = current.open
            if (chat == null || chat.conversationId != conversationId) {
                current
            } else {
                current.copy(open = transform(chat))
            }
        }
    }

    private companion object {
        /** How long the search field must be quiet before its query reaches the wire. */
        const val SEARCH_DEBOUNCE_MS = 300L
        /** One screen of conversations, and more on demand rather than a list nobody scrolls. */
        const val CONVERSATION_PAGE = 50L

        /** Catch up from the beginning: there is no local store to have a high-water mark in. */
        const val HISTORY_FROM = 0L

        /** Enough history to open a chat on, bounded so a long conversation does not stall the open. */
        const val HISTORY_LIMIT = 100L

        /** The profile endpoint takes a batch; this keeps one list refresh to one request. */
        const val PROFILE_BATCH = 50
    }
}
