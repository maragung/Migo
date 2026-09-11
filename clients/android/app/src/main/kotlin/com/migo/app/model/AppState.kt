package com.migo.app.model

import com.migo.core.ConnectionState
import com.migo.core.crypto.PeerSafetyNumber
import com.migo.core.net.AdminView
import com.migo.core.net.CaptchaChallenge
import com.migo.core.net.DeviceSummary
import com.migo.core.net.WalletSummary
import com.migo.core.protocol.BadgeWire
import com.migo.core.protocol.ConversationKind
import com.migo.core.protocol.GameCatalogueEntry
import com.migo.core.protocol.GameViewWire
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
        /**
         * The captcha challenge the form is showing, or null when it is showing none.
         *
         * Held here rather than in the screen because the server owns the timing: a challenge
         * arrives either because the register form fetched one (the web client's fetch-on-mount,
         * mirrored) or because a refused attempt came back carrying the replacement challenge
         * already spent proof entitles the next attempt to. A null-to-non-null transition is the
         * widget appearing; a value swap is the picture changing, without a round trip.
         *
         * The *answer* is not here: it is typing, held in the form's own local state beside the
         * passphrase and passed to the submit call as an argument, so no credential-shaped string
         * ever rides on this object.
         */
        val captcha: CaptchaChallenge? = null,
        /** What went wrong last time, already reduced to something worth showing a person. */
        val failure: String? = null,
    ) : AppState

    /**
     * Signed in. The conversation list is always present; [open] is the chat the window strip is
     * showing, and [windows] is every conversation the strip holds a tab for.
     *
     * The mobile shell is a tab strip at the top: Friends, Rooms and Feed are the home tabs (Feed
     * the only closable one, its X moving it to [hiddenNavs] until the "+" reopens it), and every
     * open conversation is a window tab beside them. One window is visible at a time -- [open] --
     * and a parked window keeps its tab, so switching back is one tap. The [section] is which home
     * view shows when no window is visible; the panels (Alerts, Search, Wallet, Profile, Games,
     * Admins) open from the me card's sheet and cover the screen the way they always did, each
     * carrying its own way back to [stripSection].
     */
    data class SignedIn(
        val username: String,
        val accountId: Id,
        val connection: ConnectionState,
        val conversations: List<ConversationRow> = emptyList(),
        /** True while the first page of conversations is loading. */
        val loading: Boolean = false,
        /** The conversation the window strip is showing, or null when a home view is on top. */
        val open: ChatState? = null,
        /** The open conversation windows, in open order: one strip tab each. */
        val windows: List<WindowTab> = emptyList(),
        /** The home tabs closed from their X in the strip (only Feed is closable). */
        val hiddenNavs: Set<Section> = emptySet(),
        /** A transient failure banner: a send that did not go, a page that did not load. */
        val failure: String? = null,
        /** The home view on screen when no window is visible; Friends is where a session starts. */
        val section: Section = Section.FRIENDS,
        /**
         * The home tab the strip shows, held while a panel covers the screen, so the panel's back
         * returns to the view the strip still highlights.
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
        /**
         * True while the shell owes a just-registered account its file offer: the sealed `.migo`
         * container the session layer minted from the registration root is waiting to be saved,
         * and no server holds a copy. Cleared the moment the file is written or the offer is
         * declined; the sealed bytes themselves live in the view model, never on this object —
         * the screens get the flag, and the write call is the only thing that needs the bytes.
         */
        val accountFileOffer: Boolean = false,
        val profileEdit: ProfileEditState = ProfileEditState(),
        val accountSecurity: AccountSecurityState = AccountSecurityState(),
        val securityCheckup: SecurityCheckupState = SecurityCheckupState(),
        val admins: AdminsState = AdminsState(),
        val games: GamesState = GamesState(),
        val settings: SettingsPanelState = SettingsPanelState(),
    ) : AppState

    /**
     * The shell's destinations. FRIENDS, ROOMS and FEED are the home tabs the strip carries; CHATS
     * is the window tabs' own ground, held for the parts that still speak in section terms. The
     * rest are the panels the me card's sheet opens — they cover the strip rather than joining it,
     * which is how a phone wears a second pane.
     */
    enum class Section {
        CHATS, FRIENDS, ROOMS, GAMES, FEED, ALERTS, SEARCH, WALLET, PROFILE, ADMINS, SETTINGS;

        /** True for the panels the me sheet opens, which cover the strip rather than join it. */
        val isPanel: Boolean
            get() = this == ALERTS || this == SEARCH || this == WALLET || this == PROFILE || this == ADMINS || this == GAMES || this == SETTINGS
    }
}

