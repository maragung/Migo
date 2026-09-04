package com.migo.app.model

import com.migo.core.ConnectionState
import com.migo.core.net.DeviceSummary
import com.migo.core.net.WalletSummary
import com.migo.core.protocol.BadgeWire
import com.migo.core.protocol.ConversationKind
import com.migo.core.protocol.GiftListing
import com.migo.core.protocol.InboxItem
import com.migo.core.protocol.LedgerEntryWire
import com.migo.core.protocol.ProgressionWire
import com.migo.core.protocol.RankWire
import com.migo.core.protocol.RelationshipEntry
import com.migo.core.protocol.RoomRole
import com.migo.core.protocol.RoomSummary
import com.migo.core.protocol.SuggestedUser
import com.migo.core.protocol.UserProfile
import com.migo.core.store.ServerEndpoint
import com.migo.core.wire.Id
import java.math.BigInteger

/**
 * Everything the interface draws, as one immutable value.
 *
 * The screens in this module are functions of this type and nothing else: no screen reads a
 * `MigoClient`, holds a coroutine, or keeps a `remember`ed copy of anything that also lives here.
 * That is the same discipline the desktop client follows (a read-only context plus a command buffer),
 * and it is worth the extra type because the alternative is state that exists in two places -- the
 * ratchet advanced but the bubble still says "sending", or a conversation list that disagrees with
 * the conversation open on top of it.
 *
 * It also means the whole interface can be driven from a test or a preview by constructing a value,
 * with no connection and no key store anywhere.
 */
sealed interface AppState {
    /**
     * The vault is being opened and a stored sign-in resumed.
     *
     * A distinct state rather than a flag on [SignedOut], because showing the sign-in form during it
     * would flash a form at a signed-in user on every cold start.
     */
    data object Starting : AppState

    /** Nobody is signed in on this device, or the last session could not be resumed. */
    data class SignedOut(
        /**
         * The server to talk to, as the structured record the user picked.
         *
         * The form is initialised with the persisted choice (or the dev default on a
         * fresh install) and re-emits a new record on every "Use this server" click
         * through [com.migo.app.AppViewModel.setServerEndpoint]. The form holds the
         * typed text in its own local state, so this field is always a valid
         * [ServerEndpoint] -- a partial host (one that does not satisfy
         * [ServerEndpoint.init]) never reaches here.
         */
        val serverEndpoint: ServerEndpoint,
        /** Username or email, kept across a failed attempt so it does not have to be retyped. */
        val identifier: String = "",
        /** True while a register or sign-in call is in flight; the form is disabled. */
        val busy: Boolean = false,
        /** What went wrong last time, already reduced to something worth showing a person. */
        val failure: String? = null,
    ) : AppState

    /**
     * Signed in. The conversation list is always present; [open] is the chat on top of it.
     *
     * The [section] is which destination the shell is showing — the new-ui-02 model's left panel:
     * Chats, Friends, Rooms, Games and Feed as the system tabs, with the panels (Alerts, Search,
     * Wallet, Profile) opened from the banner's avatar menu and covering the screen. Each section's
     * data lives in its own holder below, loaded on first entry and reloaded on demand; a section
     * never yet visited holds nulls, and its screen draws its skeleton.
     */
    data class SignedIn(
        val username: String,
        val accountId: Id,
        val connection: ConnectionState,
        val conversations: List<ConversationRow> = emptyList(),
        /** True while the first page of conversations is loading. */
        val loading: Boolean = false,
        /** The conversation the user is reading, or null when a section is on top. */
        val open: ChatState? = null,
        /** A transient failure banner: a send that did not go, a page that did not load. */
        val failure: String? = null,
        /** The destination on screen; Main (the friends list) is where a session starts, as on every client. */
        val section: Section = Section.FRIENDS,
        /**
         * The left panel's own tab, in the new-ui-02 model: the four system tabs drive the strip,
         * while the panels (Alerts, Search, Wallet, Profile) cover the screen the way a chat does.
         * A panel's back returns here, so covering the shell never disturbs what the strip shows.
         */
        val stripSection: Section = Section.FRIENDS,
        val rooms: RoomsState = RoomsState(),
        val space: SpaceState = SpaceState(),
        val friends: FriendsState = FriendsState(),
        val search: SearchState = SearchState(),
        val wallet: WalletState = WalletState(),
        val alerts: AlertsState = AlertsState(),
        val devices: DevicesState = DevicesState(),
        val backup: BackupState = BackupState(),
        val profileEdit: ProfileEditState = ProfileEditState(),
        val accountSecurity: AccountSecurityState = AccountSecurityState(),
    ) : AppState

