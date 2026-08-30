package com.migo.app

import android.app.Application
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import com.migo.app.model.ActivityCategory
import com.migo.app.model.ActivityRow
import com.migo.app.model.AppState
import com.migo.app.model.ChatMessage
import com.migo.app.model.ChatState
import com.migo.app.model.ConversationRow
import com.migo.app.session.MigoSession
import com.migo.app.session.SessionHooks
import com.migo.core.ConnectionState
import com.migo.core.crypto.Content
import com.migo.core.domain.IncomingMessage
import com.migo.core.domain.MessageDeletion
import com.migo.core.domain.SendOptions
import com.migo.core.domain.Subscription
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
import com.migo.core.wire.Id
import com.migo.core.wire.WireError
import com.migo.core.wire.parseId
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
            current.copy(
                open = ChatState(conversationId = conversationId, title = title, loading = true),
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
     * tour through the bottom bar costs one read per section, not one per visit.
     */
    fun selectSection(section: AppState.Section) {
        signedIn { it.copy(section = section) }
        when (section) {
            AppState.Section.HOME -> if (!homeLoaded()) loadHome()
            AppState.Section.ROOMS -> if (signedInState?.rooms?.rooms == null) loadRooms()
            AppState.Section.SPACE -> if (!spaceLoaded()) loadSpace()
            AppState.Section.FRIENDS -> if (!friendsLoaded()) loadFriends()
            AppState.Section.SEARCH -> Unit
            AppState.Section.WALLET -> if (!walletLoaded()) loadWallet()
            AppState.Section.ALERTS -> if (!alertsLoaded()) loadAlerts()
            AppState.Section.CHATS, AppState.Section.PROFILE -> Unit
        }
    }

    /** The Home dashboard's one combined read; a block that fails alone renders its empty. */
    fun loadHome() {
        val live = session ?: return
        signedIn { it.copy(home = it.home.copy(loading = true)) }
        viewModelScope.launch {
            val wallet = runCatching { live.client.economy.getBalance().balance }.getOrNull()
            val suggested = runCatching { live.client.social.suggestions(6) }.getOrDefault(emptyList())
            val inbox = runCatching { live.client.notifications.listNotifications(5) }.getOrDefault(emptyList())
            val leaders = runCatching { live.client.economy.getLeaderboard("xp", 3) }.getOrDefault(emptyList())
            val trending = runCatching {
                live.client.rooms.list(20).rooms.sortedByDescending { it.onlineCount }.take(5)
            }.getOrDefault(emptyList())
            signedIn {
                it.copy(home = it.home.copy(loading = false, balance = wallet, suggestions = suggested, notifications = inbox, leaders = leaders, trending = trending))
            }
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

    /** Projects a join reply into the conversation list, the same shape a started chat notes. */
    private fun noteRoom(joined: RoomJoinResponse) {
        val fresh = ConversationRow(
            conversationId = joined.conversationId,
            title = joined.room.name,
            kind = ConversationKind.Room,
            preview = null,
            unread = 0,
            updatedAt = 0,
        )
        signedIn { current ->
            val absent = current.conversations.none { it.conversationId == fresh.conversationId }
            current.copy(conversations = if (absent) listOf(fresh) + current.conversations else current.conversations)
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

    private fun homeLoaded(): Boolean = signedInState?.home?.loading == false && signedInState?.home?.notifications?.isNotEmpty() == true

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
        loadHome()
    }

    /** A pushed notification: the Space stream and the Alerts inbox re-read, the digest follows. */
    private fun pushed(event: NotificationEvent) {
        val current = _state.value as? AppState.SignedIn ?: return
        when (current.section) {
            AppState.Section.SPACE -> loadSpace()
            AppState.Section.ALERTS -> loadAlerts()
            AppState.Section.HOME -> loadHome()
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
    private fun readable(failure: Throwable): String =
        failure.message?.takeIf { it.isNotBlank() } ?: "Something went wrong. Try again."

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