/** One open conversation window, as the strip's tab holds it. */
data class WindowTab(
    val conversationId: Id,
    val title: String,
    /** The room behind a room-kind window, so a reopened tab can restore its live info. */
    val roomId: Id? = null,
)

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

/**
 * The Profile panel's security checkup (§50): six fixed rows — Identity, Devices, Wallets,
 * Backup, Recovery, E2EE — the same set every client builds.
 *
 * The rows that answer from state the shell already holds (the device list, the wallet
 * registrations) do not copy it here; this holder carries only the checkup's own facts: the
 * recovery contact's existence, and the two persisted timestamps that make the Backup row
 * honest. A null [recoveryConfigured] is "not checked yet" — kept distinct from false, which is
 * the warning, so a panel that showed them the same would say "Recovery contact not set"
 * before it had ever asked.
 */
data class SecurityCheckupState(
    /** Whether the account holds a recovery contact; null before the first read lands. */
    val recoveryConfigured: Boolean? = null,
    /** Unix ms of this device's last successful .migo export, or 0 when never (from Settings). */
    val lastBackupExportMs: Long = 0L,
    /** Unix ms of the last identity-key rotation, or 0 when never (from Settings). */
    val lastIdentityRotationMs: Long = 0L,
    /** True while the checkup's own reads are in flight. */
    val loading: Boolean = false,
    /** Why the last checkup read could not answer; the rows then say "not checked" honestly. */
    val failure: String? = null,
)

/**
 * The Admins panel: the Owner/CEO's management page over the global admins. The standing is
 * asked once per session (a whoami that never fails on standing) and gates the banner menu's
 * very entry, because the management page's whole point is that its existence is not public
 * information -- a non-owner never sees the word. `Closed` is a fact, not a failure: the
 * honest answer for an account that holds neither role, drawn as a sentence the same way the
 * web client draws it.
 */
data class AdminsState(
    /** The list, once the standing said owner and the read answered. Null before that. */
    val admins: List<AdminView>? = null,
    /** True while the standing-and-list read is in flight. */
    val loading: Boolean = false,
    /** True for the account this deployment names as its Owner/CEO. */
    val owner: Boolean = false,
    /** The standing was asked and the answer is "not yours to open". */
    val closed: Boolean = false,
    /** True while a grant or a revoke is in flight. */
    val busy: Boolean = false,
    /** Account ids with a revoke in flight, so only the pressed row shows its busy state. */
    val revoking: Set<String> = emptySet(),
    /** The sentence the last grant or revoke answered with, shown once. */
    val notice: String? = null,
    /** Why the last read or change could not answer. */
    val failure: String? = null,
)

/**
 * The Games panel's read: the node's own catalogue, held the session's life.
 *
 * The catalogue is versionless server-side, so it is re-read per session rather than cached across
 * one — the same posture as the gift catalogue — and the null-before-first-read rule keeps "not
 * checked yet" distinct from "this server referees nothing".
 */
data class GamesState(
    /** The node's entries; null until the first read lands. */
    val catalogue: List<GameCatalogueEntry>? = null,
    /** True while the catalogue is being read. */
    val loading: Boolean = false,
    /** Why the last read could not answer. */
    val failure: String? = null,
)

/**
 * The Settings panel's own facts: the storage the caches hold, and the one-shot sentence an action
 * there answers with.
 *
 * The preferences themselves are not here — they live in the view model's preferences flow, a
 * device-scoped fact that exists before any sign-in (the theme has to) — while this holder carries
 * only what the panel measures: the sizes of the caches it offers to clear, and the notice that
 * says a clear or a save landed. Null sizes are "not measured yet", the same honest
 * not-checked-yet the device list keeps, rather than a zero that would read as "nothing to clear".
 */