    /**
     * The shell's destinations. The first five are the reference's system tabs, in strip order; the
     * rest are the panels the banner's avatar menu opens — they cover the screen rather than joining
     * the strip, which is the new-ui-02 model's phone story.
     */
    enum class Section {
        CHATS, FRIENDS, ROOMS, GAMES, FEED, ALERTS, SEARCH, WALLET, PROFILE;

        /** True for the four panels the banner's menu opens, which cover the strip rather than join it. */
        val isPanel: Boolean
            get() = this == ALERTS || this == SEARCH || this == WALLET || this == PROFILE
    }
}

/** The Rooms directory: the server's catalogue plus the browsing state around it. */
data class RoomsState(
    /** The page held; null until the first read lands. */
    val rooms: List<RoomSummary>? = null,
    /** The live query text, debounced by the view model before it reaches the wire. */
    val query: String = "",
    /** True while a page (first or refresh) is in flight. */
    val loading: Boolean = false,
    /** Room ids with a join in flight, so their rows can disable their buttons. */
    val joining: Set<Id> = emptySet(),
)

/**
 * The Feed tab's activity stream: the inbox and the wallet's statement merged, newest first.
 *
 * Rows are plain display values — icon kind, headline, time — rather than wire types, because a
 * stream row is a synthesis (a ledger line and a notification can describe the same gift) and the
 * merge happens once, in the view model, where the two sources meet.
 */
data class SpaceState(
    val rows: List<ActivityRow> = emptyList(),
    /** True while the durable halves (inbox + ledger) are being read. */
    val loading: Boolean = false,
)

/** One row of the activity stream. */
data class ActivityRow(
    val key: String,
    /** The category filter the row belongs to. */
    val category: ActivityCategory,
    val title: String,
    /** Unix milliseconds — the event's own time, or its arrival time for live-only sources. */
    val at: Long,
)

/** The stream's categories, each a filter over the merged rows. */
enum class ActivityCategory { SOCIAL, ROOMS, GAMES, ECONOMY }

/** The Friends section: the relationship graph, the suggestions, and the acting state. */
data class FriendsState(
    /** All relationships; the screens filter by kind the way the web client does. */
    val entries: List<RelationshipEntry> = emptyList(),
    val suggestions: List<SuggestedUser> = emptyList(),
    /** True while the graph is being read. */
    val loading: Boolean = false,
    /** Account ids with a social action in flight. */
    val busy: Set<Id> = emptySet(),
)

/** The Search section: one query's answers across every surface that can honestly answer. */
data class SearchState(
    /** The live query text; the view model debounces it before the wire. */
    val query: String = "",
    /** Username-prefix matches, or null before the first query. */
    val people: List<SuggestedUser>? = null,
    /** Room name/topic matches, or null before the first query. */
    val rooms: List<RoomSummary>? = null,
    /** True while a query is in flight. */
    val loading: Boolean = false,
)

/** The Wallet section: the caller's whole economy under one address. */
data class WalletState(
    val balance: Long? = null,
    val points: Long? = null,
    val ledger: List<LedgerEntryWire> = emptyList(),
    val progression: ProgressionWire? = null,
    val badges: List<BadgeWire> = emptyList(),
    val leaders: List<RankWire> = emptyList(),
    val catalogue: List<GiftListing> = emptyList(),
    /** True while the wallet's combined read is in flight. */
    val loading: Boolean = false,
    /** The AVAX side (§184): one network at a time, balance by explicit refresh. */
    val chain: ChainState = ChainState(),
    /**
     * The account's registered wallet addresses, as the server knows them. Null before the first
     * read lands — the same honest "not checked yet" the device list keeps, rather than an empty
     * list that would read as "this account has no addresses".
     */
    val registrations: List<WalletSummary>? = null,
    /** Wallet ids with an archive in flight, so only the pressed row shows its busy state. */
    val archiving: Set<String> = emptySet(),
    /** Why the last registrations read or archive could not answer. */
    val registrationFailure: String? = null,
)

/**
 * The two first-class Avalanche networks the wallet surface knows (§184).
 *
 * The user picks a network by name, never a URL — a self-supplied RPC is the classic way a wallet
 * gets shown a fake chain (spec #44). The pinned endpoint travels with the choice in the core
 * `Network` constants; this enum is the interface's word for the same two names.
 */
enum class ChainNetworkChoice(val label: String) {
    MAINNET("Avalanche C-Chain (mainnet)"),
    FUJI("Avalanche Fuji (testnet)"),
}

/**
 * The built transaction awaiting its confirmation, exactly as it was displayed.
 *
 * The send screen shows every field, and the confirm button hands this struct back verbatim; the
 * view model re-parses the recipient's EIP-55 checksum and checks the sender against this device's
 * wallet 0 before anything is signed, so what is signed is what was shown (spec #40).
 */
data class PreparedChainTx(
    val network: ChainNetworkChoice,
    val chainId: Long,
    /** The sender, EIP-55 checksummed. */
    val from: String,
    /** The recipient, EIP-55 checksummed — the string the user confirmed. */
    val to: String,
    /** The amount, wei. AVAX has 18 decimals. */
    val valueWei: BigInteger,
    val maxPriorityFeePerGas: BigInteger,
    val maxFeePerGas: BigInteger,
    val gasLimit: Long,
    val nonce: Long,
)

/** One in-flight send as the surface shows it: the explorer's handle and spec #41's own word. */
data class TrackingChainTx(
    val txHash: String,
    val state: String,
)

/** One tracked AVAX transaction as the Activity list draws it. */
data class ChainTxRow(
    /** The transaction hash, `0x`-prefixed hex. */
    val txHash: String,
    /** The network by name; an unknown chain id labels itself honestly. */
    val network: String,
    /** The recipient, EIP-55 checksummed. */
    val to: String,
    val valueWei: BigInteger,
    /** The fee ceiling that was confirmed, wei. */
    val feeWei: BigInteger,
    val gasLimit: Long,
    /** Unix milliseconds of the broadcast. */
    val at: Long,
    /** Spec #41's own word for where the transaction stands. */
    val outcome: String,
    /** The block that included the transaction, once one did. */
    val block: Long? = null,
    /** The gas the receipt says the block actually spent, once a receipt answered. */
    val gasUsed: BigInteger? = null,
)

/**
 * The AVAX wallet surface's state.
 *
 * A balance is a pull, never a poll: [balance] is whatever the last refresh the user asked for
 * answered, and an error stays on screen because "could not check" and "zero" are different facts
 * and only one of them should reassure anybody.
 */
data class ChainState(
    /** The network the surface is on. Mainnet is the default for *display*; the first send on it
     *  says what mainnet means before the button that spends unlocks. */
    val network: ChainNetworkChoice = ChainNetworkChoice.MAINNET,
    /** The wallet's EIP-55 address, once a read discovered it. Null until then, and null forever
     *  on a device without the root — the read's error carries that sentence instead. */
    val address: String? = null,
    /** The balance in wei, after the last refresh. */
    val balance: BigInteger? = null,
    /** Why the last refresh could not answer. */
    val error: String? = null,
    /** The built transaction awaiting confirmation. */
    val prepared: PreparedChainTx? = null,
    /** Why nothing could be built. */
    val prepareError: String? = null,
    /** Why a broadcast was refused. */
    val sendError: String? = null,
    /** The acknowledgement on a mainnet send: real money, said before the button unlocks. */
    val mainnetAcknowledged: Boolean = false,
    /** The in-flight send, from acceptance to its ending. */
    val tracking: TrackingChainTx? = null,
    /** This account's tracked transactions, newest first. */
    val activity: List<ChainTxRow> = emptyList(),
)

/** The Alerts section: the durable inbox and its read state. */
data class AlertsState(
    val items: List<InboxItem> = emptyList(),
    /** True while the inbox page is being read. */
    val loading: Boolean = false,
    /** True while a mark-all-read acknowledgement is in flight. */
    val acknowledging: Boolean = false,
)

/**
 * The Profile section's device list: the account-root security view.
 *
 * A device stays listed (as `revoked`) after it is removed, because "which phone was that" is a
 * question about the past as much as the present. [removing] holds device ids with a removal in
 * flight, so only the pressed row shows its busy state.
 */
data class DevicesState(
    /** The server's rows, or null before the first read lands. */
    val devices: List<DeviceSummary>? = null,
    /** True while the list is being read. */
    val loading: Boolean = false,
    /** Device ids with a removal in flight. */
    val removing: Set<String> = emptySet(),
    /** Why the last read or removal could not answer. */
    val failure: String? = null,
    /** The sentence the last removal answered with, shown once. */
    val notice: String? = null,
)

/**
 * The Profile panel's backup counter-state. Sealing a container is Argon2 work and a file write,
 * so the button that started it owes the person who pressed it a sentence when it lands — success
 * or failure — in the panel where they pressed it, not in the shell's banner.
 */