data class SettingsPanelState(
    /** The temporary-media cache (recordings, playback scratch) in bytes, or null before the first walk. */
    val cacheBytes: Long? = null,
    /** The auto-saved chat logs' directory in bytes, or null before the first walk. */
    val logBytes: Long? = null,
    /** How many conversation snapshots the log directory holds. */
    val logCount: Int = 0,
    /** True while a clear is running, so its button cannot double-fire. */
    val clearing: Boolean = false,
    /** The sentence the last clear or log save answered with, shown once. */
    val notice: String? = null,
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
    /**
     * The peer account behind a Direct conversation, when the row is built with the member list at
     * hand. The friends list keys off it: a friend's row can then carry their chat's preview and
     * unread badge, and the strip's Friends tab can sum the direct conversations' unread.
     */
    val peerId: Id? = null,
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
    /**
     * The conversation's kind. Games are offered only where a game has an audience — a room or a
     * group, never a direct chat — mirroring the web client's rule, so the kind rides with the chat.
     */
    val kind: ConversationKind = ConversationKind.Room,
    /** The room behind a Room-kind chat, when the shell knows one; the Leave control needs it. */
    val roomId: Id? = null,
    /**
     * The peer account behind a Direct conversation, when the row it was opened from knew one.
     * The safety numbers are a read against the peer's published identities, so the chat carries
     * the id that read needs — null for a room chat, and for a direct chat whose row arrived
     * without its member list, in which case there is simply no verification surface to show.
     */
    val peerId: Id? = null,
    /** Oldest first: the order they are drawn in, and the order history must be replayed in. */
    val messages: List<ChatMessage> = emptyList(),
    /** True while history is being fetched and decrypted. */
    val loading: Boolean = false,
    /** True from the moment Send is pressed until the server accepts or rejects. */
    val sending: Boolean = false,
    /**
     * True from the moment an attachment is picked (or a recording finishes) until the message
     * that references it is accepted. Separate from [sending] so the composer can say which of
     * the two waits it is showing, the way the web client's composer does.
     */
    val uploading: Boolean = false,
    /**
     * True while this device is recording a voice note for the conversation. While it holds, the
     * composer is the recording bar — no text can be typed into a moment that is being recorded.
     */
    val recording: Boolean = false,
    /** Ids of accounts currently typing, other than this one. */
    val typing: Set<Id> = emptySet(),
    /** The text in the composer. Held here so a rotation does not lose a half-written message. */
    val draft: String = "",
    /**
     * Whether the thread's search field is showing. Held here rather than in the screen so it
     * survives a rotation like the draft does; the toggle that flips it also clears the query,
     * mirroring the web client's toggle, so a reopened field never resumes a stale filter.
     */
    val searchOpen: Boolean = false,
    /**
     * The live query text, filtered per keystroke — no submit step, because the web client's
     * filter runs on every render and the phone answers the same way. Blank (or whitespace-only)
     * means no filter: the thread draws its full loaded list, not "everything matches".
     */
    val searchQuery: String = "",
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
    /**
     * The chat's active game, as the server redacted it for this account. Session-scoped: the sync
     * replay carries no game events, so a chat opens with none and learns of one from its moves.
     */
    val game: GameViewWire? = null,
    /** True while a game start or a guess is in flight, so neither control can double-fire. */
    val gameBusy: Boolean = false,
    /**
     * The direct conversation's verification surface: one safety number per device the peer
     * publishes, and whether any of their identities changed since this conversation last
     * acknowledged them. Null for a room chat, and for a direct chat until the read lands — which
     * is the honest state, because a number that shows before the read is a number invented on the
     * spot, the most reassuring thing a verification surface could wrongly display.
     */
    val safety: ChatSafety? = null,
)

/**
 * A direct conversation's safety numbers, as the verification surface shows them.
 *
 * The numbers are per peer *device* — a Migo identity belongs to a device, so a peer signed in
 * twice publishes two — and [changed] is true when any of them differs from the fingerprint this
 * conversation last acknowledged. That flag is not cleared by the read that raised it: it stays
 * until the person acknowledges it from the warning itself, which is brief section 164's
 * "visible" done properly.
 */
data class ChatSafety(
    /** One safety number per peer device, devices in the order the enumeration returned them. */
    val numbers: List<PeerSafetyNumber>,
    /** True while any observed identity is unacknowledged as changed. */
    val changed: Boolean = false,
    /** Why the read could not answer, when it could not. The numbers are then empty. */
    val failure: String? = null,
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
 * mapping happens once, where the content is decoded. An [Attachment] is the one thing a bubble
 * may hold besides text: it is the decoded descriptor a resolver needs to fetch and open the
 * object, and its key and nonce reach the resolver and only the resolver -- never a `Text`, never
 * a log line.
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
    /**
     * The media body when the message is an image, a document or a voice note. [text] stays the
     * row's label (the caption, the file name, "Photo", "Voice note") so the conversation list and
     * the placeholder states need nothing from this field.
     */
    val attachment: Attachment? = null,
)

/**
 * What a media message is, decided once where the content was decoded.
 *
 * The web client tells a document from an image by the claimed MIME type and a voice note by its
 * content shape; this is the same decision recorded as a value, so the bubble switch is a `when`
 * over three cases rather than a mime-string test the renderer re-derives per draw.
 */
enum class AttachmentKind { Image, Document, Voice }

/**
 * One media body, decoded once where the content was decoded -- the app-side mirror of a
 * `Content.MediaRef` or `Content.VoiceNoteRef`, with the discriminator resolved and nothing else
 * carried along.
 *
 * [key] and [nonce] are the slots that open the sealed object; they exist here because a resolver
 * needs them, and the rule that keeps them safe is the [ChatMessage] one: they are passed to the
 * resolver and never reach a `Text` composable or a log. [mimeType] is the sender's claim,
 * authenticated by the message's own seal, and is a label for playback -- never a fact this client
 * acts on beyond choosing how to render (brief section 122).
 */
class Attachment(
    /** The storage id of the object. */
    val mediaId: Id,
    /** The sender's claimed MIME type -- a label for the renderer, re-judged nowhere here. */
    val mimeType: String,
    /** The claimed plaintext length, for the document row's size line. */
    val sizeBytes: Long,
    /** The symmetric key that opens the object, or all zeroes for a legacy plaintext upload. */
    val key: ByteArray,
    /** The nonce that opens the object, or all zeroes for a legacy plaintext upload. */
    val nonce: ByteArray,
    /** Which of the three bodies this is; decided at decode, never re-derived. */
    val kind: AttachmentKind,
    /** Pixel width when the sender supplied it, so an image reserves its shape before loading. */
    val width: Long? = null,
    /** Pixel height when the sender supplied it. */
    val height: Long? = null,
    /** A document's file name (the caption slot) or an image's caption. */
    val caption: String? = null,
    /** A voice note's playback duration. */
    val durationMs: Long? = null,
    /** A voice note's amplitude preview, when the sender supplied one. */
    val waveform: ByteArray? = null,
)

/** The quick reactions the long-press bar offers, in order -- the web client's own list. */
val QUICK_REACTIONS: List<String> = listOf("👍", "❤️", "😂")

/**
 * One media object's resolve state, session-scoped: a bubble asks the resolver for its [Attachment]
 * and reads the answer back from here, so a re-render never refetches and two bubbles that share a
 * media id share one download.
 */
sealed interface MediaObject {
    val mediaId: Id

    /** The fetch is in flight; the bubble keeps its placeholder. */
    class Loading(override val mediaId: Id) : MediaObject

    /** The opened bytes. Held only in memory, for the session, never written to disk. */
    class Ready(override val mediaId: Id, val bytes: ByteArray) : MediaObject

    /** The download or the open failed; the placeholder says so, and a re-ask retries. */
    class Failed(override val mediaId: Id) : MediaObject
}

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

// --- the games vocabulary, as plain data ---

/**
 * The kind numbers this build's server referees (the games crate fixes them in code), mirrored
 * rather than re-invented: an unknown value from a newer node still renders, under the generic
 * label, rather than being mis-named or dropped.
 */