data class BackupState(
    /** True while a container is being sealed and written. */
    val sealing: Boolean = false,
    /** The sentence the last export answered with, shown once. */
    val notice: String? = null,
    /** Why the last export could not answer. */
    val failure: String? = null,
)

/**
 * The Profile panel's editable half: the account's own profile as the server holds it, and the
 * form's saving state. The profile rows are a read; this is the write side, and the two stay in
 * one holder because the form the person edits is primed from the same fetch that renders it.
 *
 * The privacy choices (showLastSeen / whoCanMessage / whoCanAdd) and the search switch are
 * absent-means-unchanged, exactly as on the web: the controls start as "leave as-is" and join the
 * save only once the person touches them, because the server never sends current values back and
 * a naive form would overwrite them with defaults.
 */
data class ProfileEditState(
    /** The caller's profile, or null before the first read lands. */
    val profile: UserProfile? = null,
    /** True while the profile is being read or the form is being saved. */
    val busy: Boolean = false,
    /** The sentence the last save answered with, shown once. */
    val notice: String? = null,
    /** Why the last read or save could not answer. */
    val failure: String? = null,
)

/**
 * The Profile panel's account-security half: the passphrase-change form and the recovery-contact
 * form. The two secrets never leave this object except through the view model's save calls, which
 * wipe them the moment the worker takes them — the same contract the backup credential follows.
 */
data class AccountSecurityState(
    /** True while either form's save is in flight. */
    val busy: Boolean = false,
    /** The sentence the last save answered with, shown once. */
    val notice: String? = null,
    /** Why the last save could not answer. */
    val failure: String? = null,
)

/** One row of the conversation list. */
data class ConversationRow(
    val conversationId: Id,
    /**
     * What to show as the name.
     *
     * Resolved when the row is built rather than at draw time: a direct conversation has no title of
     * its own and has to borrow the peer's, which is a lookup, and a lookup inside a list item is a
     * lookup that runs on every scroll frame.
     */
    val title: String,
    val kind: ConversationKind,
    /**
     * The room behind a Room-kind conversation, when this shell knows one (a join or a create
     * named it). The leave affordance needs it: leaving is a room-service call, not a
     * conversation one.
     */
    val roomId: Id? = null,
    /** The last message, as a short line. Null when the conversation has no readable message yet. */
    val preview: String? = null,
    /** `lastSeq - readSeq`, floored at zero. */
    val unread: Long = 0,
    /** Unix milliseconds of the last activity, for ordering and for the timestamp column. */
    val updatedAt: Long = 0,
)

/** One open conversation. */
data class ChatState(
    val conversationId: Id,
    val title: String,
    /** The room behind a Room-kind chat, when the shell knows one; the Leave control needs it. */
    val roomId: Id? = null,
    /** Oldest first: the order they are drawn in, and the order history must be replayed in. */
    val messages: List<ChatMessage> = emptyList(),
    /** True while history is being fetched and decrypted. */
    val loading: Boolean = false,
    /** True from the moment Send is pressed until the server accepts or rejects. */
    val sending: Boolean = false,
    /** Ids of accounts currently typing, other than this one. */
    val typing: Set<Id> = emptySet(),
    /** The text in the composer. Held here so a rotation does not lose a half-written message. */
    val draft: String = "",
    /**
     * The room's live shape, for a room chat's header and for gating its moderation controls. Null
     * for a direct chat, and null for a room until a summary or a state event has named its counts.
     */
    val room: RoomLiveInfo? = null,
    /**
     * The room's non-message timeline — joins, leaves, kicks — oldest first, capped so it cannot
     * grow without bound while a busy room is left open. Always empty for a direct chat.
     */
    val notices: List<RoomNotice> = emptyList(),
    /** The room's members once the member sheet has read them; null before the first read. */
    val roster: List<RosterMember>? = null,
    /** True while the roster is being read. */
    val rosterLoading: Boolean = false,
    /** Open kick votes by target: the running tally a member row shows while its vote is live. */
    val votes: Map<Id, VoteTally> = emptyMap(),
    /** Accounts this device has personally muted, for the member sheet's own Muted list. */
    val muted: Set<Id> = emptySet(),
    /** Whether the member sheet is covering the thread. */
    val membersOpen: Boolean = false,
    /** Accounts with a moderation or a mute action in flight, so only the pressed row shows it. */
    val acting: Set<Id> = emptySet(),
)