const val GAME_KIND_TIC_TAC_TOE = 0L
const val GAME_KIND_ROCK_PAPER_SCISSORS = 1L
const val GAME_KIND_GUESS_NUMBER = 2L

/** The game statuses the store persists; OPEN is the only one a move may be applied to. */
const val GAME_STATUS_OPEN = 0L

/** The human label for a game kind; an unknown kind renders as a generic "Game". */
fun gameLabelOf(kind: Long): String = when (kind) {
    GAME_KIND_TIC_TAC_TOE -> "Tic-tac-toe"
    GAME_KIND_ROCK_PAPER_SCISSORS -> "Rock paper scissors"
    GAME_KIND_GUESS_NUMBER -> "Guess the number"
    else -> "Game"
}

/**
 * The player-count sentence for a catalogue entry, e.g. `1 player`, `2 players`, `2–4 players`.
 *
 * A range only reads as a range when the ends differ; a single-player game that said "1–1 players"
 * would be arguing with itself.
 */
fun playerRangeLabel(minPlayers: Long, maxPlayers: Long): String =
    if (maxPlayers != minPlayers) {
        "$minPlayers–$maxPlayers players"
    } else {
        "$minPlayers " + (if (minPlayers == 1L) "player" else "players")
    }

/** One parsed guess: the number guessed and what the server said about it. */
data class GuessEntry(
    val value: Long,
    /** `lower`, `higher`, or `correct` — the board's own words. */
    val feedback: String,
)

/** The guessing game's board line, parsed. The hidden number appears in no field, by design. */
data class GuessBoard(
    val low: Long,
    val high: Long,
    val remaining: Long,
    val guesses: List<GuessEntry>,
)

/**
 * Parses the guessing game's `board` string: `low-high:remaining` followed by one ` guess:feedback`
 * per guess, the feedback being `lower`, `higher`, or `correct`.
 *
 * Returns null for anything else — a board of a different game, or a grammar a newer server
 * changed — so a caller falls back to saying nothing rather than mis-quoting the state. The
 * parsing is strict on purpose: a number that "mostly" matched would invent a range the server
 * never stated.
 */
fun parseGuessBoard(board: String): GuessBoard? {
    val parts = board.trim().split(Regex("\\s+"))
    val head = Regex("^(\\d+)-(\\d+):(\\d+)$").find(parts.firstOrNull() ?: return null) ?: return null
    val guesses = ArrayList<GuessEntry>()
    for (part in parts.drop(1)) {
        val entry = Regex("^(\\d+):(lower|higher|correct)$").find(part) ?: return null
        guesses.add(GuessEntry(entry.groupValues[1].toLong(), entry.groupValues[2]))
    }
    return GuessBoard(
        low = head.groupValues[1].toLong(),
        high = head.groupValues[2].toLong(),
        remaining = head.groupValues[3].toLong(),
        guesses = guesses,
    )
}

/** The sentence the guess card shows about the newest guess, or null before the first one. */
fun guessFeedbackLine(board: GuessBoard): String? {
    val last = board.guesses.lastOrNull() ?: return null
    return when (last.feedback) {
        "lower" -> "The secret is lower."
        "higher" -> "The secret is higher."
        "correct" -> "Correct!"
        else -> null
    }
}

/**
 * The one-line text a game event row shows, when the server did not pre-render one of its own.
 *
 * The row's grammar is fixed per event name: a start names the game, a move names only the mover
 * (the published delta deliberately says nothing *about* the move, and the line must not either),
 * a finish names the winner when there is one. An event name this build does not know renders as a
 * neutral "Game update" rather than the raw wire word, which is server vocabulary a reader never
 * chose to see. `who` is already the display name — "You" for ourselves — or null when the event
 * names nobody.
 */
fun gameEventLine(event: String, who: String?, label: String?): String {
    val name = label ?: "Game"
    return when (event) {
        "started" -> if (who != null) "🎮 $name started by $who" else "🎮 $name started"
        "moved" -> (who ?: "Someone") + " made a move in $name"
        "turn_changed" -> (who ?: "Someone") + "'s turn in $name"
        "finished" -> if (who != null) "🏆 $who won $name!" else "$name ended"
        else -> "Game update"
    }
}