/**
 * A room's live shape, as the open chat reads it.
 *
 * Seeded from the [RoomSummary] a join, a create or the directory handed back, then kept current by
 * the room's event streams: a [com.migo.core.protocol.RoomStateEvent] ticks the counts, a member
 * event carries the running total, and the ceiling stays put. [myRole] is the field no state event
 * carries, so it is seeded from the summary and refined from the roster (which lists the caller among
 * the members). It is what gates the staff actions — an action offered over someone the caller does
 * not outrank is one the server can only reject.
 */
data class RoomLiveInfo(
    val onlineCount: Long,
    val memberCount: Long,
    /** The room's ceiling when it declares one; the capacity badge reads it, and null hides the badge. */
    val maxMembers: Long? = null,
    val myRole: RoomRole = RoomRole.Unknown,
)

/**
 * One line in a room's timeline that is not a message: a join, a leave, a disconnect, a kick, a ban.
 *
 * Built once from a [com.migo.core.protocol.RoomMemberEvent] for the open room — [text] is already the
 * display sentence and [key] is unique, because a notice that resolved its name or minted its key at
 * draw time would do both on every scroll frame.
 */
data class RoomNotice(
    val key: String,
    val text: String,
    /** Unix milliseconds the event was observed; member events carry no server time of their own. */
    val at: Long,
)

/** One member as the roster sheet draws it: a display name, the id behind it, and the room role. */
data class RosterMember(
    val userId: Id,
    val name: String,
    val role: RoomRole,
)

/** A running kick vote's tally, as a member row shows it while the vote is open. */
data class VoteTally(
    val votes: Long,
    val needed: Long,
)

/**
 * One message bubble.
 *
 * [text] is already the display string. A bubble never holds a [com.migo.core.crypto.Content], so
 * there is no path by which a media key or a control payload reaches a `Text` composable -- the
 * mapping happens once, where the content is decoded.
 */
data class ChatMessage(
    val messageId: Id,
    /** The server sequence number, or 0 for a message this device has sent but not yet had accepted. */
    val seq: Long,
    /** True when this device sent it, which is the only thing that decides which side it sits on. */
    val mine: Boolean,
    /** Who sent it, as a display string. Empty for own messages, which need no label. */
    val author: String,
    val text: String,
    val at: Long,
    /** True for a message sitting in the list before the server has accepted it. */
    val pending: Boolean = false,
    /**
     * True for a body this build cannot render: a content type from a newer peer, or a decode this
     * client refused. The bubble says so rather than showing an empty line, because a message that
     * silently renders as nothing looks like a delivery failure.
     */
    val unsupported: Boolean = false,
)

/**
 * A wei amount as AVAX, 18 decimals, trailing zeros trimmed: the amount a person typed is the
 * amount they should read back.
 */
fun avaxOf(wei: BigInteger): String = decimalOf(wei, 18)

/** A wei amount as nAVAX (§184's fee unit): 9 decimals, trailing zeros trimmed. */
fun navaxOf(wei: BigInteger): String = decimalOf(wei, 9)

private fun decimalOf(wei: BigInteger, decimals: Int): String {
    val whole = wei.divide(BigInteger.TEN.pow(decimals))
    var fraction = wei.subtract(whole.multiply(BigInteger.TEN.pow(decimals))).toString(10)
    if (fraction.all { it == '0' }) return whole.toString(10)
    while (fraction.length < decimals) fraction = "0$fraction"
    return "${whole}.${fraction.trimEnd('0')}"
}

/**
 * The send form's amount string as wei, or null when it is not an amount this chain accepts.
 *
 * The refusals are the ones the desktop client enforces too: empty, signed, non-decimal, a second
 * dot, more than 18 fractional digits, or too large for the u128 the wire carries.
 */
fun parseAvaxAmount(text: String): BigInteger? {
    val trimmed = text.trim()
    if (trimmed.isEmpty()) return null
    val parts = trimmed.split('.')
    if (parts.size > 2) return null
    val (whole, fraction) = parts[0] to parts.getOrElse(1) { "" }
    if (whole.isEmpty() && fraction.isEmpty()) return null
    if (whole.any { !it.isDigit() } || fraction.any { !it.isDigit() }) return null
    if (fraction.length > 18) return null
    val unit = BigInteger.TEN.pow(18)
    val wholeWei = (if (whole.isEmpty()) BigInteger.ZERO else BigInteger(whole, 10)).multiply(unit)
    val fractionWei = if (fraction.isEmpty()) {
        BigInteger.ZERO
    } else {
        BigInteger(fraction, 10).multiply(BigInteger.TEN.pow(18 - fraction.length))
    }
    return wholeWei.add(fractionWei)
}
