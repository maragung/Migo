//! The network worker: everything asynchronous, on its own thread, behind two channels.
//!
//! # Why the UI never awaits
//!
//! egui redraws by calling one function per frame. If that function ever awaited a socket, a slow
//! server would freeze the window — not a spinner, an unresponsive process the compositor greys out.
//! So the split is absolute: this module owns the tokio runtime, the sockets, the ratchets and the
//! vault, and the UI thread owns pixels. They meet at two unbounded channels carrying plain data.
//!
//! It is not a mutex around shared state, deliberately. A mutex the paint loop has to take is a mutex
//! that can be held by whatever is doing I/O, which is the same freeze by a longer route. Ownership
//! moves with the message instead, so there is nothing to contend for.
//!
//! # What crosses the channel
//!
//! [`Command`] carries intent — "send this text", "open this conversation". [`Event`] carries facts
//! already reduced to what a person sees: a decrypted [`crate::model::Message`], a connection state,
//! a toast. Ciphertext, envelopes, ratchet state and key material never leave this module, so no UI
//! code can accidentally render or log them.
//!
//! # Reconnection
//!
//! The worker reconnects on its own with exponential backoff and jitter, because a client that gives
//! up on the first dropped packet is a client people restart by hand. Jitter matters at the other end:
//! a node restarting with ten thousand clients attached gets them back in a spread rather than as one
//! synchronised thundering herd.

pub mod call;
pub(crate) mod call_audio;
pub(crate) mod call_signal;
pub(crate) mod call_video;
pub mod chain;
pub mod gateway;
pub(crate) mod group_call;
pub(crate) mod media;
pub mod quic;
pub mod rest;
pub(crate) mod room_bridge;
pub mod server_probe;
pub mod tcp;
pub(crate) mod voice_draft;
pub(crate) mod voice_listened;

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use migo_core::{Id, OsRandom, Random, Timestamp};
use migo_protocol::{
    codes, features, BadgesReq, ClientInfo, ConversationKind, DeliveryClass, EncryptionMode,
    EntitlementsReq, Frame, FriendRespond, FriendTarget, GiftCatalogueReq, GiftSend, InboxReq,
    KickPointsBuy, LeaderboardReq, LedgerReq, MessageKind, NotificationAck, Opcode, ProgressionReq,
    RelationshipListReq, RoomCreate, RoomJoinRequest, RoomLeaveRequest, RoomListRequest, SearchReq,
    SubscribeRequest, SuggestReq, Topic, TopicKind, WalletReq,
};
use tokio::sync::mpsc;

use crate::config::{ServerEndpoint, Transport};
use crate::crypto::content::{self, Content};
use crate::crypto::envelope::Envelope;
use crate::crypto::group::GroupStore;
use crate::crypto::session::{DeviceKeys, SessionStore, ONE_TIME_PREKEY_COUNT};
use crate::model::{
    self, Account, AlertRow, Body, ChainNetwork, ChainTxRow, Connection, Conversation, Delivery,
    DeviceRow, EvmWalletRow, GiftRow, LeaderRow, LedgerRow, Message, PersonRow, PreparedTx,
    Progression, Relationship, RelationshipKind, RoomRow, SessionRow, ToastKind,
};
use crate::net::chain::{ChainClient, TrackOptions};
use crate::net::gateway::{Gateway, GatewayError};
use crate::net::quic::QuicGateway;
use crate::net::rest::{CaptchaChallenge, CaptchaProof, DeviceRequest, Grant, Rest, RestError};
use crate::net::tcp::TcpGateway;
use crate::settings::VoiceSpeed;
use crate::vault::{self, SavedSession, TxRecord};

/// When to warn that the one-time prekey pool is running down.
///
/// A fifth of the published pool. Low enough that the warning is not noise on a busy account, high
/// enough that there is still time to act before the pool is empty.
const ONE_TIME_PREKEY_LOW_WATER: usize = ONE_TIME_PREKEY_COUNT as usize / 5;

/// How many undecryptable messages one sender may have held at once, awaiting its distribution.
///
/// The SDK holds the same bound. The number is a compromise the same way every skipped-message
/// bound is: generous enough that a burst of sends before a distribution lands survives, small
/// enough that a sender whose distribution never comes costs a bounded amount of memory. Held
/// messages drop oldest-first.
const MAX_PENDING_PER_SENDER: usize = 64;

/// One page of a catch-up walk, matching the web client's `CATCHUP_PAGE`.
///
/// The same number on every client so a thread that needs paging costs the same wire on each:
/// one SYNC answer bounded in rows, trimmed by the server's own byte budget when a page of fat
/// envelopes would otherwise cross the frame ceiling.
const SYNC_PAGE: u32 = 200;

/// How many pages a catch-up walk may ask for, matching the web client's `MAX_CATCHUP_PAGES`.
///
/// The budget keeps a very long conversation bounded: a walk that reaches it stops short of the
/// live edge, the thread says so through [`Event::History`]'s `more`, and the load-earlier row
/// walks the rest down from the newest — the same discipline every client in this batch keeps,
/// so no one of them invents its own unbounded walk.
const MAX_CATCHUP_PAGES: u32 = 5;

/// The server's error symbols that mean "the captcha proof is dead, whatever else is true".
///
/// Wrong, expired, or never sent: the three differ on the wire but demand the same response from
/// a form — drop the held challenge, fetch a fresh one, keep the form standing. Matched here
/// rather than in the UI because the symbol is wire vocabulary, and events are reduced facts.
const CAPTCHA_REFUSAL_SYMBOLS: [&str; 3] =
    ["INVALID_CAPTCHA", "CAPTCHA_EXPIRED", "CAPTCHA_REQUIRED"];

/// A captcha answer on its way from a form to the worker.
///
/// Owned because everything a command carries crosses a channel; the worker lends it to
/// [`CaptchaProof`] when the request body is built. The manual `Debug` keeps the answer out of
/// any trace this command path might grow, for the same reason [`crate::net::rest::Grant`]'s
/// keeps its tokens out — and even though an answer is worth far less than a token, a log line
/// that never contains it is a log line that cannot leak it.
pub struct CaptchaAnswer {
    /// The challenge being answered, exactly as the server issued it.
    pub challenge_id: String,
    /// What the user read off the image, already normalised: upper-cased, whitespace-free.
    pub answer: String,
}

impl std::fmt::Debug for CaptchaAnswer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CaptchaAnswer")
            .field("challenge_id", &self.challenge_id)
            .field("answer", &"***")
            .finish()
    }
}

/// What a profile save wants changed: the wire's absent-means-unchanged contract, in the UI's
/// own vocabulary.
///
/// A struct rather than seven loose fields because the save has seven of them and the forms
/// that build one share the shape: every `None` is a control the user did not touch, and the
/// wire keeps the corresponding server-side value.
#[derive(Debug, Default, Clone)]
pub struct ProfilePatch {
    /// New display name, or absent to leave it.
    pub display_name: Option<String>,
    /// New bio, or absent to leave it.
    pub bio: Option<String>,
    /// New custom status, or absent to leave it. The wire's own semantics: an empty string
    /// sets an empty status, exactly like the bio — the field's absence is the only "keep".
    ///
    /// Only joins a save on a session that negotiated RICH_PRESENCE; the pane gates the edit
    /// on the bit, and a patch from a session without it would be answered with a refusal.
    pub custom_status: Option<String>,
    /// New birth year, or absent.
    pub birth_year: Option<u32>,
    /// New last-seen visibility: absent is "leave as-is", because the server never sends the
    /// current value back even to its owner.
    pub show_last_seen: Option<u32>,
    /// New messaging visibility, same absent rule.
    pub who_can_message: Option<u32>,
    /// New friend-request visibility, same absent rule.
    pub who_can_add: Option<u32>,
    /// New search opt-in: absent is "the switch was never flipped".
    pub searchable: Option<bool>,
}

/// What the UI asks the worker to do.
#[derive(Debug)]
pub enum Command {
    /// Fetch a fresh image captcha challenge for the auth forms.
    ///
    /// Separate from register and sign-in because the challenge has to be on screen *before*
    /// either form can be finished; a form asks the moment it draws with nothing held.
    FetchCaptcha {
        /// The server to ask — the one the form is pointing at, which is not necessarily the
        /// one a session lives on, because no session exists yet.
        server: ServerEndpoint,
        /// The rendering to ask for: `None` for the server's default, `Some("image_alt")` for
        /// the gentler one. A string because it is the wire's own vocabulary, spelled out once
        /// at the click that sets it.
        mode: Option<String>,
    },
    /// Create an account, generate keys, and write a new vault.
    Register {
        server: ServerEndpoint,
        username: String,
        account_passphrase: String,
        passphrase: String,
        /// The answered captcha challenge, when the form held one and the answer had the shape
        /// worth sending. `None` submits without a proof.
        captcha: Option<CaptchaAnswer>,
    },
    /// Sign in to an existing account, generating keys if this device has none yet.
    SignIn {
        server: ServerEndpoint,
        identifier: String,
        account_passphrase: String,
        passphrase: String,
        /// The answered captcha challenge, as on [`Command::Register`].
        captcha: Option<CaptchaAnswer>,
    },
    /// Open the existing vault and resume its saved sign-in.
    Unlock { passphrase: String },
    /// End the session and forget every key on this device.
    SignOut,
    /// Refresh the conversation list.
    Conversations,
    /// Load history for one conversation, from what the worker holds upwards.
    ///
    /// `held` is the thread's own word for whether it is holding anything at all: a thread that
    /// holds nothing replays from the beginning (`have_seq` zero), while one that holds a
    /// transcript resumes from the worker's contiguous watermark — §158's "sync only the gap,
    /// never a full resync", with the floor chosen where the messages actually are rather than
    /// where a first key exchange happened to land.
    History { conversation_id: Id, held: bool },
    /// Page older history into a thread that already holds some, downwards from `before_seq`.
    ///
    /// `before_seq` zero means "from the newest", the shape a budget-stopped walk leaves: the
    /// newest page is what a short forward walk owes the reader, and the walk downwards dedups
    /// against what the thread already holds. The answer arrives as
    /// [`Event::HistoryEarlier`], matched by the correlation this ask minted.
    HistoryEarlier {
        conversation_id: Id,
        before_seq: u64,
    },
    /// Fetch one account's key bundles, so a private conversation's window can show their safety
    /// numbers before anything is sent.
    ///
    /// The bundle response is the only place a peer's identity key is ever observed; without this
    /// ask the numbers would appear only after the first send, and a verification surface that
    /// arrives after the conversation has started is a surface nobody looks at.
    PeerKeys { user_id: Id },
    /// Encrypt and send text.
    ///
    /// `expires_in_ms` is the disappearing lifetime the composer armed, when it did — the one
    /// value, riding both the wire field (the server's sweeper reads it) and the sealed content
    /// (a receiver's own countdown reads it). `None` is an ordinary, permanent message.
    SendText {
        conversation_id: Id,
        text: String,
        expires_in_ms: Option<u32>,
    },
    /// Start a direct conversation with one username.
    StartDirect { username: String },
    /// Start a direct conversation with an account id already held — a search hit, a suggestion.
    StartDirectById { peer: Id },
    /// Create a group conversation with the friends chosen in the new-group form. The member
    /// list carries the *other* members, exactly the way a direct create names its one peer:
    /// the server adds the caller, mints the founder role for them, and drops the caller's own
    /// id from the list without an error (the same tolerance `start_direct` relies on).
    CreateGroup {
        /// The other founding members. One is the minimum — the server refuses an empty list
        /// with "a conversation needs somebody other than its creator", and the form holds the
        /// button back until then rather than sending a refusal the person could have been
        /// spared.
        members: Vec<Id>,
        /// The group's title, as typed. Trimmed by the form; the server enforces its own
        /// length bound and says so in a sentence worth reading.
        title: String,
    },
    /// Add members to a group conversation. Any current member's right, within the group size
    /// cap the server holds; already-seated names are dropped by the server, not refused.
    InviteToGroup {
        conversation_id: Id,
        members: Vec<Id>,
    },
    /// Leave a group conversation. The last founder out promotes the earliest remaining member,
    /// so the group survives the person who made it.
    LeaveGroup { conversation_id: Id },
    /// Read a group conversation's roster: every member with role, join time, mute, and
    /// departure. The answer is what the roster panel draws and what the founder controls
    /// gate on — the member cache the send path keeps is an audience, not a roster.
    GroupRoster { conversation_id: Id },
    /// A founder mutes one member until `until`, or lifts the mute when `until` is `None`.
    /// The server refuses a self-mute and a mute aimed at the other founder; the UI mirrors
    /// both gates so its buttons say what the wire would allow.
    MuteGroupMember {
        conversation_id: Id,
        target_id: Id,
        until: Option<Timestamp>,
    },
    /// A founder removes a member outright, no vote. The other founder is beyond this reach,
    /// and the server says so; the roster panel keeps the button off founders for the same
    /// reason the web client does — a refusal the person can see coming is kinder than one
    /// that arrives.
    KickGroupMember { conversation_id: Id, target_id: Id },
    /// Start a kick vote in a group, or add this account's voice to one already running.
    /// Every member's lever, founders excepted as targets, one voice per account, and a
    /// strict majority of the members carries it — the same rules the web client states in
    /// its panel.
    VoteKickMember { conversation_id: Id, target_id: Id },
    /// A founder renames a group. The wire is a delta of title alone; the server answers with
    /// the refreshed summary and the group's members hear the new name through a state event.
    RenameGroup { conversation_id: Id, title: String },
    /// Report typing state. Best effort; dropped silently when offline.
    Typing { conversation_id: Id, typing: bool },
    /// Mark everything up to `seq` as read.
    MarkRead { conversation_id: Id, seq: u64 },
    /// Refresh the social graph: friends, pending requests, and the other relationship kinds.
    ///
    /// Follows the same shape as [`Command::Conversations`] because it is the same idea — a
    /// screen asks for the list, the worker reduces the wire answer to model rows, the screen
    /// gets one event.
    Friends,
    /// Send a friend request to an account id, typed by the user.
    ///
    /// Carries the raw text rather than an [`Id`] for the same reason [`Command::StartDirect`]
    /// does: the input field is a person typing, and the worker is where "what they typed is not
    /// an id" becomes a message worth reading instead of a parse panic.
    AddFriend { user_id: String },
    /// Accept or decline a pending request. The id comes from the relationship list, so it is
    /// already parsed.
    RespondFriend { user_id: Id, accept: bool },
    /// Fetch the device/session list over REST for the settings screen.
    Sessions,
    /// End one session of the account, by the id the list reported.
    RevokeSession { session_id: Id },
    /// Refresh the public room directory, optionally narrowed by a query.
    Rooms { query: String },
    /// Join a room, whose conversation opens like any other when the join is accepted.
    JoinRoom { room_id: Id },
    /// Create a room and enter it. Creation is entry: the reply is a join handle.
    CreateRoom {
        slug: String,
        name: String,
        /// True for a managed room (server-moderated); false for a public community room.
        managed: bool,
        topic: Option<String>,
    },
    /// Leave a room; the server closes its conversation for this account.
    LeaveRoom { room_id: Id },
    /// Read the durable notification inbox.
    Notifications,
    /// Mark every notification at or before one instant read.
    AcknowledgeAlerts { through_unix_ms: i64 },
    /// Read the wallet's whole economy: balance, statement, progression, badges, leaderboard,
    /// and the gift catalogue — six reads fired together, each arriving as its own event.
    Wallet,
    /// Buy and deliver a gift; the wallet re-reads after, because the server's arithmetic is the
    /// only arithmetic worth showing. `client_key` is the picker intent's idempotency key — the
    /// same key on every retry of one pick, so a lost reply is the first send again, not a second
    /// charge.
    SendGift {
        sku: String,
        recipient: Id,
        client_key: Option<String>,
    },
    /// Buy one Kick Point pack — the currency an outright group kick spends before it falls back
    /// to 1 $MIG. `client_key` is the buy intent's idempotency key, minted with the click, so a
    /// lost reply is the first buy again rather than a second charge. The packs and their prices
    /// are the server's; the buttons state them, and the server is the judge.
    BuyKickPoints { pack_kp: u32, client_key: String },
    /// Search public profiles by username prefix.
    SearchPeople { query: String },
    /// Ask the social graph for its own suggestions.
    Suggestions,
    /// Read the account's device list over REST for the security panel.
    Devices,
    /// Remove one of the account's devices: its sessions end with it (brief section 18).
    RevokeDevice { device_id: Id },
    /// Read the signed-in account's own profile card for the profile pane.
    ///
    /// Rides the same `PROFILE_FETCH` the names map uses, with the session's own id as the one
    /// subject; the worker routes the self card to [`Event::OwnProfile`] so the pane gets its
    /// copy without the pane knowing the fetch is a batch.
    OwnProfile,
    /// Read another member's profile card, for the member menu's "View profile".
    ///
    /// The conversation rides along because the worker's reply names only the card: the window
    /// that asked is the one that draws it, and a floating profile view that outlived the
    /// group window it was opened from would be a card with nowhere to belong. Like the own
    /// card, this rides the batch `PROFILE_FETCH` the names map uses — the worker remembers
    /// the ask and routes the answering card to [`Event::MemberProfile`] rather than to the
    /// names map alone.
    MemberProfile { conversation_id: Id, user_id: Id },
    /// Read another member's economy standing — progression and badges — for the member view's
    /// level, XP, and badge lines.
    ///
    /// The same two reads the wallet fires for the caller, aimed at another account: the wire
    /// keeps the facts on the economy service, and the worker remembers the ask so the replies
    /// file against the window that opened the view. Every fact degrades to absence — a view
    /// whose standing never answers is a card without its level lines, not a broken card.
    MemberStanding { conversation_id: Id, user_id: Id },
    /// Read the XP board's first page for another member's rank.
    ///
    /// The board is the community's own list, read whole rather than per account: the view
    /// shows a rank only when the person stands on the first page, and a person off it has no
    /// line at all — the same honest absence the web card draws.
    MemberRank { conversation_id: Id, user_id: Id },
    /// Read the caller's edge to one account, for the member view's social line.
    ///
    /// The graph walk is the friends pane's own read, issued again rather than borrowed,
    /// because the view needs the edge as it stands now — after a friend act, the line must
    /// say what the wire says, not what the pane last cached.
    MemberEdge { conversation_id: Id, user_id: Id },
    /// Read the account's entitlements — every catalogue code the account owns — for the
    /// composer's emoticon and sticker picker.
    ///
    /// One read per picker opening is the honest cadence: a pack bought elsewhere lands on
    /// the next open, the same trade the web picker's cached set makes.
    Entitlements,
    /// Read the gift catalogue alone, for the member menu's gift picker.
    ///
    /// The wallet's own read fires six requests at once, and a picker that only wants the
    /// shop's shelves should not pay for the balance, the statement, and the leaderboard to
    /// get them. The reply is the same [`Event::Gifts`] the wallet read lands as, so one
    /// handler files it for every surface that reads it.
    GiftCatalogue,
    /// Patch the caller's own profile. Absent fields keep their server-side values — the wire
    /// is a delta, not a replacement — so a save that changes a display name does not also have
    /// to know (and re-send) the privacy settings.
    SaveProfile(ProfilePatch),
    /// Upload a local image as the account's new avatar and point the profile at it.
    ///
    /// The path is read here, in the worker, because reading a file is I/O the UI thread must
    /// not own; the bytes, their claimed type, and a digest are all the wire needs. One command
    /// for both steps — upload then patch — because they are one action to the user: a failure
    /// anywhere surfaces beside the button that started it, and a success arrives as the
    /// profile's own saved event, the same one a save sends.
    ChangeAvatar { path: PathBuf },
    /// Read the account's standing on the admin surface, and the admin list when the answer is
    /// owner. One command for both because the standing is not a fact the UI should hold while
    /// a list loads: the panel decides nothing on it, it only draws what arrives.
    Admins,
    /// Appoint a global admin by username. Owner-only; the server is the judge.
    GrantAdmin { username: String },
    /// Revoke one global admin by the id the list reported. Owner-only, confirmed by the UI
    /// before it is sent, because a revocation takes moderation away from a person.
    RevokeAdmin { account_id: Id },
    /// Read the account's registered wallet addresses over REST.
    Wallets,
    /// Seal the account root into a `.migo` recovery container at `path`.
    ///
    /// The credential is the recovery credential the user chose for the container — a second
    /// secret, deliberately not the vault passphrase and not the account passphrase, because a
    /// backup sealed under either of those is a backup one breach opens.
    ExportContainer { path: PathBuf, credential: String },
    /// Restore the account from a `.migo` container onto this device, through one of two doors.
    ///
    /// The first door is the vault's. A device that already holds a vault for the *same* account —
    /// the same account id in the saved session, the same root bytes the container carries — is
    /// not a new device at all: it is the same device coming back with its own file, and it signs
    /// in through the tier-one login ceremony, a KNOWN device proving itself with the identity key
    /// and the device credential the vault already holds. No device slot is spent, and the device
    /// keeps the E2EE identity, the ratchets and the safety number every peer has verified: the
    /// vault's keys go back exactly as they came, carrying nothing but the refresh token the grant
    /// rotated in — the unlock path's own shape. (Web parity: the browser client reads its
    /// per-account device record and tries tier one first, silently; on desktop the vault *is* the
    /// device record.) A vault that belongs to a different account, or that the form's passphrase
    /// does not open, keeps the deliberate-removal refusal, because the keys inside an existing
    /// vault are the identity every peer has verified and overwriting them is not something a
    /// restore should be able to do in passing. A tier-one refusal is deliberately not allowed to
    /// fall through to tier two, the way the web client's is: the add-device ceremony behind that
    /// door would spend one of the account's eight device slots and replace this device's verified
    /// identity, so on a device that already holds the account the failure stands and says so.
    ///
    /// The second door, taken when no vault exists, is the add-device ceremony: a fresh vault, and
    /// the session that follows. The restored device holds the root — it can sign future
    /// add-device ceremonies and derive the wallets — but its E2EE identity is fresh and random,
    /// not the founding device's: a restore onto a bare machine is a new device, and new devices
    /// never inherit another device's ratchets. Only the founding device's E2EE history is a
    /// function of the root, and only its own backup restores onto it as itself.
    ImportContainer {
        /// The container file.
        path: PathBuf,
        /// The recovery credential the container was sealed with.
        credential: String,
        /// The passphrase for the vault: the new one this restore creates when the device has
        /// none, or the existing vault's own when the container is the same account coming home —
        /// the door is chosen by whether that passphrase opens a vault of the container's account
        /// (see [`Worker::import_container`]).
        passphrase: String,
        /// The account's username, as the person knows it. The ceremony itself needs only the
        /// account id the container names; the username is stored beside the session so the
        /// unlock screen greets the right person and a later passphraseless login can name the
        /// account to the server, which resolves names and not ids.
        username: String,
        server: ServerEndpoint,
    },
    /// Archive one of the account's registered wallet addresses.
    ArchiveWallet { wallet_id: Id },
    /// Record (or replace) the account's recoverable contact — one string, an email or a phone,
    /// and the server is the judge of the shape.
    SetContact { email_or_phone: String },
    /// Read whether the account *has* a recoverable contact: `GET /v1/auth/contact`.
    ///
    /// The security checkup's Recovery row. Separate from [`Command::SetContact`] for the same
    /// reason the session list is separate from sign-out: the checkup only looks, and a surface
    /// that writes while pretending to read would be a surprise to anyone watching the account.
    ContactStanding,
    /// Change the account's sign-in passphrase.
    ///
    /// Carries both secrets as raw strings for the same reason the auth forms do: the fields are
    /// a person typing, and the worker is where "what they typed is not acceptable" becomes a
    /// sentence worth reading (the server's own validation message) instead of a parse error.
    ChangePassphrase { current: String, next: String },
    /// Rotate the account's ML-DSA identity key, from this device.
    ///
    /// Carries the vault passphrase because the successor key must be sealed into the vault in
    /// the same breath as the ceremony: the worker holds no passphrase after unlock, and a
    /// successor that exists only in memory is a key nobody holds the moment the window closes.
    /// The field is a person typing, and the worker is where "what they typed did not open the
    /// vault" becomes a sentence worth reading — the same rule the passphrase change follows. The
    /// settings pane confirms the consequences before sending, and wipes the secret on submit.
    RotateIdentity { passphrase: String },
    /// Refresh the AVAX balance of the account's first wallet on one network.
    ///
    /// A pull, never a poll (§184): the wallet surface asks when the user asks, and the worker
    /// holds nothing open between asks.
    ChainBalance { network: ChainNetwork },
    /// Build one AVAX transfer: parse the recipient, read the nonce, gas and fees from the
    /// network, and answer with the full transaction the confirm screen must show before
    /// anything is signed (spec #40).
    ChainPrepare {
        network: ChainNetwork,
        /// The recipient as typed. The worker parses it — EIP-55 checksum and all — because a
        /// refusal worth reading is one the worker writes, not a parse error the form has to
        /// translate.
        recipient: String,
        /// The amount as typed, in AVAX. Parsed to wei here for the same reason.
        amount_avax: String,
    },
    /// Sign and broadcast exactly the transaction the confirm screen displayed.
    ///
    /// The prepared values ride back verbatim — the signing path re-derives every field from
    /// them, so what is signed is what was shown, and a tampered `to` fails the EIP-55 checksum
    /// here rather than moving value.
    ChainSend { tx: PreparedTx },
    /// Internal: a tracker task finished following one broadcast transaction. Sent by the
    /// tracker into this worker's own loop, because the Activity list — and its next sealing
    /// into the vault — belongs to the loop, not the task.
    ChainSettled {
        network: ChainNetwork,
        tx_hash: String,
        outcome: String,
        block: Option<u64>,
        gas_used: Option<u128>,
    },
    /// Start a voice call to the one other member of a two-person conversation. The call key is
    /// minted and distributed before the invite, so the callee can unseal the offer the moment
    /// it rings; a call this device is already in — either end of one — is refused by the
    /// worker with a toast rather than a second call.
    StartCall { conversation_id: Id, callee_id: Id },
    /// Answer the ringing incoming call. No payload: one call rings at a time, and the worker
    /// knows which.
    AcceptCall,
    /// Decline the ringing incoming call with the Declined reason.
    DeclineCall,
    /// End whatever call this device is in — a placement, a ring it was about to answer, or a
    /// live call. The worker picks the honest wire message for each (Cancel before the answer,
    /// End after it, nothing at all for a placement that never reached the wire).
    EndCall,
    /// Flip this side's microphone mute. Muting is silence at the capture source, not a
    /// signalling fact: the far side hears quiet, the same mute every other client shows.
    ToggleCallMute,
    /// Put an ended call's overlay away. The call is over on the wire; this only clears the
    /// screen.
    DismissCall,
    /// Join the group call of one conversation — or re-send a join already in flight, because
    /// the call id the join carries is its idempotency key and a press that lands twice must
    /// seat the same call, not a second one. `call_id` names a call already running in the
    /// conversation when the caller knows of one (the header's join-in-progress offer passes
    /// it, so the press seats the running call); `None` lets the worker mint, starting the
    /// conversation's call.
    JoinGroupCall {
        conversation_id: Id,
        call_id: Option<Id>,
    },
    /// Leave the group call of one conversation, seated or still joining.
    LeaveGroupCall { conversation_id: Id },
    /// Attach a local file to a conversation. The worker reads the bytes and judges them the
    /// way the server will: an image the bytes prove is one, anything else is a document.
    ///
    /// The path is read here, in the worker, for the same reason the avatar's is — reading a
    /// file is I/O the UI thread must not own — and the whole three-step upload (ticket, PUT,
    /// commit) plus the message that references it is one command, because it is one action
    /// to the person who clicked Attach.
    SendAttachment {
        conversation_id: Id,
        path: PathBuf,
        /// The disappearing lifetime, as on [`Command::SendText`] — an armed send carries its
        /// promise whatever the body, because "this vanishes" is about the send, not the medium.
        expires_in_ms: Option<u32>,
    },
    /// Begin recording a voice note in a conversation. One recording runs at a time; a second
    /// ask while one runs is ignored (the UI trades its mic button for the recording bar, so
    /// the ask should not be possible). The lifetime is the composer's disappearing arm, when
    /// one is on — captured here because the arm may be switched off before the recording ends,
    /// and the promise was made when the recording began.
    StartRecording {
        conversation_id: Id,
        expires_in_ms: Option<u32>,
    },
    /// Pause the live recording — the speaker's own word. The capture stands down, the timer
    /// and the cap stand still, and nothing recorded so far is touched.
    PauseRecording,
    /// Resume a paused recording.
    ResumeRecording,
    /// Stop the live recording into the preview: the two-step mode's Stop, holding the note
    /// for the composer's Send or Delete rather than sending on the spot.
    StopRecording,
    /// Send the note — the preview's Send, or a hold-mode release while the recorder is still
    /// live, which finalises and sends in one motion.
    SendVoiceNote,
    /// Cancel — the bar's Cancel, the preview's Delete, or a hold-mode slide away. Nothing is
    /// deleted outright: the note stays a draft for the undo window, section 179's rule that
    /// an accidental cancel must be a recoverable mistake.
    CancelVoiceNote,
    /// Restore a cancelled note from its undo window, back into the preview it was stopped at.
    UndoVoiceNoteDiscard,
    /// Offer a draft the last session left behind — the app-death rule of section 179 — as
    /// the preview the composer would show a note it had just stopped. Asked on every
    /// conversation open; the worker ignores it when a note is already live or held.
    RecoverVoiceDraft { conversation_id: Id },
    /// React to one message with one emoji. Add-only, the same shape every Migo client
    /// sends: the server mints a deterministic message id from the envelope, and the UI adds
    /// its own chip on the click rather than waiting for an echo this device suppresses.
    SendReaction {
        conversation_id: Id,
        target_message_id: Id,
        emoji: String,
    },
    /// Withdraw one of this account's own messages for everyone. The server keeps the row as a
    /// tombstone — the sequence numbering has no hole — and fans the tombstone out to every
    /// participant, this device included (as an echo it recognises by id).
    DeleteMessage { conversation_id: Id, message_id: Id },
    /// Replace one of this account's own text messages. The caller passes the replacement
    /// *text*; the worker re-seals it through the same chain that sealed the original, because
    /// the envelope the wire wants is the caller's crypto, and the crypto lives in the worker.
    EditMessage {
        conversation_id: Id,
        message_id: Id,
        text: String,
    },
    /// Block an account. One-sided and silent: the server tears down any friendship in both
    /// directions and tells the blocked party nothing. There is no unblock opcode — the wire
    /// is set-only — so the control that issues this reads "Block" and then "Blocked".
    BlockUser { user_id: Id },
    /// Mute or unmute an account for the caller alone. A volume control, not a verdict: no
    /// teardown, no notification, and a muted account's room messages are simply not drawn.
    MuteUser { user_id: Id, on: bool },
    /// Fetch one attachment for the thread. Deduplicated by the worker: a bubble that asks
    /// twice still costs one fetch.
    FetchMedia { media_id: Id },
    /// Save one fetched attachment's original bytes to a local path. Fetches first when the
    /// cache holds nothing, then writes — the file the sender sent, never a re-encode.
    SaveMedia { media_id: Id, path: PathBuf },
    /// Play one voice note, or stop it if it is the one already playing.
    PlayVoiceNote { media_id: Id },
    /// Stop whatever voice note is playing.
    StopVoiceNote,
    /// Set the voice-note playback speed (§179): 1x, 1.5x, or 2x, done entirely in the
    /// client — the media is never asked for again. A note playing now keeps its position
    /// and only changes pace; a note started later begins at this speed.
    SetVoiceSpeed { speed: VoiceSpeed },
    /// Mark one voice note listened or unlistened on this device, by hand. §179's
    /// receiver-local rule: nothing is sent, the sender is never told, and an unmark does
    /// not unsay a receipt that already went.
    SetVoiceNoteListened { media_id: Id, listened: bool },
    /// Internal: a playback pump crossed the listened threshold — the note has been heard
    /// to (near) its end. Sent by the playback thread into this worker's own loop, the same
    /// way [`Command::VoiceNoteEnded`] is, because the marks belong to the loop, not the
    /// thread.
    VoiceNoteHeard { media_id: Id },
    /// Internal: a voice note finished playing on its own — the pump ran out of samples.
    /// Sent by the playback thread into this worker's own loop, the same way a chain
    /// tracker's ending is, because the playing state belongs to the loop, not the thread.
    VoiceNoteEnded { media_id: Id },
    /// Stop the worker. Sent on window close.
    Shutdown,
}

/// What the owner's admin surface holds between asks, as one answer.
///
/// `Closed` rather than an error: an account that is neither owner nor admin asked honestly and
/// the answer is "not yours to open", which is not a failure of the request. A real refusal —
/// unreachable server, unknown route — stays a `Failed` with its reason, because "could not
/// check" and "not yours" are different sentences and only one of them is a complaint.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum AdminsAnswer {
    /// Asked, not answered.
    #[default]
    Loading,
    /// The caller is this deployment's owner, and these are the admins.
    Owner(Vec<crate::model::AdminRow>),
    /// The caller holds neither role. The surface exists; it is not theirs.
    Closed,
    /// The ask itself failed, for a reason safe to show as-is.
    Failed(String),
}

/// What the worker tells the UI.
#[derive(Debug)]
pub enum Event {
    /// The connection state changed.
    Connection(Connection),
    /// The session's negotiated RICH_PRESENCE bit, exactly as WELCOME stated it.
    ///
    /// Sent on every successful connect, reconnects included, because the negotiated set is
    /// per-session: a node that drops the bit (a kill switch, an older server behind the same
    /// address) must not leave a pane offering an edit the new session would refuse. The one
    /// consumer today is the profile pane's status field, which is editable only when the bit
    /// is in the intersection — the brief's own rule that a feature nobody negotiated is not
    /// asked for at all.
    RichPresence(bool),
    /// A vault exists on disk, so the first screen should offer to unlock it.
    ///
    /// Carries nothing: the account name and the server address are inside the sealed body, so there
    /// is nothing about the account this event could truthfully report before the passphrase arrives.
    VaultFound,
    /// No vault exists, so the first screen should offer to register or sign in.
    VaultMissing,
    /// Signed in. Carries the safety number, which is derived from local keys only.
    SignedIn(Account),
    /// Signed out, by request or because the server revoked the session.
    SignedOut,
    /// A captcha challenge arrived for the auth forms.
    ///
    /// Carries the wire view as-is: the picture stays base64 until the form decodes it, because
    /// the decode belongs with the texture it feeds, on the UI thread.
    CaptchaChallenge(CaptchaChallenge),
    /// A captcha challenge could not be fetched. `reason` is safe to show as-is.
    ///
    /// Separate from the connection state on purpose: a form whose challenge will not load is
    /// not a form whose sign-in failed, and reporting it as one would release a busy flag that
    /// was never set.
    CaptchaUnavailable { reason: String },
    /// The server refused a submit over the captcha: wrong, expired, or missing proof.
    ///
    /// One event for all three because they differ only in the telling. What each means to a
    /// form is identical: the challenge it holds is dead, so drop it and fetch another, and
    /// keep the form on screen for the next attempt.
    CaptchaRefused,
    /// The full conversation list.
    Conversations(Vec<Conversation>),
    /// The server's answer to a conversation this client asked to create.
    ///
    /// Sent separately from [`Event::Conversations`] because asking is already intent: every
    /// caller of `StartDirect` — a friend's Message action, a search hit, the new-chat field —
    /// asked because it wants the thread open, so the id arrives on its own and the shell can
    /// open the tab without guessing which conversation in the refreshed list is the new one.
    ConversationCreated { conversation_id: Id },
    /// A group's roster arrived: every member with role, join time, mute, and departure, in the
    /// server's order (active first by join time, then the departed as they left).
    ///
    /// The roster is a different fact from the member cache the send path keeps: the cache is an
    /// encryption audience of ids, the roster is the panel a person reads — roles, mutes and all.
    /// Both are fed by the same wire answer; this event carries the panel's copy.
    GroupRoster {
        conversation_id: Id,
        members: Vec<crate::model::RosterMember>,
    },
    /// A group's membership moved: somebody joined, left, was removed, or was voted out.
    ///
    /// The group twin of [`Event::RoomMember`]: the same wire enum, the same notice-line shape,
    /// keyed by the conversation because a group has no room id to key by. The sender-key
    /// rotation the movement demands happens in the worker, below this event.
    GroupMember {
        conversation_id: Id,
        user_id: Id,
        change: migo_protocol::MemberChange,
    },
    /// A group's metadata moved as a delta: the title changed, because a founder renamed it.
    ///
    /// Only the title exists on this wire today; the event stays a delta (absent means
    /// unchanged) so a field the server adds later has a shape to arrive in.
    GroupRenamed { conversation_id: Id, title: String },
    /// A kick vote's tally, as this account's own voice landed in it.
    ///
    /// Sent for the reply the caller sees — "2 of 4, still open" — while the same tally for
    /// everyone else arrives as [`Event::GroupVoteBroadcast`]. `open: false` is the moment the vote
    /// carried and the removal happened (the member event for it follows separately).
    GroupVoteStatus {
        conversation_id: Id,
        target_id: Id,
        votes: u32,
        needed: u32,
        member_count: u32,
        open: bool,
    },
    /// A running kick vote's tally, for everyone the vote concerns.
    ///
    /// `closed: Some(true)` is a vote that ended without passing — expired, or its target left;
    /// the UI stops drawing the tally it was showing. The newest tally per conversation is the
    /// one that matters, the same coalescing rule the wire states. Not named `GroupVoteEvent`
    /// despite mirroring the wire's `ConversationVoteEvent`: clippy's variant-name lint aside,
    /// the enum already has a `GroupVoteStatus` for the caller's own reply and the two are
    /// easiest to tell apart when the broadcast spells what it is.
    GroupVoteBroadcast {
        conversation_id: Id,
        target_id: Id,
        votes: u32,
        needed: u32,
        member_count: u32,
        closed: Option<bool>,
    },
    /// A leave was accepted: the group's thread goes with the person, because everything the
    /// worker held under that conversation — the sender-key chain, the ratchets, the held
    /// messages — was dropped before this event crossed the channel.
    GroupLeft { conversation_id: Id },
    /// A page of history, oldest first, and whether the walk that fetched it stopped short.
    ///
    /// `more` is the walk's own honest edge: true when the page budget ran out with the server
    /// still holding history above what arrived — the thread's cue to offer the load-earlier
    /// row, which pages the rest down from the newest. False when the walk reached the live
    /// edge, and deliberately absent-minded when it is false for other reasons (a truncated
    /// hole the server can no longer fill): an established `more` is never cleared by a walk,
    /// only by the load-earlier row's own downward walk reaching the bottom.
    History {
        conversation_id: Id,
        messages: Vec<Message>,
        more: bool,
    },
    /// A page of older history, paged downwards by the load-earlier row's ask.
    ///
    /// Carries `from_seq` — the page's lowest sequence — because that is the next ask's cursor,
    /// and `more` for the same reason [`Event::History`] does: false is the server saying the
    /// walk downwards has reached the bottom, which is the row's cue to withdraw itself.
    HistoryEarlier {
        conversation_id: Id,
        messages: Vec<Message>,
        from_seq: u64,
        more: bool,
    },
    /// One live message.
    Message(Message),
    /// The server accepted an outgoing message and assigned it a sequence number.
    Accepted {
        message_id: Id,
        conversation_id: Id,
        seq: u64,
    },
    /// An outgoing message could not be sent.
    SendFailed { message_id: Id },
    /// Someone else's receipt watermark moved: they delivered or read up to `seq`.
    ///
    /// The UI's read marker turns on this. Only `Read` watermarks mark messages read; a
    /// `Delivered` one may arrive first (the server advances Delivered before Read) and the
    /// marker must not treat it as read. Own receipts never arrive here — [`Self::on_receipt`]
    /// drops them — so the recipient of this event is always a peer.
    Receipt {
        conversation_id: Id,
        user_id: Id,
        kind: migo_protocol::ReceiptKind,
        seq: u64,
    },
    /// Someone started or stopped typing.
    Typing {
        conversation_id: Id,
        user_id: Id,
        typing: bool,
    },
    /// Display names for account ids, so the UI can title a direct conversation.
    Names(HashMap<Id, String>),
    /// The social graph moved: friendships and pending requests, reduced to model rows.
    ///
    /// Followed by names and presence for the same ids wherever the server discloses them,
    /// because a friends list of bare ids is a list of strangers.
    Relationships(Vec<Relationship>),
    /// Someone acted on the social graph and this account was the audience: a request arrived,
    /// or one of ours was accepted.
    ///
    /// Carries the actor and what the state string said, as far as it said anything: `Some(true)`
    /// an acceptance, `Some(false)` a request, `None` a state this build has no name for. All
    /// three mean the graph moved — the difference is only whether there is anything true to
    /// say about it beyond "look again".
    FriendChanged { user_id: Id, accepted: Option<bool> },
    /// An account's presence changed, from a presence event or a profile fetch. `Unknown` is
    /// never carried: unobserved is the absence of an event, not one.
    PresenceChanged { user_id: Id, state: model::Presence },
    /// The device/session list for the settings screen, or the reason it could not be had.
    ///
    /// A failure is a fact the panel must keep showing — "could not check" and "no other
    /// devices" are different states and only one of them should reassure anybody — so it rides
    /// the same event rather than dying as a toast.
    Sessions(Result<Vec<SessionRow>, String>),
    /// The public room directory, reduced to rows.
    Rooms(Vec<RoomRow>),
    /// A join (or create) was accepted and its conversation is ready to open.
    RoomJoined {
        conversation_id: Id,
        room_id: Id,
        title: String,
    },
    /// A leave (or a removal) took the room away from this account: the rooms pane drops the
    /// room from its joined set, and when the bridge map still knew the conversation, the
    /// thread's window closes the way a group's own departure closes it — everything the
    /// worker held under that conversation was dropped before this event crossed the channel.
    ///
    /// `self_left` separates the two wordings the toast owes: a leave the person asked for
    /// ("Left the room") and a removal done to them ("You were removed from the room"). The
    /// conversation is `None` only when the bridge map had already forgotten the room, in
    /// which case there is no thread left to close either.
    RoomLeft {
        room_id: Id,
        conversation_id: Option<Id>,
        self_left: bool,
    },
    /// Someone came, went, dropped, came back, or was removed in a room this account watches.
    ///
    /// Carries the room's own wire event as far as the room id, member id, the change enum, and the
    /// running member total — the fields a notice line and a live count are drawn from. Not the
    /// decrypted display name: that is a profile fetch away, and the chat pane resolves it the way
    /// it resolves a direct conversation's title.
    RoomMember {
        room_id: Id,
        user_id: Id,
        change: migo_protocol::MemberChange,
        member_count: Option<u32>,
    },
    /// A watched room's counters moved: the online tally, the member total, or the capacity ceiling.
    ///
    /// Each field is a delta — absent means unchanged — so the model folds it onto what it holds
    /// rather than replacing a snapshot.
    RoomState {
        room_id: Id,
        online_count: Option<u32>,
        member_count: Option<u32>,
    },
    /// The durable notification inbox, newest first.
    Alerts(Vec<AlertRow>),
    /// A notification was pushed: the cue to re-read whatever inbox-shaped surface is showing.
    AlertPushed,
    /// A game in a watched conversation moved: started, played, or finished.
    ///
    /// The published delta as the wire put it, with the IDL's `room_id` already read as the
    /// conversation it names (one subject, two names). It is a delta, not a state — the board
    /// lives in GAME_VIEW's answer — so the only honest consumer is a feed that appends the line,
    /// and a surface that wants the score asks for the view.
    GamePushed {
        conversation_id: Id,
        game_id: Id,
        event: String,
        actor_id: Option<Id>,
        /// The board version every event of one move shares, for a consumer that orders by it.
        state_version: u64,
    },
    /// The caller's wallet moved on the server: a spend made from any session of the account.
    ///
    /// The economy twin of [`Event::AlertPushed`]: a cue to re-read, never a fact. The wire's
    /// event names the kind and the amount but never the resulting balance, so the only honest
    /// reaction is to ask for the wallet again.
    EconomyPushed,
    /// The caller's wallet: the MIG coin balance, the points balance, and the Kick Point balance.
    ///
    /// `kick_points` is optional the way the wire's field is: a node that predates the currency
    /// reads as `None`, and the surface shows the dash rather than a zero it cannot stand behind.
    Balance {
        coins: u64,
        points: u64,
        kick_points: Option<u64>,
    },
    /// The wallet's statement, newest first.
    Ledger(Vec<LedgerRow>),
    /// The caller's XP progression.
    ProgressionArrived(Progression),
    /// The caller's badges, by code.
    Badges(Vec<String>),
    /// The XP leaderboard page.
    Leaderboard(Vec<LeaderRow>),
    /// The gift catalogue: SKU, name, price, category.
    Gifts(Vec<GiftRow>),
    /// Accounts found by search or offered as suggestions.
    People(Vec<PersonRow>),
    /// The account's devices for the security panel, or the reason they could not be had.
    ///
    /// A failure rides the same event rather than dying as a toast for the same reason the session
    /// list's does: "could not check" and "you have one device" are different facts and only one
    /// should reassure anybody.
    Devices(Result<Vec<DeviceRow>, String>),
    /// The signed-in account's own profile card, or the reason it could not be had.
    ///
    /// The same shape `Devices` pins: the pane's "not loaded yet" and "the server would not say"
    /// are different sentences, and only the second one is a complaint.
    OwnProfile(Result<crate::model::OwnProfile, String>),
    /// The profile pane's save was accepted: the reply is the refreshed card, the same shape the
    /// fetch returns, so the pane replaces its copy from the reply instead of re-reading.
    ProfileSaved(crate::model::OwnProfile),
    /// Another member's profile card, answering the member menu's "View profile".
    ///
    /// Carries the conversation whose window asked, because the card's own reply names only
    /// the account: the view belongs to the window that opened it, and a second group's menu
    /// must not steal a card the first group is still showing. The card arrives whole or not
    /// at all — a profile the server would not disclose leaves the menu's ask unanswered, and
    /// the view says nothing rather than half-naming somebody.
    MemberProfile {
        conversation_id: Id,
        card: crate::model::MemberCard,
    },
    /// Another member's XP progression, answering the member view's standing ask. Same routing
    /// rule as the card: the window that asked is the one that draws it.
    MemberProgression {
        conversation_id: Id,
        progression: Progression,
    },
    /// Another member's badges with the days they were earned, answering the same ask.
    MemberBadges {
        conversation_id: Id,
        badges: Vec<crate::model::BadgeRow>,
    },
    /// Another member's position on the XP board's first page — `None` when they stand off it,
    /// which the view draws as no rank line rather than a guess.
    MemberRank {
        conversation_id: Id,
        position: Option<u32>,
    },
    /// The caller's edge to one account, answering the member view's social-line ask. `None`
    /// is "no edge the graph names" — drawn as no line, exactly like the web card's unknown.
    MemberEdge {
        conversation_id: Id,
        kind: Option<RelationshipKind>,
    },
    /// The account's owned catalogue codes, for the composer's picker: the emoticon packs and
    /// sticker packs the picker's tabs draw.
    Entitlements(Vec<String>),
    /// An avatar upload was refused — the file would not read, the server declined the bytes,
    /// or the profile patch behind them was rejected.
    ///
    /// Filed beside the pane's own failure line rather than toasted, for the same reason the
    /// profile save's refusals are: the person is looking at the button that started it.
    AvatarChangeFailed { reason: String },
    /// The owner's admin management surface: the standing first, and with it the list.
    ///
    /// The standing is carried rather than remembered by the UI because it is a server fact,
    /// not a UI preference: a stale client that kept the surface open after the owner
    /// designation moved would draw a management page it no longer owns. `Closed` is the
    /// honest answer for an account that holds neither role, drawn as a sentence — the same
    /// treatment the web client gives it.
    Admins(AdminsAnswer),
    /// An admin grant or revoke was refused. Filed rather than toasted because it belongs
    /// beside the form or row that caused it.
    AdminChangeFailed { reason: String },
    /// The account's registered wallet addresses.
    Wallets(Result<Vec<EvmWalletRow>, String>),
    /// When this device last sealed a `.migo` container, in unix seconds — `None` when it never
    /// has, or when a rotation retired the last container's right to vouch the account.
    ///
    /// Sent once at sign-in (from the vault, so the checkup's Backup row is honest before any
    /// click) and again after every export and every completed rotation, because those are the
    /// only two moments the fact moves. A local fact, not a server one: it rides no wire.
    BackupState { last_backup_at: Option<u64> },
    /// Whether the account has a recovery contact on file, or the reason the server would not
    /// say. The same honest-uncertainty shape the device list pins: "could not check" and
    /// "not configured" are different sentences, and only the second one is advice.
    ContactStanding(Result<bool, String>),
    /// The AVAX balance of the account's first wallet, in wei, on the network asked.
    ///
    /// The EIP-55 address rides along because the same read is what discovers it. A `None`
    /// address is not an error state of the network: it is this device not holding the account
    /// root, which is a fact about the device and worth its own sentence on the wallet surface.
    ChainBalance {
        network: ChainNetwork,
        address: Option<String>,
        balance: Result<u128, String>,
    },
    /// A built AVAX transfer ready for the confirm screen, or the reason nothing could be built.
    ChainPrepared(Result<PreparedTx, String>),
    /// A broadcast was accepted — carries the tx hash, which is *acceptance*, never confirmation
    /// (spec #41) — or the reason the endpoint refused it.
    ChainSent(Result<String, String>),
    /// The tracker passed through a state for one transaction: `PENDING` on first sight, or the
    /// ending it reached. Progress, so the wallet surface can show the ladder honestly.
    ChainState { tx_hash: String, state: String },
    /// A tracker finished following one transaction. The ending is spec #41's own word; the
    /// Activity list arrives separately, already reduced.
    ChainSettled { tx_hash: String, outcome: String },
    /// This account's tracked AVAX transactions (Activity), newest first. Sent at sign-in and
    /// after every send and settle, because the list is the worker's to keep, not the UI's.
    ChainActivity(Vec<ChainTxRow>),
    /// One peer device's E2EE identity, as a key-bundle response observed it.
    ///
    /// Carries the *pair* safety number — this device's and that peer device's identity keys in
    /// one number, [`model::pair_safety_number`]'s cross-client derivation, grouped for reading —
    /// and whether the peer's fingerprint differed from the last one this vault sealed for that
    /// device, which is the §47/§164 moment the conversation window has to draw a warning for.
    /// Sent for every observed device, not only changed ones: the verification block shows the
    /// numbers themselves, and a device that never changed is the baseline the reader is comparing
    /// against.
    PeerIdentity {
        user_id: Id,
        device_id: Id,
        safety_number: String,
        changed: bool,
    },
    /// The one call this device is in — placement, ring, answer, or live call — projected for the
    /// overlay. Arrives whenever the projection changes, so the overlay is always a frame behind
    /// the truth at most. `None`-shaped news travels as [`Event::CallGone`].
    Call(call::CallView),
    /// The call overlay should go away: the last call ended and was dismissed (or was never
    /// this device's to show anymore). Separate from [`Event::Call`] because "no call" is not a
    /// view, and an `Option` in every event would tax every other reader of the enum.
    CallGone,
    /// This device was seated in a group call — the roster snapshot answered its join — or the
    /// roster moved and the count changed. `participant_count` is seats, one per account, so
    /// the UI's badge is the number of people in the call, not devices.
    GroupCallSeated {
        conversation_id: Id,
        participant_count: u32,
    },
    /// The group call of one conversation is over for this device: left, ended, dropped with
    /// the gateway, or torn down with the conversation itself.
    GroupCallEnded { conversation_id: Id },
    /// A group call is running in a conversation this device holds no seat in — news the
    /// conversation topic's announcements carry to every member, seated or not. The header's
    /// join button reads this as "join what is running", passing `call_id` back to the join
    /// so the press seats the running call rather than minting a second one beside it.
    GroupCallInProgress {
        conversation_id: Id,
        call_id: Id,
        /// The roster's size after the change the announcement named.
        count: u32,
    },
    /// The running call of one conversation is over — its last seat left — so the header's
    /// join-in-progress offer goes with it. The spectator's twin of
    /// [`Event::GroupCallEnded`], kept separate because the seated and spectated calls of
    /// one conversation are different facts with different lifetimes.
    GroupCallInProgressEnded { conversation_id: Id },
    /// A fetched image decoded: the pixels the bubble's texture wants, at their own size.
    ///
    /// Decoding happens in the worker — before the ask the bytes are sealed, and after it
    /// they are an encoded format, and neither shape is one a paint loop should parse.
    MediaImage {
        media_id: Id,
        width: u32,
        height: u32,
        rgba: Vec<u8>,
    },
    /// An attachment could not be had: the fetch failed, the seal refused, or the bytes
    /// decoded as nothing this build can show. `reason` is safe to show as-is.
    ///
    /// Filed against the media id rather than toasted, so the failure lands in the bubble
    /// it belongs to and a later press can try again.
    MediaFailed { media_id: Id, reason: String },
    /// A voice-note recording began, so the composer trades its field for the recording bar.
    RecordingStarted { conversation_id: Id },
    /// The live recording's own state, on the recording's own tick: how long it has run
    /// (standing still through a pause exactly as the capture does), whether it is paused,
    /// and the waveform's sampled bars so the bar can draw what is being said.
    RecordingProgress {
        conversation_id: Id,
        elapsed_ms: u64,
        paused: bool,
        amplitudes: Vec<u8>,
    },
    /// The capture ended — into the preview, into a cancel, or into a send. The bar goes away.
    RecordingStopped { conversation_id: Id },
    /// A finished note waits on the composer's word: the two-step mode's preview, an undo
    /// window's restore, or a draft recovered after an app death. The duration and waveform
    /// are the sender's own measurements, so the preview lays out before any byte is read.
    RecordingPreview {
        conversation_id: Id,
        duration_ms: u32,
        waveform: Vec<u8>,
    },
    /// A note was discarded: `undoable` says whether the undo window holds it (the chip is
    /// shown) or the window has just closed and the bytes are gone (the chip is withdrawn).
    NoteDiscarded { conversation_id: Id, undoable: bool },
    /// A voice note started playing; the bubble's button becomes a stop.
    VoicePlaying { media_id: Id },
    /// A voice note stopped playing — the stop button, or the last sample itself.
    VoiceStopped { media_id: Id },
    /// The listened marks for the account that just signed in, read back from this device's
    /// store: every voice note this account has heard to (near) its end or marked by hand.
    /// The whole set at once, so a fresh sign-in replaces whatever a previous account left
    /// on screen. §179's receiver-local state — nothing behind this event was ever sent.
    VoiceNotesListened { media_ids: Vec<Id> },
    /// One voice note became listened — played through to (near) its end. The mark is this
    /// device's own memory, and the sender is not told.
    VoiceNoteListened { media_id: Id },
    /// Something worth a line at the bottom of the window.
    Toast { text: String, kind: ToastKind },
}

/// The UI's handle on the worker.
pub struct Net {
    commands: mpsc::UnboundedSender<Command>,
    events: std_mpsc::Receiver<Event>,
    /// Kept so the worker thread is joined on drop rather than abandoned mid-write to the vault.
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Net {
    /// Starts the worker thread and returns a handle to it.
    ///
    /// `ctx` is cloned into the worker so an arriving event can wake the UI. Without it egui would
    /// only notice a new message the next time something else caused a repaint — which, on an idle
    /// window, is never.
    pub fn spawn(ctx: egui::Context, vault_path: PathBuf) -> Self {
        let (command_tx, command_rx) = mpsc::unbounded_channel();
        // The worker keeps its own sending end, so a tracker task can report its ending into the
        // loop that owns the Activity list; cloning before the thread takes the receiver keeps
        // this handle's `send` alive for the UI.
        let worker_commands = command_tx.clone();
        let (event_tx, event_rx) = std_mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("migo-net".to_owned())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_multi_thread()
                    // Two threads: one for the socket, one for anything that blocks briefly. The
                    // default is one per core, which for a client that holds a single connection is
                    // several megabytes of stacks doing nothing.
                    .worker_threads(2)
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(_) => {
                        let _ = event_tx.send(Event::Toast {
                            text: "could not start the network thread".to_owned(),
                            kind: ToastKind::Error,
                        });
                        ctx.request_repaint();
                        return;
                    }
                };
                let sink = Sink {
                    events: event_tx,
                    ctx,
                };
                runtime.block_on(Worker::new(sink, worker_commands, vault_path).run(command_rx));
            })
            .ok();
        Self {
            commands: command_tx,
            events: event_rx,
            thread,
        }
    }

    /// Queues a command. Silently dropped once the worker has stopped, which only happens at exit.
    pub fn send(&self, command: Command) {
        let _ = self.commands.send(command);
    }

    /// Takes the next event, or `None` if there is nothing waiting.
    ///
    /// Never blocks: this is called from the paint loop, and a blocking read there is the freeze this
    /// whole module exists to avoid.
    pub fn try_recv(&self) -> Option<Event> {
        self.events.try_recv().ok()
    }
}

impl Drop for Net {
    fn drop(&mut self) {
        self.send(Command::Shutdown);
        if let Some(thread) = self.thread.take() {
            // Joined rather than detached: the worker may be part-way through writing the vault, and
            // a process that exits during that write loses the identity key.
            let _ = thread.join();
        }
    }
}

/// The worker's end of the event channel, paired with the egui context to wake.
#[derive(Clone)]
struct Sink {
    events: std_mpsc::Sender<Event>,
    ctx: egui::Context,
}

impl Sink {
    fn send(&self, event: Event) {
        if self.events.send(event).is_ok() {
            self.ctx.request_repaint();
        }
    }

    fn toast(&self, text: impl Into<String>, kind: ToastKind) {
        self.send(Event::Toast {
            text: text.into(),
            kind,
        });
    }
}

/// One signed-in session's worth of state.
/// One conversation's cached membership: the member ids, and whether the set is the whole
/// membership or a list row's capped preview of it. The distinction is the whole bug this type
/// exists for — see [`Signed::members`].
#[derive(Debug, Clone, Default)]
struct MemberCache {
    ids: Vec<Id>,
    complete: bool,
}

impl MemberCache {
    /// Applies one membership movement onto the cache, as a `CONVERSATION_MEMBER_EVENT` reports
    /// it. A join adds the account; a leave, kick, or ban removes it; anything else (a connect
    /// or disconnect is a *presence* fact, an unknown change is not a fact at all) leaves the
    /// set alone. `complete` is never touched: a preview that already truncates cannot be made
    /// whole by patching it — only the roster read promotes.
    fn apply(&mut self, event: &migo_protocol::ConversationMemberEvent) {
        self.apply_change(event.user_id, event.change);
    }

    /// The same movement, factored so a *room* member event can apply it too: the room stream
    /// reports the same joins and departures the conversation stream does, it just names the
    /// room — the caller bridges room→conversation and hands the account and the change here.
    fn apply_change(&mut self, user_id: Id, change: migo_protocol::MemberChange) {
        match change {
            migo_protocol::MemberChange::Joined => {
                if !self.ids.contains(&user_id) {
                    self.ids.push(user_id);
                }
            }
            migo_protocol::MemberChange::Left
            | migo_protocol::MemberChange::Kicked
            | migo_protocol::MemberChange::Banned => {
                self.ids.retain(|id| *id != user_id);
            }
            _ => {}
        }
    }

    /// Promotes the cache with a roster's answer: the active ids replace the preview wholesale
    /// — a merge would keep a preview's truncation and call the result complete. An empty
    /// active set is refused for the same reason the SDK refuses one: a conversation the roster
    /// says has nobody in it is an answer no audience can be built from, and whatever the
    /// preview named is the better guess until a fuller answer says otherwise.
    fn promote(&mut self, active: Vec<Id>) {
        if !active.is_empty() {
            self.ids = active;
            self.complete = true;
        }
    }
}

struct Signed {
    server: ServerEndpoint,
    rest: Rest,
    account: Account,
    access_token: String,
    sessions: SessionStore,
    /// The sender-key layer: one outbound chain per conversation, one receiver per remote sender
    /// device. Every content message — rooms and direct chats both — is sealed here, the same
    /// architecture the web and Android clients speak; the pairwise layer below it now carries
    /// only key distributions.
    groups: GroupStore,
    /// Messages held because the sender's distribution has not arrived yet, keyed by
    /// `(conversation, sender device)`.
    ///
    /// The SDK holds these for the same reason: the pairwise channel and the group fan-out are
    /// two transports, and a content message can win the race against its own key. Bounded per
    /// sender by [`MAX_PENDING_PER_SENDER`], drained when the distribution arrives.
    pending: HashMap<(Id, Id), Vec<migo_protocol::MessageEvent>>,
    /// Prekey bundles already fetched, by device id, so a conversation does not refetch per message.
    bundles: HashMap<Id, migo_crypto::x3dh::PrekeyBundle>,
    /// Which devices belong to which account, learned from KEY_BUNDLE responses.
    devices: HashMap<Id, Vec<Id>>,
    /// Members of each conversation, with whether the set is the whole membership.
    ///
    /// A conversation-list row carries only a *preview* — the server caps each row's members
    /// field at `MEMBER_PREVIEW`, enough to render the row and far short of an encryption
    /// audience. A cache seeded from a list row is therefore incomplete, and the first send to
    /// it reads the roster (the whole truth, [`Opcode::ConversationRoster`]) before choosing
    /// who the sender key is sealed for; a summary from a create or a join answers with the
    /// whole membership and seeds a complete entry. Member events patch the set in place —
    /// they are deltas, and a complete cache they patch stays correct — but never promote an
    /// incomplete one: a preview that already truncates cannot be made whole by adding one.
    members: HashMap<Id, MemberCache>,
    /// Conversation topics this session has already subscribed to.
    ///
    /// The hub delivers a topic's events only to subscribed sessions, so the desktop client that
    /// never subscribes hears nothing: no live messages, no receipts, no room events. The ids are
    /// tracked like the presence watches below so a list re-read does not re-send a SUBSCRIBE per
    /// conversation; cleared when the gateway reconnects, because subscriptions live and die with
    /// the session that held them.
    conversations_watched: HashSet<Id>,
    /// Room topics this session has subscribed to — the room's own event stream (member and state
    /// deltas), the one place the wire ever names a room's live totals.
    ///
    /// Tracked so a list re-read or a reconnect does not re-send a SUBSCRIBE per room. Unlike the
    /// two sets above this one is *not* cleared on reconnect: the subscriptions die with the
    /// session, but the room ids are still the ones the account is in, so the reconnect path
    /// re-subscribes from it rather than forgetting which rooms the session cared about. Freed on
    /// leave — the server's membership gate would refuse the topic anyway, but a set that grows
    /// with every room ever entered is a slow leak.
    rooms_watched: HashSet<Id>,
    /// Room id → the conversation behind it, remembered from the join (the one wire moment that
    /// names both). Needed at leave time: the ack names only the room, but the crypto state and
    /// the membership bookkeeping are keyed by conversation.
    room_conversations: HashMap<Id, Id>,
    /// Account ids whose user topics this session has already subscribed to for presence.
    ///
    /// Tracked so a relationship refresh does not re-send a SUBSCRIBE for every friend on every
    /// reconnect; cleared when the gateway reconnects, because subscriptions live and die with
    /// the session that held them.
    watched: HashSet<Id>,
    /// Which conversations are end-to-end encrypted, filed from every list read and create
    /// answer because the summary is the one wire moment that states it.
    ///
    /// The upload path turns on this fact: an end-to-end conversation's attachment is sealed
    /// before any bytes cross the wire, a room's travels as plaintext under the server's own
    /// content policy (documents excepted — the one kind every client seals even into rooms).
    /// A conversation this session has not been told about is guessed sealed: the wrong guess
    /// fails an upload that can be retried, while the reverse would upload plaintext into an
    /// encrypted conversation.
    e2e: HashSet<Id>,
    /// What every seen media reference said about its object: the key and nonce that open the
    /// download, the claimed type, and whether it is a voice note (which is both its seal
    /// domain and how it is served).
    ///
    /// Filed from every path that decrypts content — live, history, held-and-drained — and
    /// from our own commits, because the keys travel only inside the message: the one message
    /// a session misses is the one whose attachment can never be fetched again. Keyed by
    /// media id, the one name the fetch wire knows.
    media_keys: HashMap<Id, MediaKeying>,
    /// The sequencing account per conversation: the contiguous watermark, the furthest seq
    /// seen, and where a stalled catch-up walk gave up. Held here rather than in the UI's
    /// message store because every event consumes a seq — the key exchanges and the
    /// tombstones included — and the watermark must count events the renderer never sees
    /// (§152: the seq is a prefix of the event stream, not of the rendered one).
    sequences: HashMap<Id, SeqAccount>,
}

/// What one media reference said about its object: the slots that open it, and how to serve
/// it once opened.
#[derive(Clone)]
struct MediaKeying {
    /// The key from the message's key slot: fresh when the upload sealed, all-zero when the
    /// object is a room's plaintext.
    key: Vec<u8>,
    /// The nonce paired with the key.
    nonce: Vec<u8>,
    /// The MIME type the message claimed — what a save or a save-dialog is named after.
    mime_type: String,
    /// True when the object is a voice note: its seal domain is `migo-voice` and it is
    /// served by playing, not by showing.
    voice: bool,
}

/// One avatar upload in flight: the bytes the worker read, waiting for the ticket that says
/// where they go.
struct AvatarPending {
    /// The path, kept for the failure sentences that name it.
    path: PathBuf,
    /// The bytes themselves, held until the ticket's URL is known and they can be PUT.
    bytes: Vec<u8>,
}

/// One attachment upload between its `MEDIA_UPLOAD_BEGIN` and the ticket's arrival. Keyed by
/// the correlation the reply will carry, because more than one attachment may be in flight
/// and the wire's reply names nothing but the correlation.
struct AttachmentBegin {
    /// The conversation the message will land in.
    conversation_id: Id,
    /// The optimistic row's id, minted now so the commit's message send reuses it — the same
    /// id the ACCEPTED acknowledgement will move from sending to sent.
    message_id: Id,
    /// What the message will claim, and the key slots that open the object.
    plan: media::OutgoingMedia,
    /// The bytes for the PUT: the sealed blob for an end-to-end upload, the plaintext for a
    /// room's.
    wire_bytes: Vec<u8>,
    /// When the attachment is a voice note, the conversation its draft belongs to. The draft
    /// outlives the upload on purpose: it is cleared only when the object is committed, and
    /// every failure on the way hands the note back to the preview rather than deleting
    /// five minutes of speech over one dropped request.
    voice_note: Option<Id>,
}

/// One attachment upload between its `MEDIA_UPLOAD_COMMIT` and the acknowledgement that says
/// the object exists.
struct AttachmentCommit {
    /// The conversation the message will land in.
    conversation_id: Id,
    /// The optimistic row's id, the same one the BEGIN minted.
    message_id: Id,
    /// The object's id — the upload ticket's, which the message's content will name and
    /// every later fetch will use. The commit's acknowledgement carries only `ok`, so it
    /// rides here.
    media_id: Id,
    /// What the message will claim, key slots included.
    plan: media::OutgoingMedia,
    /// The voice note's conversation, carried the whole way so the commit's success is the
    /// one door the draft leaves by.
    voice_note: Option<Id>,
}

/// One fetch waiting on its signed URL, keyed by the correlation the reply will carry.
struct MediaWant {
    /// The object being fetched.
    media_id: Id,
    /// What the download is for once it opens.
    intent: MediaIntent,
}

/// What a fetched attachment is being fetched *for* — the one fact that survives the fetch,
/// because it decides how the opened bytes are served.
enum MediaIntent {
    /// Show it in the thread.
    Show,
    /// Play it as a voice note.
    Play,
    /// Write the original bytes to this path.
    SaveTo(PathBuf),
}

/// A bounded cache of opened attachments, keyed by media id, oldest first.
///
/// Bounded because a thread full of images is a scrolling session's worth of megabytes and
/// the seal's decryption work should be paid once per attachment, not once per scroll. The
/// bound is bytes, not entries: a document and a thumbnail cost what they cost. Eviction is
/// oldest-first and never empties the cache for one oversized entry — a note bigger than the
/// whole budget is better playable than dropped for being large.
struct MediaCache {
    entries: VecDeque<(Id, media::CachedMedia)>,
    cost: usize,
}

/// The cache's byte budget: enough for a handful of images or a conversation's voice notes,
/// small enough that a long session's media cannot grow the worker without end.
const MEDIA_CACHE_MAX_BYTES: usize = 32 * 1024 * 1024;

/// How often the live recording states itself: the cadence the bar's clock and waveform
/// move at, fast enough to feel live and slow enough that a repaint is not the recording's
/// main cost. The Android recorder's own tick runs at 100ms on a phone's composer; a
/// desktop frame does not need to wake that often to look awake.
const RECORDING_TICK_MS: u64 = 250;

/// How long a discarded note stays recoverable — the undo window section 179 keeps open
/// because a slide nobody meant is the mistake the hold mode makes easy. The Android
/// composer's `NOTE_UNDO_MS`, the same five seconds.
const NOTE_UNDO_WINDOW: Duration = Duration::from_secs(5);

impl MediaCache {
    fn new() -> Self {
        Self {
            entries: VecDeque::new(),
            cost: 0,
        }
    }

    fn get(&self, id: &Id) -> Option<&media::CachedMedia> {
        self.entries
            .iter()
            .rev()
            .find(|(held, _)| held == id)
            .map(|(_, entry)| entry)
    }

    fn insert(&mut self, id: Id, entry: media::CachedMedia) {
        if self.get(&id).is_some() {
            return;
        }
        self.cost += entry.cost();
        self.entries.push_back((id, entry));
        while self.cost > MEDIA_CACHE_MAX_BYTES && self.entries.len() > 1 {
            if let Some((_, evicted)) = self.entries.pop_front() {
                self.cost = self.cost.saturating_sub(evicted.cost());
            }
        }
    }

    fn clear(&mut self) {
        self.entries.clear();
        self.cost = 0;
    }
}

/// The capture pump's shared half: what the worker reads while the pump writes.
///
/// The samples are *counted*, not kept — the bytes themselves are appended to the draft file
/// as the chunks arrive, so the count is the recording's own clock (elapsed is the count over
/// the note's rate) and the worker's memory holds one note's facts, not one note's audio —
/// section 179's incremental rule, the same one the Android recorder keeps by writing into
/// its file from the first second.
struct RecordShared {
    /// The samples taken so far. Frozen through a pause, because paused chunks are dropped
    /// rather than buffered — a pause is time the note does not contain.
    samples: AtomicU64,
    /// The live waveform's sampled bars, one per tenth of a second, pushed by the pump as
    /// each window fills.
    amplitudes: Mutex<Vec<u8>>,
    /// The pump's stop flag, so an end is an end even mid-chunk.
    stop: AtomicBool,
    /// The pause flag: chunks that arrive paused are dropped, and the timer and the cap both
    /// stand still because the count is what drives them.
    paused: AtomicBool,
}

/// One voice-note recording in progress: the open microphone, the pump appending its chunks
/// to the draft file, and the interruption state only the worker knows.
struct Recording {
    /// The conversation the note will land in.
    conversation_id: Id,
    /// The device handle, held so capture continues; dropped to stop. The chunk receiver is
    /// not here — the capture pump owns it now. Never read: the handle exists to be held.
    #[allow(dead_code)]
    microphone: call_audio::Microphone,
    /// The pump's shared half: the count, the bars, and the two flags.
    shared: Arc<RecordShared>,
    /// The pump thread's handle, joined when the recording ends so the draft file is fully
    /// written before anyone reads it back.
    pump: Option<std::thread::JoinHandle<()>>,
    /// The disappearing lifetime the composer was armed with when the recording began, so the
    /// finished note keeps the promise that was standing when its recording started — an arm
    /// switched off mid-note does not retroactively un-promise a send that had one.
    expires_in_ms: Option<u32>,
    /// Whether the pause standing now was set by an interruption — a call arriving, a call
    /// being placed — rather than the speaker's own Pause. An interruption's pause is lifted
    /// by the interruption passing; a pause the speaker chose is theirs to lift.
    paused_by_interruption: bool,
    /// The last elapsed the tick persisted to the draft descriptor, so the descriptor is
    /// rewritten on its own once-a-second cadence rather than on every tick.
    persisted_ms: u64,
}

/// A finished recording the composer is holding: the two-step mode's preview, the undo
/// window's cancelled note, or a draft recovered after an app death. The bytes stay in the
/// draft store — the send reads them back — so holding a note costs the worker its facts
/// and nothing else.
struct HeldNote {
    /// The conversation the note will land in.
    conversation_id: Id,
    /// The playing time, from the samples themselves: the count over the rate, the same
    /// clock the live bar ticked.
    duration_ms: u64,
    /// The sampled amplitude bars, unfolded — the fold happens when the message is built.
    amplitudes: Vec<u8>,
    /// The disappearing arm the recording began under, when one stood.
    expires_in_ms: Option<u32>,
}

/// One voice note playing: the open speaker and the pump that feeds it.
struct Playing {
    /// The note's media id, so a late "ended" report from a stopped pump can be told from
    /// this one's.
    media_id: Id,
    /// The device handle, held so playback continues; dropped to stop. Never read: the
    /// handle exists to be held.
    #[allow(dead_code)]
    speaker: call_audio::Speaker,
    /// The pump's stop flag.
    stop: Arc<AtomicBool>,
    /// The pump's speed flag, as a whole percent: the loop re-reads it every chunk and
    /// changes pace without losing its place, the same shared-flag shape `stop` takes.
    speed: Arc<AtomicU32>,
}

/// The reconnect schedule after a lost gateway connection.
///
/// Exponential backoff with jitter and a cap, driven from the worker's main select loop rather than
/// from inside the failure handler. That placement is the point. Sleeping inside the handler would
/// leave the command channel unserviced for the whole backoff, so a user who closes the window during
/// an outage would wait up to half a minute for the process to notice; here the sleep is one arm of a
/// select and `Shutdown` still wins. It also removes an async cycle — the handler no longer calls the
/// connect path that can call the handler.
#[derive(Debug, Clone, Copy)]
struct Retry {
    /// Attempts already made since the connection was lost.
    attempts: u32,
    /// The un-jittered delay, doubled after each failure and capped.
    backoff: Duration,
    /// What to actually wait: [`Self::backoff`] plus this round's jitter.
    wait: Duration,
}

/// The heartbeat interval this client pings at, from the `heartbeat_ms` a WELCOME advertised.
///
/// Half the advertised interval — the same derivation the Android client uses, for the same
/// reason: the server's deadline is two full intervals, so answering at half of one leaves a
/// full interval of slack for a timer that fires late. The floor keeps a pathological
/// advertisement (a heartbeat of, say, 2s) from turning the desktop into a constant pinger,
/// and a `0` advertisement from arming a spin.
const MIN_HEARTBEAT: Duration = Duration::from_secs(5);

/// The gateway session this worker would resume: the session's id, and the count of sequenced
/// frames this process has read off it.
///
/// The count is the client half of §150's sequencing contract, reconstructed exactly the way the
/// SDK's transport reconstructs it: the server numbers a server-to-client frame iff it is
/// Critical and left through the session mailbox, so the client counts every inbound frame the
/// same rule would number — Critical by class, or by the ERROR flag an error reply always rides
/// in on, minus the two Critical frames the server writes straight to the transport and never
/// numbers (WELCOME, consumed by the handshake before counting starts, and RECONNECT_HINT,
/// excluded by [`is_sequenced`] itself). An off-by-one here turns a clean resume into a refused
/// one, so the classification lives beside this struct and is pinned by its own test.
///
/// Kept across a disconnect — that is its whole point: the next `connect` attaches it to the
/// HELLO, and the server's retained ring answers with the frames the outage cost rather than a
/// full resync (§158). Dropped when a fresh WELCOME names a new session, when the server
/// refuses the resume, and at sign-out.
#[derive(Debug, Clone, Copy)]
struct GatewaySession {
    /// The session id the WELCOME minted — the id a resume asks for by name.
    id: Id,
    /// How many sequenced frames this process has read off the session so far.
    sequenced: u64,
}

impl GatewaySession {
    /// The resume request this session state turns into, for the HELLO of a reconnect.
    fn resume_request(&self) -> migo_protocol::ResumeRequest {
        migo_protocol::ResumeRequest {
            session_id: self.id,
            last_frame_seq: self.sequenced,
        }
    }
}

/// Whether an inbound frame consumes a `frame_seq` on the client's counter.
///
/// Mirrors the server's mailbox sequencing and the SDK's `isSequenced`: Critical frames are
/// sequenced, except RECONNECT_HINT, which the server writes directly to the transport at
/// graceful shutdown and never numbers. WELCOME is the other unsequenced Critical frame, but it
/// is consumed by the handshake before the receive loop starts counting, so it never reaches
/// here. A frame this build has no opcode for is not counted: the server cannot have numbered
/// an opcode it never sent, and a newer server's unknown frames follow their own class rules,
/// not this build's guesses.
fn is_sequenced(frame: &Frame) -> bool {
    if frame.header.is_error() {
        return true;
    }
    match Opcode::from_wire(frame.header.opcode) {
        Some(Opcode::ReconnectHint) => false,
        Some(opcode) => matches!(opcode.class(), DeliveryClass::Critical),
        None => false,
    }
}

/// One conversation's sequence account: the contiguous watermark, the highest seq ever seen,
/// and the stall the last unproductive fill stopped at.
///
/// §152 makes a conversation's sequence numbers gapless by construction, so "what this client
/// holds" is a *prefix*, and the watermark names its top: it advances only when the next brick
/// lands on it (seq = watermark + 1), stands still through redeliveries, and holds when an
/// arrival leaps past it — a hole, which is exactly what a catch-up walk exists to fill. The
/// first event a conversation routes becomes the floor: history below it may be gone (the
/// server's `Truncated`) or simply unfetched, which is the caller's floor to choose. Tombstones
/// and key exchanges count like any message — the prefix is of *events*, not of content — which
/// is why this account lives in the net layer, where every event passes, rather than in the UI's
/// message store, which never sees the ones the crypto layer consumes.
#[derive(Debug, Default, Clone, Copy)]
struct SeqAccount {
    /// The highest seq held contiguously; zero until the first event sets the floor.
    watermark: u64,
    /// The highest seq ever routed, gap or no gap — the top of the hole a fill must target.
    highest_seen: u64,
    /// The watermark an unproductive fill stopped at, if one did.
    ///
    /// While the watermark stands here the server has already been asked and has already
    /// answered without moving it, so a later above-gap event re-asks nothing — the hot loop a
    /// persistent (purged) hole must never become. The stall lifts the moment the watermark
    /// moves.
    stalled_at: Option<u64>,
}

impl SeqAccount {
    /// Routes one seq through the account, returning the gap it opened, if any.
    ///
    /// The gap is named as its top — the account's `highest_seen` after this event — because a
    /// fill asked for less would leave the hole's tail unfetched, and one asked for more would
    /// tail live traffic instead of filling the hole.
    fn track(&mut self, seq: u64) -> Option<u64> {
        if seq > self.highest_seen {
            self.highest_seen = seq;
        }
        let gap = if seq == self.watermark + 1 {
            self.watermark = seq;
            None
        } else if seq <= self.watermark {
            // A redelivery the UI's own id-keyed dedup handles, or history below the floor —
            // the caller's floor to choose, not this account's to fill.
            None
        } else if self.watermark == 0 {
            // The floor: the first event a conversation ever routes, whatever its seq.
            self.watermark = seq;
            None
        } else {
            Some(self.highest_seen)
        };
        if self
            .stalled_at
            .is_some_and(|stalled| stalled != self.watermark)
        {
            // The watermark moved since the stall was recorded, so the stall is over.
            self.stalled_at = None;
        }
        gap
    }
}

/// One catch-up walk in flight, keyed by the conversation it walks.
///
/// A walk is a fire-and-forget page loop: the worker sends a SYNC and the answer's arrival in
/// [`Worker::on_history`] decides whether another page goes out. `to_seq` bounds a gap walk to
/// the hole it exists to fill (`None` walks to the server's live edge), and the page budget
/// keeps a very long conversation bounded the way the web client's `MAX_CATCHUP_PAGES` does.
struct CatchUp {
    /// The seq the walk is walking toward, or `None` for the live edge.
    to_seq: Option<u64>,
    /// Pages the walk may still ask for after this one.
    pages_left: u32,
}

impl CatchUp {
    /// Whether the walk continues after a page, given the watermark's move across it.
    ///
    /// `moved` is false when the page could not advance the watermark — every row a
    /// redelivery, or a hole the server says is purged — and a walk that cannot move it must
    /// stop rather than re-ask on a loop; the caller records the stall so a later above-gap
    /// event does not restart it. `more` is the server's own word for whether history remains
    /// above the page, which only an unbounded walk listens to: a gap walk compares the
    /// watermark against its target instead, because `more` is true whenever the *conversation*
    /// has history above the page, hole filled or not.
    fn continues(&self, moved: bool, more: bool, watermark: u64) -> bool {
        if self.pages_left == 0 || !moved {
            return false;
        }
        match self.to_seq {
            Some(to) => watermark < to,
            None => more,
        }
    }
}

fn heartbeat_interval(advertised_ms: u32) -> Duration {
    let half = Duration::from_millis(u64::from(advertised_ms) / 2);
    half.max(MIN_HEARTBEAT)
}

impl Retry {
    /// Attempts before giving up and telling the user to check the address.
    const LIMIT: u32 = 8;
    /// The first pause: long enough not to hammer a server that is restarting, short enough that a
    /// handful of dropped packets never reaches the user as a visible outage.
    const BASE: Duration = Duration::from_millis(500);
    /// A long outage must not turn into half an hour of silence after it ends.
    const CAP: Duration = Duration::from_secs(30);

    /// The schedule for the first attempt after a connection is lost.
    fn first(random: &mut dyn Random) -> Self {
        Self {
            attempts: 0,
            backoff: Self::BASE,
            wait: Self::BASE + Self::jitter(random),
        }
    }

    /// The schedule after an attempt failed. Returns `None` once the limit is reached.
    fn after_failure(self, random: &mut dyn Random) -> Option<Self> {
        let attempts = self.attempts + 1;
        if attempts >= Self::LIMIT {
            return None;
        }
        let backoff = (self.backoff * 2).min(Self::CAP);
        Some(Self {
            attempts,
            backoff,
            wait: backoff + Self::jitter(random),
        })
    }

    /// Up to half a second of spread, so ten thousand clients do not return in lockstep and knock the
    /// node over again the moment it comes back.
    fn jitter(random: &mut dyn Random) -> Duration {
        let mut bytes = [0u8; 2];
        random.fill_bytes(&mut bytes);
        Duration::from_millis(u64::from(u16::from_le_bytes(bytes) % 500))
    }
}

/// The live realtime connection, over whichever transport the session is riding.
///
/// All three bindings speak MWP frames with the same send/receive shape, so the worker treats
/// them identically: an enum rather than a shared trait object, because the set is closed by the
/// brief (TCP the native default, WebSocket the web transport, QUIC the second option —
/// section 138) and a dyn would be vocabulary with no fourth implementation behind it.
enum Realtime {
    Tcp(TcpGateway),
    WebSocket(Box<Gateway>),
    Quic(QuicGateway),
}

impl Realtime {
    /// A fresh correlation id, for a request whose reply must be matched to it.
    fn correlate(&mut self) -> u32 {
        match self {
            Self::Tcp(gateway) => gateway.correlate(),
            Self::WebSocket(gateway) => gateway.correlate(),
            Self::Quic(gateway) => gateway.correlate(),
        }
    }

    /// Encodes and sends one frame on the live transport.
    async fn send<T: migo_protocol::Encode>(
        &mut self,
        opcode: Opcode,
        correlation: u32,
        value: &T,
    ) -> Result<(), GatewayError> {
        match self {
            Self::Tcp(gateway) => gateway
                .send(opcode, correlation, value)
                .await
                .map_err(|_| GatewayError::Transport),
            Self::WebSocket(gateway) => gateway.send(opcode, correlation, value).await,
            Self::Quic(gateway) => gateway
                .send(opcode, correlation, value)
                .await
                .map_err(|_| GatewayError::Transport),
        }
    }

    /// Reads the next protocol frame off the live transport.
    async fn next_frame(&mut self) -> Result<Frame, GatewayError> {
        match self {
            Self::Tcp(gateway) => gateway
                .next_frame()
                .await
                .map_err(|_| GatewayError::Transport),
            Self::WebSocket(gateway) => gateway.next_frame().await,
            Self::Quic(gateway) => gateway
                .next_frame()
                .await
                .map_err(|_| GatewayError::Transport),
        }
    }

    /// Closes politely, so the server retires the session rather than timing it out.
    ///
    /// Consuming `self` rather than taking `&mut self` mirrors the underlying gateways' own
    /// close-by-ownership shape: a close is always the last thing that happens to a connection.
    async fn close(self) {
        match self {
            // The WebSocket gateway is a large struct (the TLS stream state machine); boxing
            // keeps the enum one pointer wide so the QUIC variant does not pay for its size.
            Self::Tcp(mut gateway) => gateway.close().await,
            Self::WebSocket(gateway) => gateway.close().await,
            Self::Quic(mut gateway) => gateway.close().await,
        }
    }
}

/// The worker itself.
struct Worker {
    sink: Sink,
    /// The command channel's sending end, so a spawned tracker task can report its ending into
    /// the loop that owns the Activity list — the same loop that will seal it into the vault.
    commands: mpsc::UnboundedSender<Command>,
    vault_path: PathBuf,
    signed: Option<Signed>,
    gateway: Option<Realtime>,
    /// Armed while the gateway is down and a reconnect is still worth trying.
    retry: Option<Retry>,
    /// The room a leave is in flight for. The wire's acknowledgement names no room, so the
    /// request's own id is the only thing that can say which room the ack answers.
    pending_leave: Option<Id>,
    /// The room a join-bell membership probe is in flight for. A `Joined` naming this account
    /// for a room this session has never heard of arrived on the account's own user topic —
    /// the join happened on another device — and the reaction is an idempotent re-join. But a
    /// bell can be stale (the account may have left since the other device joined), and a
    /// reaction that re-joined them would be this device deciding membership, so the roster is
    /// asked for first: it refuses anyone who is not in the room, and only a room that still
    /// answers is re-joined.
    pending_room_probe: Option<Id>,
    /// The conversation a roster read is in flight for. The wire's roster answer names no
    /// conversation, so the request's own subject is the only thing that can say which
    /// membership the reply promotes. One at a time by design: the send path asks only when
    /// its cached membership is incomplete, and the answer that stores completes that cache,
    /// so a second ask for the same conversation cannot arise before the first is answered.
    pending_roster: Option<Id>,
    /// The member card a "View profile" ask is waiting on: the conversation whose window drew
    /// the menu, and the account the card must name. The profile reply is a batch that names
    /// its subjects but not its asker, so this is the only thing that can say which window a
    /// card answers — the same one-at-a-time patience the roster ask keeps, for the same
    /// reason: the reply that clears the ask is the reply that fills the view.
    pending_member_profile: Option<(Id, Id)>,
    /// The member view's standing ask, split per reply because the wire answers progression
    /// and badges as two frames that name no asker: each half is remembered until its own
    /// reply lands, and each reply routes by its half. The progression reply does name its
    /// account, so that half is checked against the account as well as the ask; the badge
    /// reply names nobody, so its half is spent on the next badge frame — the one race this
    /// pattern allows, the same trade a wallet refresh firing mid-ask already makes everywhere
    /// a reply cannot name its request.
    pending_member_progression: Option<(Id, Id)>,
    /// The badge half of the member view's standing ask. See
    /// [`Self::pending_member_progression`] for why the halves are separate.
    pending_member_badges: Option<(Id, Id)>,
    /// The XP-board page a member rank ask is waiting on. The board reply names the accounts
    /// it ranks but not the asker, so the ask is remembered to find the row — and a row the
    /// page does not hold is the answer "no rank", not a lost ask.
    pending_member_rank: Option<(Id, Id)>,
    /// The graph walk a member view's social-line ask is waiting on. The relationships reply
    /// is the whole graph, so the ask is only the memory of which edge to pull out of it.
    pending_member_edge: Option<(Id, Id)>,
    /// The founding keys a registration attempt minted but has not yet made stick (§12). A
    /// registration that fails after the server heard it must be retried with the *same* keys:
    /// a fresh root would be a different identity key, which the server can only answer with
    /// USERNAME_TAKEN. Cleared the moment the vault is written — from then on the vault is the
    /// keys' home.
    pending_registration: Option<DeviceKeys>,
    /// This account's tracked AVAX transactions (§184's Activity list), in memory between
    /// passphrase moments — this worker deliberately does not hold the passphrase after unlock,
    /// so the list is re-sealed into the vault only when a sign-in next opens it.
    ///
    /// The account id rides along so a different account signing in over the same window never
    /// inherits another account's history.
    txs: Option<(Id, Vec<TxRecord>)>,
    /// The last-seen E2EE identity fingerprint of every peer device this session has observed,
    /// in memory between passphrase moments — the same trade the Activity list makes, for the
    /// same reason. This is the map a key-change warning is decided against, so it has to
    /// outlive the conversation window that observed the key: a fingerprint memory that died at
    /// sign-out would warn about every peer, every session, and mean none of them.
    ///
    /// The account id rides along for the same reason the Activity list's does.
    peers: Option<(Id, HashMap<Id, [u8; 32]>)>,
    /// When this device last sealed a `.migo` container, in memory between passphrase moments —
    /// the same trade again, and for the same reason: the export path holds the container's own
    /// credential, never the vault passphrase, so the stamp reaches the vault only when a
    /// sign-in, a restore or a rotation next opens it. A crash before that door loses the date
    /// and the checkup says "Never backed up", which understates what exists and never
    /// overstates it — the direction a security surface is allowed to be wrong in.
    ///
    /// The inner `Option` is the fact itself: `None` is "never, or invalidated by a rotation",
    /// not "unknown". The account id rides along for the same reason the Activity list's does.
    last_backup_at: Option<(Id, Option<u64>)>,
    /// One avatar upload between its BEGIN and the ticket's arrival. The bytes wait here
    /// because the worker never blocks on a reply; the frame arm completes the flow when the
    /// ticket lands. Replaced, never queued — a second pick while one is in flight refuses at
    /// the UI before a command is ever issued.
    avatar_pending: Option<AvatarPending>,
    /// An avatar upload between its COMMIT and the acknowledgement that closes it: the media
    /// id the profile patch will name once the server says the object exists.
    avatar_commit_pending: Option<(Id, PathBuf)>,
    /// The MWP heartbeat interval, taken from the WELCOME's advertised `heartbeat_ms` and
    /// halved — the same derivation the Android client uses — with the same floor, so a
    /// server that advertises an aggressively short interval cannot turn the desktop into a
    /// ping flood. `None` while disconnected; set on every connect, which is also what
    /// re-arms it after a reconnect.
    ///
    /// This is the client half of the keep-alive contract (section 139 / §151): the server
    /// closes a session whose socket stays silent for two advertised intervals, and until
    /// now the desktop never sent anything on an idle connection — an idle desktop session
    /// died at the deadline and churned a full reconnect (keys, subscriptions, list
    /// re-reads) every time the user stepped away. One PING at half the interval keeps the
    /// session alive at a cost of a few bytes; the server's own half — a probe at half the
    /// deadline before closing — covers the case where this timer is late.
    heartbeat: Option<Duration>,
    /// One HTTP client for every chain call. A `reqwest::Client` shares its connection pool, so
    /// cloning it per operation is free, and the chain conversation stays off the Migo session's
    /// client entirely.
    chain_http: reqwest::Client,
    /// The call engine: the one call this device can be in, the rings it hears, and the call
    /// keys that unseal them. Lives on the worker because a call is a network session with its
    /// own timers, and the loop below owns every other timed thing the same way.
    calls: call::Calls,
    /// The group-call seat beside the 1:1 engine: the SFU roster this device holds, the frame
    /// key it shares with the roster, and a join still waiting on its roster snapshot. One at
    /// a time, like the engine's own call.
    group_calls: group_call::GroupCalls,
    /// Attachment uploads between their BEGIN and the ticket's arrival, keyed by correlation.
    /// More than one may be in flight: an attach and a voice note can overlap, and the wire's
    /// reply names nothing but the correlation it answers.
    attachment_begins: HashMap<u32, AttachmentBegin>,
    /// Attachment uploads between their COMMIT and the acknowledgement that closes them.
    attachment_commits: HashMap<u32, AttachmentCommit>,
    /// Fetches waiting on their signed URL, keyed by correlation.
    media_wants: HashMap<u32, MediaWant>,
    /// Media ids a fetch is already in flight for, so a bubble that asks twice (or a
    /// scrolling thread that asks many times) costs one fetch.
    media_fetching: HashSet<Id>,
    /// What has been fetched and opened, bounded, so a scroll back through a thread of
    /// images does not re-decrypt or re-download what this session already had.
    media_cache: MediaCache,
    /// The voice-note recording in progress, if one runs.
    recording: Option<Recording>,
    /// A finished note the composer is holding — the preview, the undo window's cancelled
    /// note, or a recovered draft. One at a time, like the recording: the composer is one
    /// surface, and two held notes would be a question asked twice.
    held_note: Option<HeldNote>,
    /// When the undo window on a discarded note closes, if one stands. The deadline belongs
    /// to the loop because the bytes are deleted by it — the window's expiry is the one
    /// moment the draft store's door swings shut on its own.
    note_undo_until: Option<std::time::Instant>,
    /// The voice-note draft store, beside the vault. The recording writes into it
    /// incrementally, the preview holds from it, and the send reads it back.
    drafts: voice_draft::VoiceDraftStore,
    /// The voice note playing, if one plays. One at a time, like the recording: a speaker is
    /// a device, and the second note would mix with the first.
    playing: Option<Playing>,
    /// The playback speed the next note starts at (§179): 1x, 1.5x, or 2x, changed by the
    /// player's own control and seeded from the saved setting at startup. The pump playing
    /// now is told through its shared flag, so a change never restarts the note.
    voice_speed: VoiceSpeed,
    /// The voice notes this account has listened to, by media id — heard to (near) the end
    /// or marked by hand. §179's receiver-local state: nothing here is ever sent, and it is
    /// written beside the vault so it survives a restart.
    listened: HashSet<Id>,
    /// The listened marks' store, beside the vault: what one session heard, the next
    /// session reads back at sign-in.
    voice_listened: voice_listened::VoiceListenedStore,
    /// Group rosters asked for by the panel and not yet answered, keyed by the conversation
    /// the ask named. More than one may be in flight — the roster panel asks for whichever
    /// group window is open, and the reply names nothing but the correlation it answers, so
    /// the conversation the request carried is the only key the answer can be filed under.
    /// (The send path's own roster ask is the single-slot `pending_roster` above, kept apart
    /// because it is a different ask with a different consequence: one promotes an audience,
    /// this one fills a panel.)
    group_rosters: HashMap<Id, ()>,
    /// The group leave awaiting its acknowledgement. One at a time: the leave closes the
    /// window that asked for it, so a second ask cannot be made before the first is answered.
    /// The ack names nothing, so the ask is remembered the same way a room leave's is.
    group_leave: Option<Id>,
    /// The kick vote awaiting its reply, as the (conversation, target) the request named. The
    /// reply carries only the tally — neither the conversation nor the target — so the ask is
    /// the only place the answer's subject lives.
    pending_vote: Option<(Id, Id)>,
    /// The gateway session this worker would resume on its next connect, if the socket drops.
    /// `None` until a WELCOME mints one, and again after a fresh session replaces it or the
    /// server refuses the resume.
    session: Option<GatewaySession>,
    /// Catch-up walks in flight, keyed by the conversation each is walking. One per
    /// conversation by design: a second trigger while a walk runs is the running walk's
    /// business, and a duplicate would race its pages.
    catchups: HashMap<Id, CatchUp>,
    /// Backwards history asks in flight, keyed by the correlation the reply will carry: the
    /// SYNC answer names its conversation but not its direction, so the ask's own correlation
    /// is the only thing that can say the page that arrived is a load-earlier page.
    earlier_asks: HashMap<u32, Id>,
    /// Forward SYNC asks in flight, keyed the same way: a refusal that ends a walk carries no
    /// conversation of its own, so the correlation is the only thing that can say which walk
    /// an error reply retires.
    sync_asks: HashMap<u32, Id>,
    /// The persisted room bridge, beside the vault: what a join wrote, the next process reads
    /// back, so the room topics a restart lost are re-subscribed without a re-join.
    room_bridges: room_bridge::RoomBridgeStore,
}

impl Worker {
    fn new(sink: Sink, commands: mpsc::UnboundedSender<Command>, vault_path: PathBuf) -> Self {
        // The draft store stands beside the vault and borrows its path only here, before the
        // struct literal moves it.
        let drafts = voice_draft::VoiceDraftStore::beside(&vault_path);
        let room_bridges = room_bridge::RoomBridgeStore::beside(&vault_path);
        let voice_listened = voice_listened::VoiceListenedStore::beside(&vault_path);
        Self {
            sink,
            commands,
            vault_path,
            signed: None,
            gateway: None,
            retry: None,
            pending_leave: None,
            pending_room_probe: None,
            pending_roster: None,
            pending_member_profile: None,
            pending_member_progression: None,
            pending_member_badges: None,
            pending_member_rank: None,
            pending_member_edge: None,
            pending_registration: None,
            txs: None,
            peers: None,
            last_backup_at: None,
            avatar_pending: None,
            avatar_commit_pending: None,
            heartbeat: None,
            chain_http: reqwest::Client::new(),
            calls: call::Calls::new(),
            group_calls: group_call::GroupCalls::new(),
            attachment_begins: HashMap::new(),
            attachment_commits: HashMap::new(),
            media_wants: HashMap::new(),
            media_fetching: HashSet::new(),
            media_cache: MediaCache::new(),
            recording: None,
            held_note: None,
            note_undo_until: None,
            drafts,
            playing: None,
            voice_speed: VoiceSpeed::default(),
            listened: HashSet::new(),
            voice_listened,
            group_rosters: HashMap::new(),
            group_leave: None,
            pending_vote: None,
            session: None,
            catchups: HashMap::new(),
            earlier_asks: HashMap::new(),
            sync_asks: HashMap::new(),
            room_bridges,
        }
    }

    /// The worker's whole life: report what the vault looks like, then serve commands and frames.
    async fn run(mut self, mut commands: mpsc::UnboundedReceiver<Command>) {
        if vault::exists(&self.vault_path) {
            // The username and server are inside the encrypted body, so they are not knowable until
            // the passphrase arrives. The unlock screen shows the file, not the account.
            self.sink.send(Event::VaultFound);
        } else {
            self.sink.send(Event::VaultMissing);
        }

        loop {
            // Read out of `self` before either future is built: the frame future borrows the gateway
            // mutably, so the reconnect timer must not also capture `self`.
            let wait = self.retry.map(|plan| plan.wait);

            // All three arms in one select so a command is served promptly even while a frame is
            // pending, an inbound frame is not delayed behind an idle command channel, and a reconnect
            // backoff never blocks either of the other two.
            let frame = async {
                match self.gateway.as_mut() {
                    Some(gateway) => Some(gateway.next_frame().await),
                    // No connection: park forever rather than spin. The command arm still wakes us.
                    None => {
                        std::future::pending::<()>().await;
                        None
                    }
                }
            };
            let due = async move {
                match wait {
                    Some(wait) => tokio::time::sleep(wait).await,
                    // Nothing scheduled: park, so this arm never completes and never spins.
                    None => std::future::pending::<()>().await,
                }
            };
            // The heartbeat arm, alongside the reconnect timer: same pattern, different
            // question. It must live in this select — not in a spawned task — because it
            // needs the same exclusive `&mut` gateway the frame arm borrows, and a ping sent
            // from a sibling task would race the loop's own writes.
            let beat = self.heartbeat;
            let beat = async move {
                match beat {
                    Some(interval) => tokio::time::sleep(interval).await,
                    None => std::future::pending::<()>().await,
                }
            };
            // The call engine's own timers, the same shape again: candidate batches lingering
            // for the linger window, ring expiry, the reconnect window, the call-key wait, the
            // TURN fetch's lenient timeout. Built from `next_tick` rather than a spawned task
            // for the same reason the heartbeat is: the engine's state belongs to this loop,
            // and a tick handled by a sibling task would race the loop's own transitions.
            // `select!` drops the losing futures before running the chosen arm's handler, so
            // the borrow this future holds on the engine ends before `on_call_tick` takes its
            // own — the same discipline the frame arm follows with the gateway.
            let call_tick = self.calls.next_tick();
            // The recording's own tick, the same shape one more time: a quarter-second
            // cadence while a note is live, driving the bar's clock and waveform, persisting
            // the draft descriptor once a second, and ending the note at the cap. It belongs
            // to the loop because all of that touches the worker's own state, and a timer
            // that fired from a sibling task would race every hand that touches them. The
            // cap itself is read from the sample count rather than the wall clock, so a
            // pause stands the cap still the same way it stands the timer still.
            let recording_live = self.recording.is_some();
            let record = async move {
                if recording_live {
                    tokio::time::sleep(Duration::from_millis(RECORDING_TICK_MS)).await
                } else {
                    // Nothing recording: park, so this arm never completes and never spins.
                    std::future::pending::<()>().await
                }
            };
            // The undo window on a discarded note, the same shape again: the bytes are
            // deleted by its expiry, and the deletion is the loop's to make.
            let note_until = self.note_undo_until;
            let note_window = async move {
                match note_until {
                    Some(deadline) => {
                        tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)).await
                    }
                    None => std::future::pending::<()>().await,
                }
            };

            tokio::select! {
                command = commands.recv() => {
                    match command {
                        Some(Command::Shutdown) | None => break,
                        Some(command) => self.handle(command).await,
                    }
                }
                Some(result) = frame => {
                    match result {
                        Ok(frame) => self.on_record(frame).await,
                        Err(error) => self.on_disconnect(error),
                    }
                }
                () = due => self.reconnect().await,
                () = beat => self.send_heartbeat().await,
                Some(tick) = call_tick => self.on_call_tick(tick).await,
                () = record => self.recording_ticked().await,
                () = note_window => self.note_undo_expired(),
            }
        }

        if let Some(gateway) = self.gateway.take() {
            gateway.close().await;
        }
    }

    async fn handle(&mut self, command: Command) {
        match command {
            Command::FetchCaptcha { server, mode } => {
                self.fetch_captcha(server, mode).await;
            }
            Command::Register {
                server,
                username,
                account_passphrase,
                passphrase,
                captcha,
            } => {
                self.bootstrap(
                    server,
                    username,
                    account_passphrase,
                    passphrase,
                    captcha,
                    true,
                )
                .await;
            }
            Command::SignIn {
                server,
                identifier,
                account_passphrase,
                passphrase,
                captcha,
            } => {
                self.bootstrap(
                    server,
                    identifier,
                    account_passphrase,
                    passphrase,
                    captcha,
                    false,
                )
                .await;
            }
            Command::Unlock { passphrase } => self.unlock(passphrase).await,
            Command::SignOut => self.sign_out().await,
            Command::Conversations => self.request_conversations().await,
            Command::History {
                conversation_id,
                held,
            } => {
                // The window opening says which floor the walk climbs from: a thread that
                // already holds rows continues from the watermark, and an empty one replays
                // from zero — the same `messagesRef` test the web client's open makes. The
                // watermark decides the continuation pages on its own.
                let from = if held {
                    self.watermark(conversation_id)
                } else {
                    0
                };
                self.catch_up(conversation_id, from, None).await;
            }
            Command::HistoryEarlier {
                conversation_id,
                before_seq,
            } => {
                self.request_earlier(conversation_id, before_seq).await;
            }
            Command::PeerKeys { user_id } => self.fetch_peer_keys(user_id).await,
            Command::SendText {
                conversation_id,
                text,
                expires_in_ms,
            } => {
                self.send_text(conversation_id, text, expires_in_ms).await;
            }
            Command::StartDirect { username } => self.start_direct(username).await,
            Command::StartDirectById { peer } => self.start_direct_by_id(peer).await,
            Command::CreateGroup { members, title } => {
                self.create_group(members, title).await;
            }
            Command::InviteToGroup {
                conversation_id,
                members,
            } => {
                self.invite_to_group(conversation_id, members).await;
            }
            Command::LeaveGroup { conversation_id } => {
                self.leave_group(conversation_id).await;
            }
            Command::GroupRoster { conversation_id } => {
                self.request_group_roster(conversation_id).await;
            }
            Command::MuteGroupMember {
                conversation_id,
                target_id,
                until,
            } => {
                self.mute_group_member(conversation_id, target_id, until)
                    .await;
            }
            Command::KickGroupMember {
                conversation_id,
                target_id,
            } => {
                self.kick_group_member(conversation_id, target_id).await;
            }
            Command::VoteKickMember {
                conversation_id,
                target_id,
            } => {
                self.vote_kick_member(conversation_id, target_id).await;
            }
            Command::RenameGroup {
                conversation_id,
                title,
            } => {
                self.rename_group(conversation_id, title).await;
            }
            Command::Typing {
                conversation_id,
                typing,
            } => {
                self.send_typing(conversation_id, typing).await;
            }
            Command::MarkRead {
                conversation_id,
                seq,
            } => {
                self.mark_read(conversation_id, seq).await;
            }
            Command::Friends => self.request_relationships().await,
            Command::AddFriend { user_id } => self.add_friend(user_id).await,
            Command::RespondFriend { user_id, accept } => {
                self.respond_friend(user_id, accept).await;
            }
            Command::Sessions => self.fetch_sessions().await,
            Command::RevokeSession { session_id } => {
                self.revoke_session(session_id).await;
            }
            Command::Rooms { query } => self.request_rooms(query).await,
            Command::JoinRoom { room_id } => self.join_room(room_id).await,
            Command::CreateRoom {
                slug,
                name,
                managed,
                topic,
            } => {
                self.create_room(slug, name, managed, topic).await;
            }
            Command::LeaveRoom { room_id } => self.leave_room(room_id).await,
            Command::Notifications => self.request_notifications().await,
            Command::AcknowledgeAlerts { through_unix_ms } => {
                self.acknowledge_alerts(through_unix_ms).await;
            }
            Command::Wallet => self.request_wallet().await,
            Command::SendGift {
                sku,
                recipient,
                client_key,
            } => {
                self.send_gift(sku, recipient, client_key).await;
            }
            Command::BuyKickPoints {
                pack_kp,
                client_key,
            } => {
                self.buy_kick_points(pack_kp, client_key).await;
            }
            Command::SearchPeople { query } => self.search_people(query).await,
            Command::Suggestions => self.request_suggestions().await,
            Command::Devices => self.fetch_devices().await,
            Command::RevokeDevice { device_id } => {
                self.revoke_device(device_id).await;
            }
            Command::OwnProfile => self.fetch_own_profile().await,
            Command::MemberProfile {
                conversation_id,
                user_id,
            } => {
                self.fetch_member_profile(conversation_id, user_id).await;
            }
            Command::MemberStanding {
                conversation_id,
                user_id,
            } => {
                self.fetch_member_standing(conversation_id, user_id).await;
            }
            Command::MemberRank {
                conversation_id,
                user_id,
            } => {
                self.fetch_member_rank(conversation_id, user_id).await;
            }
            Command::MemberEdge {
                conversation_id,
                user_id,
            } => {
                self.fetch_member_edge(conversation_id, user_id).await;
            }
            Command::Entitlements => self.request_entitlements().await,
            Command::GiftCatalogue => self.request_gift_catalogue().await,
            Command::SaveProfile(patch) => self.save_profile(patch).await,
            Command::ChangeAvatar { path } => self.change_avatar(path).await,
            Command::Admins => self.fetch_admins().await,
            Command::GrantAdmin { username } => self.grant_admin(username).await,
            Command::RevokeAdmin { account_id } => {
                self.revoke_admin(account_id).await;
            }
            Command::Wallets => self.fetch_wallets().await,
            Command::ExportContainer { path, credential } => {
                self.export_container(path, credential).await;
            }
            Command::ImportContainer {
                path,
                credential,
                passphrase,
                username,
                server,
            } => {
                self.import_container(path, credential, passphrase, username, server)
                    .await;
            }
            Command::ArchiveWallet { wallet_id } => {
                self.archive_wallet(wallet_id).await;
            }
            Command::SetContact { email_or_phone } => {
                self.set_contact(email_or_phone).await;
            }
            Command::ContactStanding => {
                self.fetch_contact_standing().await;
            }
            Command::ChangePassphrase { current, next } => {
                self.change_passphrase(current, next).await;
            }
            Command::RotateIdentity { passphrase } => {
                self.rotate_identity(passphrase).await;
            }
            Command::ChainBalance { network } => self.chain_balance(network).await,
            Command::ChainPrepare {
                network,
                recipient,
                amount_avax,
            } => {
                self.chain_prepare(network, recipient, amount_avax).await;
            }
            Command::ChainSend { tx } => self.chain_send(tx).await,
            Command::ChainSettled {
                network,
                tx_hash,
                outcome,
                block,
                gas_used,
            } => {
                self.chain_settled(network, tx_hash, outcome, block, gas_used)
                    .await;
            }
            Command::StartCall {
                conversation_id,
                callee_id,
            } => {
                self.start_call(conversation_id, callee_id).await;
            }
            Command::AcceptCall => self.accept_call().await,
            Command::DeclineCall => self.decline_call().await,
            Command::EndCall => self.end_call().await,
            Command::ToggleCallMute => self.toggle_call_mute(),
            Command::DismissCall => self.dismiss_call(),
            Command::JoinGroupCall {
                conversation_id,
                call_id,
            } => {
                self.join_group_call(conversation_id, call_id).await;
            }
            Command::LeaveGroupCall { conversation_id } => {
                self.leave_group_call(conversation_id).await;
            }
            Command::SendAttachment {
                conversation_id,
                path,
                expires_in_ms,
            } => {
                self.send_attachment(conversation_id, path, expires_in_ms)
                    .await;
            }
            Command::StartRecording {
                conversation_id,
                expires_in_ms,
            } => {
                self.start_recording(conversation_id, expires_in_ms);
            }
            Command::PauseRecording => self.pause_recording(false),
            Command::ResumeRecording => self.resume_recording(false),
            Command::StopRecording => self.stop_recording(),
            Command::SendVoiceNote => self.send_voice_note().await,
            Command::CancelVoiceNote => self.cancel_voice_note(),
            Command::UndoVoiceNoteDiscard => self.undo_note_discard(),
            Command::RecoverVoiceDraft { conversation_id } => {
                self.recover_voice_draft(conversation_id);
            }
            Command::SendReaction {
                conversation_id,
                target_message_id,
                emoji,
            } => {
                self.send_reaction(conversation_id, target_message_id, emoji)
                    .await;
            }
            Command::DeleteMessage {
                conversation_id,
                message_id,
            } => {
                self.delete_message(conversation_id, message_id).await;
            }
            Command::EditMessage {
                conversation_id,
                message_id,
                text,
            } => {
                self.edit_message(conversation_id, message_id, text).await;
            }
            Command::BlockUser { user_id } => self.block_user(user_id).await,
            Command::MuteUser { user_id, on } => self.mute_user(user_id, on).await,
            Command::FetchMedia { media_id } => {
                self.want_media(media_id, MediaIntent::Show).await;
            }
            Command::SaveMedia { media_id, path } => {
                self.want_media(media_id, MediaIntent::SaveTo(path)).await;
            }
            Command::PlayVoiceNote { media_id } => self.play_voice_note(media_id).await,
            Command::StopVoiceNote => self.stop_voice_note(),
            Command::SetVoiceSpeed { speed } => self.set_voice_speed(speed),
            Command::SetVoiceNoteListened { media_id, listened } => {
                self.set_voice_listened(media_id, listened);
            }
            Command::VoiceNoteHeard { media_id } => self.voice_note_heard(media_id),
            Command::VoiceNoteEnded { media_id } => self.voice_note_ended(media_id),
            Command::Shutdown => {}
        }
    }

    /// Registers or signs in, generating keys and writing the vault.
    async fn bootstrap(
        &mut self,
        server: ServerEndpoint,
        identifier: String,
        account_passphrase: String,
        passphrase: String,
        captcha: Option<CaptchaAnswer>,
        register: bool,
    ) {
        self.sink.send(Event::Connection(Connection::Connecting));

        let rest = match Rest::new(&crate::config::rest_base_url(&server)) {
            Ok(rest) => rest,
            Err(error) => return self.fail(error.to_string()),
        };

        // Reuse the existing device id when a vault is already present, so signing in again does not
        // orphan the identity key every peer has already verified.
        let existing = vault::load(&self.vault_path, &passphrase).ok();
        let device_id = existing
            .as_ref()
            .and_then(|keys| keys.session.as_ref())
            .map(|s| s.device_id);
        let device = DeviceRequest::describe(device_id);

        // Lent to the wire body rather than moved into it, so the answer's bytes stay in the
        // command's own allocation until the request is done.
        let proof = captcha.as_ref().map(|answer| CaptchaProof {
            challenge_id: &answer.challenge_id,
            answer: &answer.answer,
        });

        // A registration's keys are resolved *before* the request (§12): the vault's when a
        // passphrase just opened one, else a founding set minted once and reused across retries.
        // The identity key travels with the request, so a retry whose first attempt already
        // landed reconciles into the account that attempt made instead of being refused as a
        // taken name. A sign-in mints nothing here — an additional device's keys never touch the
        // account root.
        let registration_keys = if register {
            Some(match existing {
                Some(keys) => keys,
                None => self.pending_registration.take().unwrap_or_else(|| {
                    DeviceKeys::founding(&migo_account::MigoRoot::generate(&mut OsRandom))
                }),
            })
        } else {
            None
        };
        let identity_public_key = registration_keys
            .as_ref()
            .and_then(|keys| keys.identity_key())
            .map(|identity| identity.public_key().to_vec());
        let grant = if register {
            rest.register(
                &identifier,
                &account_passphrase,
                device,
                proof,
                identity_public_key.as_deref(),
            )
            .await
        } else {
            rest.login(&identifier, &account_passphrase, device, proof)
                .await
        };
        let grant = match grant {
            Ok(grant) => grant,
            Err(error) => {
                // §12: the attempt failed, not the account — hold the founding keys for the
                // retry, whatever took the request down.
                if let Some(keys) = registration_keys {
                    self.pending_registration = Some(keys);
                }
                // A captcha refusal is not a dead form: the attempt consumed the challenge
                // either way, so tell the UI to drop it and draw a fresh one, and let the
                // ordinary failure path below keep the form standing for the retry. A refusal
                // that carries its own replacement skips even that round trip — the server
                // minted the next challenge with the refusal, so it is handed straight to the
                // form as though it had been fetched.
                match &error {
                    RestError::Server {
                        captcha: Some(challenge),
                        ..
                    } => self
                        .sink
                        .send(Event::CaptchaChallenge((**challenge).clone())),
                    RestError::Server { symbol, .. }
                        if CAPTCHA_REFUSAL_SYMBOLS.contains(&symbol.as_str()) =>
                    {
                        self.sink.send(Event::CaptchaRefused);
                    }
                    _ => {}
                }
                return self.fail(error.to_string());
            }
        };
        // The account exists and the vault below is about to hold the keys, so whatever a failed
        // attempt left pending is spent: a later registration is a genuinely new account and
        // must mint a genuinely new root.
        self.pending_registration = None;

        // A vault whose passphrase just opened keeps its keys; otherwise this device is new and needs
        // a fresh identity. Generating one unconditionally would silently replace the key peers have
        // verified, and every safety number would change with no explanation.
        //
        // A *new* identity gets the account-root treatment: a registration is the founding device of
        // a brand-new account, so it mints the root, derives its E2EE identity from the root's E2EE
        // domain (recoverable from a `.migo` container, which is the point), and enrols a device
        // credential. A sign-in is an *additional* device of an account that exists: fresh random
        // E2EE identity, fresh credential, no root — additional devices never inherit the founding
        // device's material.
        let mut keys = match registration_keys {
            Some(keys) => keys,
            None => DeviceKeys::additional(),
        };
        // Captured before `establish` takes the keys: the account-root follow-ups (publishing the
        // identity, registering the first wallet) happen once the session exists, but they need the
        // material the session store is about to own.
        let root = keys.root();
        keys.session = Some(SavedSession {
            server_url: crate::config::rest_base_url(&server),
            account_id: grant.account_id,
            device_id: grant.device_id,
            username: identifier.clone(),
            refresh_token: grant.refresh_token.clone(),
        });
        // The Activity list and peer-fingerprint memory this process already holds for this
        // account are newer than whatever the vault last sealed; a different account's never
        // cross over.
        self.carry_live_records(&mut keys, grant.account_id);
        if let Err(error) = vault::save(&self.vault_path, &passphrase, &keys) {
            return self.fail(error.to_string());
        }

        self.establish(
            server,
            rest,
            keys,
            grant.account_id,
            grant.device_id,
            grant.session_id,
            identifier,
            grant.access_token,
        )
        .await;

        // The legacy upgrade door: a device that holds the root tells the server so, idempotently,
        // on every sign-in. A server that already has the keys reconciles to the same rows; one
        // that has never seen them records them now, which is what makes the account
        // ML-DSA-loginable at all. A refusal here is a toast, not a failed sign-in — the passphrase
        // already worked.
        if root.is_some() {
            self.publish_root_material().await;
            // And the account's first wallet: a registration is the moment wallet 0 comes into
            // existence, so it is registered now rather than waiting for a settings screen to
            // ask. Idempotent, like the publish above — a device that signs in again reconciles
            // to the same rows.
            self.sync_wallets().await;
        }
    }

    /// Fetches a captcha challenge for a form.
    ///
    /// Its failures are reported through [`Event::CaptchaUnavailable`] rather than
    /// [`Self::fail`], because a challenge that will not load is not a failed sign-in: `fail`
    /// would flip the connection state of a form that never submitted, and release a busy flag
    /// that was never set.
    async fn fetch_captcha(&mut self, server: ServerEndpoint, mode: Option<String>) {
        let outcome = match Rest::new(&crate::config::rest_base_url(&server)) {
            Ok(rest) => rest.request_captcha(mode.as_deref()).await,
            Err(error) => Err(error),
        };
        match outcome {
            Ok(challenge) => self.sink.send(Event::CaptchaChallenge(challenge)),
            Err(error) => self.sink.send(Event::CaptchaUnavailable {
                reason: error.to_string(),
            }),
        }
    }

    /// Opens the vault and resumes its saved sign-in.
    async fn unlock(&mut self, passphrase: String) {
        self.sink.send(Event::Connection(Connection::Connecting));

        let keys = match vault::load(&self.vault_path, &passphrase) {
            Ok(keys) => keys,
            Err(error) => return self.fail(error.to_string()),
        };
        let Some(saved) = keys.session.clone() else {
            return self.fail("this vault has no saved sign-in; sign in again".to_owned());
        };
        // Reconstruct the server endpoint from the legacy string field, falling back to the dev
        // policy for any shape the parser does not recognise. The saved URL is the one this device
        // last successfully used, so the form is not consulted on unlock.
        let server = crate::config::server_endpoint_from_url(&saved.server_url);
        let rest = match Rest::new(&crate::config::rest_base_url(&server)) {
            Ok(rest) => rest,
            Err(error) => return self.fail(error.to_string()),
        };
        let grant = match rest.refresh(&saved.refresh_token, saved.device_id).await {
            Ok(grant) => grant,
            Err(error) => {
                // A dead refresh token is not a dead account on a device that holds the root: the
                // ML-DSA login ceremony signs in without a passphrase, using the very keys the
                // vault just gave back. Only when that too is impossible — no root, no credential,
                // or a server that refuses — does the refresh failure stand as the answer.
                match self.ceremony_login(&rest, &keys, &saved).await {
                    Some(grant) => grant,
                    None => return self.fail(error.to_string()),
                }
            }
        };

        // The server rotates the refresh token on every exchange, so the vault has to be rewritten or
        // the next unlock would present a token the server has already retired — which it treats as
        // refresh reuse, and rightly so.
        let mut keys = keys;
        keys.session = Some(SavedSession {
            refresh_token: grant.refresh_token.clone(),
            ..saved.clone()
        });
        // As at sign-in: this process's own record of the same account's transactions and peer
        // fingerprints is the newer copy, and it is the one that gets sealed.
        self.carry_live_records(&mut keys, grant.account_id);
        if let Err(error) = vault::save(&self.vault_path, &passphrase, &keys) {
            return self.fail(error.to_string());
        }

        self.establish(
            server,
            rest,
            keys,
            grant.account_id,
            grant.device_id,
            grant.session_id,
            saved.username,
            grant.access_token,
        )
        .await;
    }

    /// The passphraseless sign-in: the ML-DSA login ceremony, run from a vault that holds the root
    /// and this device's credential.
    ///
    /// `None` means "this device cannot sign in this way" — no root, no credential, a challenge
    /// that would not issue, a signature that failed — and the caller reports the failure of the
    /// thing it was actually trying (the refresh exchange), rather than a stack of ceremony detail
    /// the user cannot act on. The server's own anti-enumeration shape makes this the right
    /// behaviour: an unknown identifier and a wrong passphrase produce the same `CHALLENGE_INVALID`,
    /// so a ceremony error message would only ever be this client's guess.
    async fn ceremony_login(
        &mut self,
        rest: &Rest,
        keys: &DeviceKeys,
        saved: &SavedSession,
    ) -> Option<Grant> {
        let identity = keys.identity_key()?;
        let credential = keys.device_credential()?;
        let challenge = rest
            .identity_login_challenge(&saved.username, saved.device_id)
            .await
            .ok()?;
        // Signed exactly as received, never re-encoded: the server verifies against the bytes it
        // stored, so a canonicalising client would sign a different message and fail.
        let payload = base64_decode(&challenge.payload)?;
        let identity_signature = identity.sign_login(&payload).ok()?;
        let device_signature = credential.sign_login(&payload).ok()?;
        rest.identity_login(
            challenge.challenge_id,
            &identity_signature,
            &device_signature,
        )
        .await
        .ok()
    }

    /// Brings up the gateway connection and publishes this device's public keys.
    #[allow(clippy::too_many_arguments)]
    async fn establish(
        &mut self,
        server: ServerEndpoint,
        rest: Rest,
        keys: DeviceKeys,
        account_id: Id,
        device_id: Id,
        session_id: Id,
        username: String,
        access_token: String,
    ) {
        let safety_number = model::safety_number(&keys.identity_public().fingerprint());
        let holds_root = keys.root.is_some();
        let account = Account {
            account_id,
            device_id,
            session_id,
            username,
            safety_number,
            holds_root,
        };
        // The Activity list and the peer-fingerprint memory are sealed with the keys they arrived
        // with; they become the worker's to keep from here until the session ends.
        let txs = keys.txs.clone();
        let peers = keys.peer_fingerprints.clone();
        let last_backup_at = keys.last_backup_at;
        let sessions = SessionStore::new(keys);
        // The group layer signs with the same identity the pairwise layer identifies with, so a
        // broadcast's signature is verifiable against the identity its distributions carried.
        let groups = GroupStore::new(migo_crypto::IdentitySecret::from_seeds(
            sessions.identity().expose_signing_seed(),
            sessions.identity().expose_exchange_seed(),
        ));

        // The room bridge read back before the first connect: the reconnect path re-subscribes
        // room topics from `rooms_watched`, and a fresh sign-in would hold none — the join's
        // answer, the one wire moment that names a room's conversation, belongs to the process
        // that joined. Reading it here means the first connect of this process watches the same
        // rooms the last one did, without a re-join's membership side effects (§156: a seated
        // member's join is free of fan-out, but the desktop's own record of the join should not
        // have to depend on repeating it).
        let room_conversations: HashMap<Id, Id> =
            self.room_bridges.load(account_id).into_iter().collect();
        let rooms_watched: HashSet<Id> = room_conversations.keys().copied().collect();

        // The listened marks read back beside the room bridges, for the same reason: they are
        // this account's own memory on this device, and a session that started with them
        // empty would draw every note it had already heard as unheard.
        self.listened = self.voice_listened.load(account_id);

        self.signed = Some(Signed {
            server,
            rest,
            account: account.clone(),
            access_token,
            sessions,
            groups,
            pending: HashMap::new(),
            bundles: HashMap::new(),
            devices: HashMap::new(),
            members: HashMap::new(),
            conversations_watched: HashSet::new(),
            rooms_watched,
            room_conversations,
            watched: HashSet::new(),
            e2e: HashSet::new(),
            media_keys: HashMap::new(),
            sequences: HashMap::new(),
        });
        // A sign-in is a session boundary at the worker's own door too: no gateway session to
        // resume (the first connect mints one), and no walks in flight whose pages a previous
        // session's conversations still owe.
        self.session = None;
        self.catchups.clear();
        self.earlier_asks.clear();
        self.sync_asks.clear();
        self.txs = Some((account_id, txs));
        self.peers = Some((account_id, peers));
        self.last_backup_at = Some((account_id, last_backup_at));

        self.sink.send(Event::SignedIn(account));
        // After SignedIn, which resets the panes: the stamp is a session-start fact the checkup
        // owns, and an event filed before the reset would be an event nobody kept.
        self.sink.send(Event::BackupState { last_backup_at });
        self.sink.send(Event::ChainActivity(self.chain_rows()));
        // The listened marks, the same way: they are this account's own thread state, seeded
        // after the reset so a previous account's marks are replaced rather than merged.
        self.sink.send(Event::VoiceNotesListened {
            media_ids: self.listened.iter().copied().collect(),
        });
        self.connect().await;
    }

    /// Folds this process's live account records — the Activity list, the last-seen peer
    /// identity fingerprints, and the last-backup stamp — into the keys a passphrase moment is
    /// about to seal.
    ///
    /// The worker deliberately holds no passphrase after unlock, so mid-session updates to these
    /// lists live only in its memory and reach the vault only when a sign-in, a restore or a
    /// rotation next opens it: this fold is that moment, at every door the vault is saved
    /// through. A different account's records never cross over, so a second account signing in
    /// over the same window inherits nothing — not another account's history, not its key-change
    /// memory, and not its backup date.
    fn carry_live_records(&self, keys: &mut DeviceKeys, account_id: Id) {
        if let Some((id, txs)) = self.txs.as_ref() {
            if *id == account_id {
                keys.txs = txs.clone();
            }
        }
        if let Some((id, peers)) = self.peers.as_ref() {
            if *id == account_id {
                keys.peer_fingerprints = peers.clone();
            }
        }
        if let Some((id, at)) = self.last_backup_at.as_ref() {
            if *id == account_id {
                keys.last_backup_at = *at;
            }
        }
    }

    /// Connects the gateway, retrying with backoff until it succeeds or the session ends.
    async fn connect(&mut self) {
        let Some(signed) = self.signed.as_ref() else {
            return;
        };
        let hello = migo_protocol::Hello {
            protocol_version: migo_protocol::PROTOCOL_VERSION,
            client: ClientInfo {
                platform: migo_protocol::Platform::Desktop,
                app_version: env!("CARGO_PKG_VERSION").to_owned(),
                os_version: None,
                device_model: None,
            },
            // Only what this client actually implements. Claiming a feature it cannot honour would
            // make the server send frames it then ignores, and the user would see silence rather than
            // an error. BATCHING is honoured since inbound envelopes are unpacked in `on_record`;
            // RICH_PRESENCE is honoured since the profile pane's status field saves through
            // PROFILE_UPDATE.custom_status — and only draws as an editable field on sessions whose
            // WELCOME put the bit in the intersection. CALLS, ROOMS, and ECONOMY are honoured
            // because the client speaks those families' opcodes (the call signal path, the rooms
            // screens, the wallet and gifting) and the server now refuses a family's frames on a
            // session that did not announce its bit — the mask is the switch's own key. GROUP_CALL
            // is honoured for the seat this build takes: the SFU join, the roster it answers, and
            // the call-key rotation the roster's own movement triggers.
            features: features::E2E_V1
                | features::PRESENCE
                | features::TYPING
                | features::COMPRESSION
                | features::BATCHING
                | features::RICH_PRESENCE
                | features::CALLS
                | features::GROUP_CALL
                | features::ROOMS
                | features::ECONOMY,
            locale: "en".to_owned(),
            bandwidth_mode: migo_protocol::BandwidthMode::Auto,
            access_token: Some(signed.access_token.clone()),
            device_id: Some(signed.account.device_id),
            // The resume ask of the session this process still holds, if it holds one: a
            // disconnect costs the frames the outage dropped, and the server's retained ring
            // answers with exactly those rather than making the client re-sync from scratch
            // (§158). `None` on the first connect of a process, after a fresh WELCOME, after a
            // refused resume, and at sign-out — the moments there is no session left to ask for.
            resume: self.session.as_ref().map(GatewaySession::resume_request),
        };

        self.sink.send(Event::Connection(Connection::Connecting));
        // The endpoint names the transport. TCP is the native default; QUIC is the second option.
        // Both are tried only when the user picked them, and a server that does not negotiate the
        // bit is not an error — the worker falls back to the WebSocket path and says so in the
        // connection state.
        let picked = signed.server.transport;
        let mut fell_back = false;
        let mut fallback_reason = String::new();
        let connected = match picked {
            Transport::Tcp => match tcp::connect(&signed.server, hello.clone()).await {
                Ok((gateway, welcome)) => {
                    if welcome.features & features::TCP_TRANSPORT == 0 {
                        // The contract, not a fault: the negotiated set is the intersection, and
                        // this node did not offer the bit. Close cleanly and take the WebSocket
                        // path rather than stranding the session on a transport the server never
                        // agreed to.
                        let mut gateway = gateway;
                        gateway.close().await;
                        fell_back = true;
                        fallback_reason =
                            "the server did not negotiate TCP; connected over WebSocket".to_owned();
                        self.connect_websocket(hello).await
                    } else {
                        Ok((Realtime::Tcp(gateway), welcome))
                    }
                }
                Err(_) => {
                    // An unreachable TCP listener on a TCP-picked endpoint still has a working
                    // WebSocket path; fall back rather than erroring a server the user can reach.
                    fell_back = true;
                    fallback_reason =
                        "could not reach the TCP listener; connected over WebSocket".to_owned();
                    self.connect_websocket(hello).await
                }
            },
            Transport::Quic => match quic::connect(&signed.server, hello.clone()).await {
                Ok((gateway, welcome)) => {
                    if welcome.features & features::QUIC == 0 {
                        // The contract, not a fault: the negotiated set is the intersection, and
                        // this node did not offer the bit. Close cleanly and take the default
                        // path rather than stranding the session on a transport the server never
                        // agreed to.
                        let mut gateway = gateway;
                        gateway.close().await;
                        fell_back = true;
                        fallback_reason =
                            "the server did not negotiate QUIC; connected over WebSocket"
                                .to_owned();
                        self.connect_websocket(hello).await
                    } else {
                        Ok((Realtime::Quic(gateway), welcome))
                    }
                }
                Err(_) => {
                    // An unreachable QUIC listener on a QUIC-picked endpoint still has a working
                    // default path; fall back rather than erroring a server the user can reach.
                    fell_back = true;
                    fallback_reason =
                        "could not reach the QUIC listener; connected over WebSocket".to_owned();
                    self.connect_websocket(hello).await
                }
            },
            Transport::WebSocket => self.connect_websocket(hello).await,
        };
        let asked_to_resume = self.session.is_some();
        match connected {
            Ok((realtime, welcome)) => {
                self.gateway = Some(realtime);
                self.retry = None;
                // Arm the heartbeat from what this node advertised: half the interval, floored
                // at 5s, so a session that goes quiet only from idleness — the desktop's
                // resting state — never meets the two-interval deadline. Re-armed here on
                // every connect, and disarmed in `on_disconnect`.
                self.heartbeat = Some(heartbeat_interval(welcome.limits.heartbeat_ms));
                // The negotiated set is per-session, so the pane's status gate is re-stated on
                // every connect: a node that stopped carrying the bit between two sessions must
                // not leave an editable field on screen that this session would only refuse.
                let rich_presence = welcome.features & features::RICH_PRESENCE != 0;
                self.sink.send(Event::RichPresence(rich_presence));
                if fell_back {
                    self.sink
                        .send(Event::Connection(Connection::Fallback(fallback_reason)));
                } else {
                    self.sink.send(Event::Connection(Connection::Online));
                }
                if asked_to_resume && welcome.resumed == Some(true) {
                    // The server took the resume: same session, retained topics re-applied,
                    // ring tail on its way (§158). The frame counter above kept counting into
                    // the same `GatewaySession`, so the next disconnect's ask is already
                    // correct, and every correlation-keyed ask this session minted is answered
                    // in the replay's own order — new requests could not have minted fresh ids
                    // between the drop and the replay, because the worker was off the socket.
                    // What a resumed session must *not* do is re-sync: subscriptions live, the
                    // rosters live, and a re-read would only race the replay's own events.
                    return;
                }
                // A fresh session, resume or not: the WELCOME minted a session id this process
                // has never counted for, so the counter restarts at zero and the next drop's
                // resume asks from this session's own first sequenced frame.
                self.session = Some(GatewaySession {
                    id: welcome.session_id,
                    sequenced: 0,
                });
                // The in-flight walks belong to the session that minted their correlations: a
                // page that never arrived cannot be continued by a reply this session will
                // never send. The per-conversation watermarks survive — they are facts about
                // the conversation, not the session — and the list read below re-arms the
                // catch-ups they still need.
                self.catchups.clear();
                self.earlier_asks.clear();
                self.sync_asks.clear();
                self.publish_keys().await;
                // A fresh session holds no subscriptions, so the accounts this device watches for
                // presence go back to "never subscribed" and the own-topic subscribe below is the
                // only one that can be sent unconditionally. The conversation topics go the same
                // way — they are re-subscribed by the list read that follows, which is sent
                // unconditionally below for exactly this reason. The room topics are the
                // exception: the list read cannot recover them (the conversation summary names no
                // room id), so their ids are kept across the reconnect and re-subscribed here.
                let rooms_to_watch: Vec<Id> = self
                    .signed
                    .as_ref()
                    .map(|signed| signed.rooms_watched.iter().copied().collect())
                    .unwrap_or_default();
                if let Some(signed) = self.signed.as_mut() {
                    signed.watched.clear();
                    signed.conversations_watched.clear();
                }
                self.watch_topics(TopicKind::Room, rooms_to_watch).await;
                self.subscribe_self().await;
                self.announce_presence().await;
                self.request_conversations().await;
                // The graph is refreshed on every reconnect for the same reason the conversation
                // list is: the other devices of this account act on it too, and a client that
                // never re-reads shows a friendship that ended an hour ago.
                self.request_relationships().await;
                // The dashboard's own facts ride the same reconnect: the rooms, the suggestions,
                // the inbox, and the wallet are the session's other four screens' first reads,
                // and a reconnect is a session boundary to them too.
                self.request_rooms(String::new()).await;
                self.request_suggestions().await;
                self.request_notifications().await;
                self.request_wallet().await;
            }
            Err(error) => {
                // A refused resume is the server saying the ring has moved on, not a failure to
                // reach the server: drop the session state and connect once more, now asking
                // for nothing. The re-connect's fresh WELCOME takes the full resync path above,
                // which is exactly what RESUME_REQUIRED's message asks for — "re-sync by
                // cursor". Every other error is a real one and takes the retry path below.
                if asked_to_resume {
                    if let GatewayError::Refused {
                        code: codes::RESUME_REQUIRED,
                        ..
                    } = error
                    {
                        self.session = None;
                        // Boxed because `connect` recurses here: the refused-resume re-connect
                        // asks for nothing, so the recursion is one level deep, but the future
                        // still cannot hold itself by value.
                        Box::pin(self.connect()).await;
                        return;
                    }
                }
                self.sink
                    .send(Event::Connection(Connection::Failed(error.to_string())));
            }
        }
    }

    /// Opens the WebSocket path. Shared by the WebSocket-picked endpoint and by the fallbacks
    /// from a TCP or QUIC endpoint that could not be taken (a failed dial, or a WELCOME without
    /// the negotiated bit), so every path that lands here builds the same HELLO.
    async fn connect_websocket(
        &self,
        hello: migo_protocol::Hello,
    ) -> Result<(Realtime, migo_protocol::Welcome), GatewayError> {
        let Some(signed) = self.signed.as_ref() else {
            return Err(GatewayError::Closed);
        };
        // A WebSocket-picked endpoint names its own gateway port and TLS posture, so it is
        // dialled as typed. A TCP- or QUIC-picked endpoint's `gateway_port` names the *native*
        // listener, and the server's WebSocket rides the REST listener (`/ws` on the same port),
        // so the fallback dials the origin — the web client's posture — rather than the native
        // scheme at a port nothing answers.
        let url = match signed.server.transport {
            Transport::WebSocket => crate::config::gateway_url(&signed.server),
            _ => crate::config::websocket_origin_url(&signed.server),
        };
        let (gateway, welcome) = Gateway::connect(&url, hello).await?;
        Ok((Realtime::WebSocket(Box::new(gateway)), welcome))
    }

    /// One scheduled reconnect attempt, run from the select loop.
    ///
    /// Takes the plan first, so a failure to re-arm cannot leave a timer firing in a tight loop.
    async fn reconnect(&mut self) {
        let Some(plan) = self.retry.take() else {
            return;
        };
        if self.signed.is_none() {
            return;
        }
        self.connect().await;
        if self.gateway.is_some() {
            return;
        }
        match plan.after_failure(&mut OsRandom) {
            Some(next) => self.retry = Some(next),
            None => self.sink.send(Event::Connection(Connection::Failed(
                "could not reconnect; check the server address and try signing in again".to_owned(),
            ))),
        }
    }

    /// Publishes this device's public identity and prekeys.
    async fn publish_keys(&mut self) {
        let Some(signed) = self.signed.as_ref() else {
            return;
        };
        let remaining = signed.sessions.one_time_remaining();
        let keys = signed.sessions.keys();
        let signed_prekey = keys.signed_prekey_signed();
        let message = migo_protocol::KeyPublish {
            identity_key: keys.identity_public().to_bytes().to_vec(),
            signed_prekey_id: signed_prekey.key_id,
            signed_prekey: signed_prekey.public_key.to_vec(),
            signed_prekey_signature: signed_prekey.signature.to_vec(),
            one_time_prekeys: keys
                .one_time_public()
                .into_iter()
                .map(|(key_id, public_key)| migo_protocol::PrekeyEntry {
                    key_id,
                    public_key: public_key.to_vec(),
                })
                .collect(),
        };
        self.request(Opcode::KeyPublish, &message).await;

        // Worth saying out loud before the pool empties. A peer can still start a session from the
        // signed prekey without a one-time key, so nothing breaks — but that first message loses the
        // one-time key's forward secrecy, and the user is the only one who can fix it, by signing in
        // again with the passphrase so a fresh pool can be generated and sealed into the vault. This
        // worker deliberately does not hold the passphrase after unlock, so it cannot do that itself.
        if remaining < ONE_TIME_PREKEY_LOW_WATER {
            self.sink.toast(
                format!("Only {remaining} one-time keys left. Sign in again to replenish them."),
                ToastKind::Info,
            );
        }
    }

    async fn request_conversations(&mut self) {
        let message = migo_protocol::ConversationListRequest {
            limit: 100,
            cursor: None,
        };
        self.request(Opcode::ConversationList, &message).await;
    }

    /// Starts a catch-up walk for one conversation, bounded or not.
    ///
    /// `from` is the "have" the walk's *first* page asks from — zero for a thread the UI holds
    /// nothing of (a fresh open replays from the conversation's floor, the web client's own
    /// rule), the watermark for everything else. Later pages ask from the watermark the pages
    /// themselves have advanced, so the walk climbs one contiguous brick at a time.
    ///
    /// `to_seq` names the hole's top for a gap walk — the walk stops once the watermark reaches
    /// it, so it never tails live traffic; `None` walks to the server's live edge, which is what
    /// an open thread asks for and what a list-read refresh asks for when the server's
    /// `last_seq` has moved past the watermark. One walk per conversation: a second trigger
    /// while one runs is the running walk's business, and a duplicate would race its pages.
    ///
    /// The refusals are loop guards, not errors. A walk already in flight needs nothing. A
    /// watermark stalled at its own value has already been asked and already answered without
    /// moving — the hole below it is gone from the server's history, and re-asking is the hot
    /// loop the stall exists to prevent. A `to_seq` at or below the watermark is a hole already
    /// filled (the gap's own event moved the watermark past its top by the time the walk was
    /// triggered, or the gap closed between the trigger and here).
    async fn catch_up(&mut self, conversation_id: Id, from: u64, to_seq: Option<u64>) {
        if self.catchups.contains_key(&conversation_id) {
            return;
        }
        if let Some(signed) = self.signed.as_ref() {
            if let Some(account) = signed.sequences.get(&conversation_id) {
                if account.stalled_at == Some(account.watermark) {
                    return;
                }
                if let Some(to) = to_seq {
                    if to <= account.watermark {
                        return;
                    }
                }
            }
        }
        self.catchups.insert(
            conversation_id,
            CatchUp {
                to_seq,
                pages_left: MAX_CATCHUP_PAGES,
            },
        );
        self.sync_page(conversation_id, from).await;
    }

    /// Sends one forward SYNC page for a walk already in `catchups`, from the "have" the caller
    /// names — the walk's chosen floor for the first page, the watermark for every page after
    /// it. The contiguous watermark is the only cursor that heals: a page asked from the maximum
    /// would leave the hole below it unfilled for good (§152's gapless seqs are what make the
    /// hole knowable at all).
    ///
    /// The correlation is remembered beside the ask, because the reply that ends a walk is not
    /// always a page: a ranged repair whose range the server has purged is answered
    /// SEQUENCE_GAP, and an error frame names no conversation — only the ask's own correlation
    /// can say which walk the refusal retires.
    async fn sync_page(&mut self, conversation_id: Id, have_seq: u64) {
        let to_seq = self
            .catchups
            .get(&conversation_id)
            .and_then(|walk| walk.to_seq);
        let message = migo_protocol::SyncRequest {
            conversation_id,
            have_seq,
            limit: SYNC_PAGE,
            to_seq,
            backwards: None,
        };
        if let Some(correlation) = self.send_and_remember(Opcode::Sync, &message).await {
            self.sync_asks.insert(correlation, conversation_id);
        }
    }

    /// Asks for one page of history *below* what the thread holds, for the "Load earlier" row.
    ///
    /// The ask is backwards (`history_before`): the server reads down from `before_seq`, or
    /// from the conversation's newest when `before_seq` is zero — the first ask of a thread
    /// whose catch-up stopped at the page budget, which wants the newest end first because
    /// that is the end the open window shows. The correlation is remembered because the SYNC
    /// answer names its conversation but not its direction: only the ask's own correlation can
    /// say the page that arrived is an earlier page, to be prepended, rather than a catch-up
    /// page, to be walked onward.
    async fn request_earlier(&mut self, conversation_id: Id, before_seq: u64) {
        let message = migo_protocol::SyncRequest {
            conversation_id,
            have_seq: before_seq,
            limit: SYNC_PAGE,
            to_seq: None,
            backwards: Some(true),
        };
        if let Some(correlation) = self.send_and_remember(Opcode::Sync, &message).await {
            self.earlier_asks.insert(correlation, conversation_id);
        }
    }

    /// Fetches one account's key bundles, without sending anything.
    ///
    /// The send path asks for bundles only when it needs a session started; this is the ask a
    /// conversation window makes so it can show the peer's safety numbers from the first frame
    /// they are looked at, not from the first message. The bundles land in
    /// [`Worker::on_bundles`] like any other fetch, and the identities they carry reach the UI
    /// as [`Event::PeerIdentity`] entries.
    async fn fetch_peer_keys(&mut self, user_id: Id) {
        let request = migo_protocol::KeyBundleRequest {
            user_id,
            device_id: None,
        };
        self.request(Opcode::KeyBundleFetch, &request).await;
    }

    async fn start_direct(&mut self, username: String) {
        // A username has to become an account id before a conversation can name its members, and the
        // profile lookup is the only thing that can do it. The conversation is created when the
        // response arrives.
        let message = migo_protocol::ProfileRequest {
            user_ids: Vec::new(),
        };
        let _ = message;
        // PROFILE_FETCH takes ids, not names, so a username search is a REST concern rather than a
        // gateway one. Until that endpoint exists on the server, accept an id typed directly and say
        // so plainly rather than failing silently.
        match Id::parse(username.trim()) {
            Ok(peer) => {
                let Some(signed) = self.signed.as_ref() else {
                    return;
                };
                let create = migo_protocol::ConversationCreateRequest {
                    kind: ConversationKind::Direct,
                    members: vec![signed.account.account_id, peer],
                    title: None,
                };
                self.request(Opcode::ConversationCreate, &create).await;
            }
            Err(_) => self.sink.toast(
                "enter the account id of the person to message",
                ToastKind::Info,
            ),
        }
    }

    /// Starts a direct conversation with an account id already held.
    ///
    /// The parsed-id path of [`Self::start_direct`] without the parsing: a search hit or a
    /// suggestion already names an account, and round-tripping its text form would be a parse of
    /// a value this worker minted.
    async fn start_direct_by_id(&mut self, peer: Id) {
        let Some(signed) = self.signed.as_ref() else {
            return;
        };
        let create = migo_protocol::ConversationCreateRequest {
            kind: ConversationKind::Direct,
            members: vec![signed.account.account_id, peer],
            title: None,
        };
        self.request(Opcode::ConversationCreate, &create).await;
    }

    /// Creates a group conversation: the founding members and a title, the way the web
    /// client's new-group form sends it.
    ///
    /// The member list names the *other* members — the server adds the caller and drops the
    /// caller's own id without complaint, the same tolerance the direct path relies on — and
    /// the caller lands as the group's first founder. The title is the group's alone on this
    /// wire; an empty one is sent as `None` so the server's own "a group's title cannot be
    /// empty" refusal (which fires only for a present-but-empty title) is never asked for.
    async fn create_group(&mut self, members: Vec<Id>, title: String) {
        let trimmed = title.trim().to_owned();
        let create = migo_protocol::ConversationCreateRequest {
            kind: ConversationKind::Group,
            members,
            title: (!trimmed.is_empty()).then_some(trimmed),
        };
        self.request(Opcode::ConversationCreate, &create).await;
    }

    /// Invites members into a group. Any current member's right; the server drops
    /// already-seated names and refuses the size cap, both in sentences worth reading.
    async fn invite_to_group(&mut self, conversation_id: Id, members: Vec<Id>) {
        let invite = migo_protocol::ConversationInviteRequest {
            conversation_id,
            members,
        };
        self.request(Opcode::ConversationInvite, &invite).await;
    }

    /// Leaves a group conversation. The ack is remembered because the wire's reply names
    /// nothing: the conversation the request carried is the only key its answer can be filed
    /// under, the same trade a room leave makes with `pending_leave`.
    async fn leave_group(&mut self, conversation_id: Id) {
        self.group_leave = Some(conversation_id);
        let leave = migo_protocol::ConversationLeaveRequest { conversation_id };
        self.request(Opcode::ConversationLeave, &leave).await;
    }

    /// Reads a group's roster for the panel — a different ask from the send path's roster
    /// read, filed separately because one promotes an encryption audience while this one
    /// fills the window a person reads.
    async fn request_group_roster(&mut self, conversation_id: Id) {
        self.group_rosters.insert(conversation_id, ());
        let roster = migo_protocol::ConversationRosterRequest { conversation_id };
        self.request(Opcode::ConversationRoster, &roster).await;
    }

    /// A founder mutes one member until `until`, or lifts the mute when `until` is `None`.
    /// The server is the judge of the role and the immunity; this only carries the intent.
    async fn mute_group_member(
        &mut self,
        conversation_id: Id,
        target_id: Id,
        until: Option<Timestamp>,
    ) {
        let mute = migo_protocol::ConversationMuteRequest {
            conversation_id,
            target_id,
            until,
        };
        self.request(Opcode::ConversationMute, &mute).await;
    }

    /// A founder removes a member outright. No frame announces it beyond the member event
    /// everyone in the group hears.
    async fn kick_group_member(&mut self, conversation_id: Id, target_id: Id) {
        let kick = migo_protocol::ConversationKickRequest {
            conversation_id,
            target_id,
        };
        self.request(Opcode::ConversationKick, &kick).await;
    }

    /// Starts a kick vote, or adds this account's voice to one already running. The reply —
    /// this voice's own tally — is decoded in `on_group_vote_reply`; the same tally for
    /// everyone else arrives as a vote event. The (conversation, target) pair is remembered
    /// because the reply carries neither, only the numbers.
    async fn vote_kick_member(&mut self, conversation_id: Id, target_id: Id) {
        self.pending_vote = Some((conversation_id, target_id));
        let vote = migo_protocol::ConversationVoteKickRequest {
            conversation_id,
            target_id,
        };
        self.request(Opcode::ConversationVoteKick, &vote).await;
    }

    /// A founder renames a group. The reply is the refreshed summary (the same shape a create
    /// answers with), and the group's other members learn the name through a state event.
    async fn rename_group(&mut self, conversation_id: Id, title: String) {
        let update = migo_protocol::ConversationUpdateRequest {
            conversation_id,
            title: Some(title),
        };
        self.request(Opcode::ConversationUpdate, &update).await;
    }

    async fn send_typing(&mut self, conversation_id: Id, typing: bool) {
        let message = migo_protocol::TypingEvent {
            conversation_id,
            state: if typing {
                migo_protocol::TypingState::Start
            } else {
                migo_protocol::TypingState::Stop
            },
            user_id: None,
        };
        // Best effort by design: a lost typing indicator costs nothing, and retrying one would put a
        // stale "typing…" on someone's screen after the message had already arrived.
        self.request(Opcode::Typing, &message).await;
    }

    async fn mark_read(&mut self, conversation_id: Id, seq: u64) {
        let message = migo_protocol::MessageReceipt {
            conversation_id,
            kind: migo_protocol::ReceiptKind::Read,
            seq,
            user_id: None,
            at: None,
        };
        self.request(Opcode::MessageReceipt, &message).await;
    }

    /// Subscribes this session to its own account's user topic.
    ///
    /// That topic is where the server puts the events addressed to this account rather than to
    /// one of its conversations: friend requests, acceptances, notifications. The gateway grants
    /// it by right (a user's own presence stream is theirs), so a refusal here would mean the
    /// session is not really authenticated — which the handshake would already have caught.
    async fn subscribe_self(&mut self) {
        let Some(signed) = self.signed.as_ref() else {
            return;
        };
        let request = SubscribeRequest {
            topics: vec![Topic {
                kind: TopicKind::User,
                id: signed.account.account_id,
            }],
        };
        self.request(Opcode::Subscribe, &request).await;
    }

    /// Announces this device as online.
    ///
    /// What the web client does after its connect, done here for the same reason: presence on
    /// this server is per-device and client-reported, so a client that never speaks PRESENCE_SET
    /// reads as unobserved to everyone watching. Best effort — a set that fails costs one green
    /// dot, and the reconnect path will try again anyway.
    ///
    /// The custom status stays absent, and this is the only PRESENCE_SET this client sends: the
    /// server refuses one on this wire (`FEATURE_DISABLED` — a status is meant to outlive a
    /// disconnect, which a presence entry does not), and a frame the sender knows will be
    /// refused is a frame that is not sent.
    async fn announce_presence(&mut self) {
        let message = migo_protocol::PresenceUpdate {
            state: migo_protocol::PresenceState::Online,
            custom_status: None,
        };
        self.request(Opcode::PresenceSet, &message).await;
    }

    /// Subscribes to the user topics of accounts this device wants presence for.
    ///
    /// Only unwatched ids are sent, and the batch is capped so one refresh on a large graph
    /// cannot produce an unbounded frame. A refusal is silent by design: the server answers
    /// "no" without a reason so SUBSCRIBE cannot be used to probe, and a client that grilled
    /// the user about every declined watch would be inventing reasons the server chose not to
    /// give.
    async fn watch_users(&mut self, ids: Vec<Id>) {
        const WATCH_BATCH: usize = 128;
        let topics: Vec<Topic> = {
            let Some(signed) = self.signed.as_mut() else {
                return;
            };
            ids.into_iter()
                .filter(|id| signed.watched.insert(*id))
                .take(WATCH_BATCH)
                .map(|id| Topic {
                    kind: TopicKind::User,
                    id,
                })
                .collect()
        };
        if topics.is_empty() {
            return;
        }
        let request = SubscribeRequest { topics };
        self.request(Opcode::Subscribe, &request).await;
    }

    /// Requests the account's whole social graph.
    async fn request_relationships(&mut self) {
        let message = RelationshipListReq {
            limit: 200,
            kind: None,
            cursor: None,
        };
        self.request(Opcode::RelationshipList, &message).await;
    }

    /// Sends a friend request to whatever the user typed, if it names an account.
    async fn add_friend(&mut self, user_id: String) {
        match Id::parse(user_id.trim()) {
            Ok(target) => {
                let message = FriendTarget { user_id: target };
                self.request(Opcode::FriendRequest, &message).await;
            }
            Err(_) => self
                .sink
                .toast("enter the account id of the person to add", ToastKind::Info),
        }
    }

    /// Accepts or declines a pending request.
    async fn respond_friend(&mut self, user_id: Id, accept: bool) {
        let message = FriendRespond { user_id, accept };
        self.request(Opcode::FriendRespond, &message).await;
    }

    /// Fetches names and presence for a set of accounts in one PROFILE_FETCH.
    ///
    /// The friends list and the direct-conversation titles both render through the names map, so
    /// one fetch here serves two screens. Requests are capped because a profile answer costs the
    /// server a row read per account, and an account with a thousand relationships should not
    /// turn one refresh into a thousand-row fan-out.
    async fn fetch_profiles(&mut self, ids: Vec<Id>) {
        const PROFILE_BATCH: usize = 128;
        if ids.is_empty() {
            return;
        }
        // Deduped before the cap so one account holding two relationship kinds (a friend and a
        // favourite, say) costs one profile read, not one of the batch's slots.
        let mut ids = ids;
        ids.sort_unstable();
        ids.dedup();
        ids.truncate(PROFILE_BATCH);
        let message = migo_protocol::ProfileRequest { user_ids: ids };
        self.request(Opcode::ProfileFetch, &message).await;
    }

    /// Reads the signed-in account's own profile card.
    ///
    /// The same `PROFILE_FETCH` the names map uses, with the session's own id as the one
    /// subject. The reply is routed by opcode like every reply here, so `on_profiles` is the
    /// one place that decides where a card lands: when a card answers for this account it is
    /// the pane's copy, and it is sent to the pane as well as to the names map, so one
    /// refresh serves both.
    async fn fetch_own_profile(&mut self) {
        let Some(signed) = self.signed.as_ref() else {
            return;
        };
        let me = signed.account.account_id;
        let message = migo_protocol::ProfileRequest { user_ids: vec![me] };
        self.request(Opcode::ProfileFetch, &message).await;
    }

    /// Reads another member's profile card for the member menu's "View profile".
    ///
    /// The ask is remembered before the fetch is fired, because the reply is a batch that
    /// names its subjects but not its asker: without the memory, the card would land in the
    /// names map and the window that asked would never hear the answer it asked for. The
    /// conversation is remembered with the account so the answering event can name the window
    /// the card belongs to.
    async fn fetch_member_profile(&mut self, conversation_id: Id, user_id: Id) {
        self.pending_member_profile = Some((conversation_id, user_id));
        self.fetch_profiles(vec![user_id]).await;
    }

    /// Reads another member's economy standing: the progression and the badges, the same two
    /// reads the wallet fires for the caller, aimed at the account the view names. Each half
    /// of the ask is remembered separately because each reply is its own frame.
    async fn fetch_member_standing(&mut self, conversation_id: Id, user_id: Id) {
        self.pending_member_progression = Some((conversation_id, user_id));
        self.pending_member_badges = Some((conversation_id, user_id));
        self.request(
            Opcode::Progression,
            &ProgressionReq {
                of_account: user_id,
            },
        )
        .await;
        self.request(
            Opcode::Badges,
            &BadgesReq {
                of_account: user_id,
            },
        )
        .await;
    }

    /// Reads the XP board's first page for another member's rank — the same board the wallet
    /// reads, one page deeper, because a rank is only a fact the community's own first page
    /// can state.
    async fn fetch_member_rank(&mut self, conversation_id: Id, user_id: Id) {
        self.pending_member_rank = Some((conversation_id, user_id));
        let board = LeaderboardReq {
            board: "xp".to_owned(),
            limit: Some(100),
        };
        self.request(Opcode::Leaderboard, &board).await;
    }

    /// Reads the caller's edge to one account: the same graph walk the friends pane makes,
    /// remembered so the reply's edge can be pulled out for the view's social line.
    async fn fetch_member_edge(&mut self, conversation_id: Id, user_id: Id) {
        self.pending_member_edge = Some((conversation_id, user_id));
        self.request_relationships().await;
    }

    /// Reads the account's entitlements — the catalogue codes the account owns — for the
    /// composer's picker. One read, one reply, no routing to remember: only the picker asks,
    /// and only one picker stands at a time. The page's own ceiling is the store's `MAX_PAGE`
    /// of two hundred, and a shelf of packs never reaches it, so the read takes the whole
    /// shelf in one page and the cursor the wire would offer is never taken.
    async fn request_entitlements(&mut self) {
        self.request(Opcode::Entitlements, &EntitlementsReq::default())
            .await;
    }

    /// Reads the gift catalogue alone — the wallet's whole-economy read without the other
    /// five requests, for surfaces that want the shop's shelves and nothing else.
    async fn request_gift_catalogue(&mut self) {
        self.request(Opcode::GiftCatalogue, &GiftCatalogueReq {})
            .await;
    }

    /// Reduces one wire card to the member view's row.
    ///
    /// The same reduction the own card takes, minus the owner's own disclosure: a birth year
    /// is the one thing an account says about itself on its own pane that another account's
    /// card never carries. The custom status, country, and language are the account's public
    /// face and ride along.
    fn member_card_from_wire(profile: &migo_protocol::UserProfile) -> crate::model::MemberCard {
        crate::model::MemberCard {
            account_id: profile.user_id,
            username: profile.username.clone(),
            display_name: profile.display_name.clone(),
            public_id: profile.public_id.clone(),
            bio: profile.bio.clone(),
            presence: profile
                .presence
                .map(|state| model::Presence::from_wire(state.to_wire()))
                .unwrap_or(model::Presence::Unknown),
            verified: profile.verified,
            custom_status: profile.custom_status.clone(),
            country: profile.country.clone(),
            language: profile.language.clone(),
        }
    }

    /// Reduces one wire card to the profile pane's row.
    fn own_profile_from_wire(profile: &migo_protocol::UserProfile) -> crate::model::OwnProfile {
        crate::model::OwnProfile {
            account_id: profile.user_id,
            username: profile.username.clone(),
            display_name: profile.display_name.clone(),
            public_id: profile.public_id.clone(),
            bio: profile.bio.clone(),
            custom_status: profile.custom_status.clone(),
            birth_year: profile.birth_year,
            presence: profile
                .presence
                .map(|state| model::Presence::from_wire(state.to_wire()))
                .unwrap_or(model::Presence::Online),
        }
    }

    /// Patches the caller's own profile and files the refreshed card with the pane.
    ///
    /// The wire's patch semantics are the form's: an absent field is "leave it", so the
    /// command carries `None` for every control the user did not touch. The custom status
    /// rides this patch too — the RICH_PRESENCE bit's own field (section 148) — and no longer
    /// rides the presence wire at all, where the server refuses it: a status is meant to
    /// outlive a disconnect, which a presence entry is not. The reply is the caller's own
    /// card read back through the same path a fetch takes, which makes it the authoritative
    /// copy — the pane replaces its profile from it rather than re-reading.
    async fn save_profile(&mut self, patch: crate::net::ProfilePatch) {
        let message = migo_protocol::ProfileUpdate {
            display_name: patch.display_name,
            bio: patch.bio,
            avatar_media_id: None,
            birth_year: patch.birth_year,
            show_last_seen: patch.show_last_seen,
            who_can_message: patch.who_can_message,
            who_can_add: patch.who_can_add,
            searchable: patch.searchable,
            custom_status: patch.custom_status,
        };
        self.request(Opcode::ProfileUpdate, &message).await;
    }

    /// Uploads a local image as the account's avatar and patches the profile to point at it.
    ///
    /// The web panel's own flow, on this client's wires: `MEDIA_UPLOAD_BEGIN` over the socket
    /// mints the ticket, the bytes PUT to the ticket's URL over plain HTTP, `MEDIA_UPLOAD_COMMIT`
    /// closes it with the real SHA-256 of what was sent — the digest is the one thing the server
    /// records about the bytes, and sending a placeholder would make this client the odd one out
    /// in every future integrity check — and then the profile patch carries the new media id.
    /// A failure anywhere aborts the ticket (best-effort; the abort's own failure never masks
    /// the real one), so the server does not hold a half-written object, and is filed beside the
    /// pane's form rather than toasted.
    ///
    /// The MIME type is claimed from the file's extension, because the client has nothing else
    /// to read it from — the server is the authority anyway: it sniffs the bytes at commit and
    /// records what it found, refusing anything that is not an image.
    async fn change_avatar(&mut self, path: PathBuf) {
        let bytes = match tokio::fs::read(&path).await {
            Ok(bytes) => bytes,
            Err(error) => {
                self.sink.send(Event::AvatarChangeFailed {
                    reason: format!("Could not read the file: {error}"),
                });
                return;
            }
        };
        if bytes.is_empty() {
            self.sink.send(Event::AvatarChangeFailed {
                reason: "The file is empty.".to_owned(),
            });
            return;
        }
        let content_type = Self::image_mime_of(&path).to_owned();

        // Begin: one ticket, over the live socket, matched by correlation the way every
        // fire-and-forget request here is. The reply arrives as a frame and is routed below.
        let begin = migo_protocol::MediaBegin {
            kind: 0, // Avatar: the wire's own numbering, zero.
            content_type,
            size: bytes.len() as u64,
            conversation_id: None,
            width: None,
            height: None,
            duration_ms: None,
        };
        if self
            .send_and_remember(Opcode::MediaUploadBegin, &begin)
            .await
            .is_none()
        {
            return;
        }
        self.avatar_pending = Some(AvatarPending { path, bytes });
    }

    /// Sends one request whose reply this worker will match by correlation when it arrives.
    ///
    /// The avatar flow needs an answer before its next step, but the worker's shape is a loop
    /// that never blocks on one frame — so the request goes out, the pending state remembers
    /// what the answer is for, and the frame arm finishes the job. Returns the correlation id
    /// the reply must carry, or `None` when the send itself failed (already reported).
    async fn send_and_remember<T: migo_protocol::Encode>(
        &mut self,
        opcode: Opcode,
        value: &T,
    ) -> Option<u32> {
        let Some(gateway) = self.gateway.as_mut() else {
            self.sink.toast("not connected", ToastKind::Error);
            return None;
        };
        let correlation = gateway.correlate();
        if let Err(error) = gateway.send(opcode, correlation, value).await {
            self.on_disconnect(error);
            return None;
        }
        Some(correlation)
    }

    /// A `MEDIA_UPLOAD_BEGIN` reply arrived while an avatar upload was pending: PUT the bytes,
    /// commit with their digest, then patch the profile.
    async fn avatar_ticket_arrived(&mut self, ticket: migo_protocol::MediaTicket) {
        let Some(pending) = self.avatar_pending.take() else {
            return;
        };
        let Some(signed) = self.signed.as_ref() else {
            return;
        };

        // PUT the bytes. This is the data plane: plain HTTP to the signed URL, no token.
        if let Err(error) = signed
            .rest
            .put_upload_bytes(&ticket.upload_url, pending.bytes.clone())
            .await
        {
            self.abort_avatar(ticket.upload_id).await;
            self.sink.send(Event::AvatarChangeFailed {
                reason: error.to_string(),
            });
            return;
        }

        // Commit with the real digest of the bytes that were sent.
        let digest = {
            use sha2::{Digest, Sha256};
            let mut hasher = Sha256::new();
            hasher.update(&pending.bytes);
            hasher.finalize().to_vec()
        };
        let commit = migo_protocol::MediaCommit {
            upload_id: ticket.upload_id,
            digest,
        };
        if self
            .send_and_remember(Opcode::MediaUploadCommit, &commit)
            .await
            .is_none()
        {
            self.abort_avatar(ticket.upload_id).await;
            return;
        }
        self.avatar_commit_pending = Some((ticket.upload_id, pending.path));
    }

    /// The `MEDIA_UPLOAD_COMMIT` acknowledgement arrived: the object exists, so point the
    /// profile at it. The commit's ack carries only `ok`, so the media id rides here in the
    /// worker's own pending state rather than in the reply.
    async fn avatar_committed(&mut self) {
        let Some((media_id, _path)) = self.avatar_commit_pending.take() else {
            return;
        };
        let message = migo_protocol::ProfileUpdate {
            display_name: None,
            bio: None,
            avatar_media_id: Some(media_id),
            birth_year: None,
            show_last_seen: None,
            who_can_message: None,
            who_can_add: None,
            searchable: None,
            // Pointing the profile at the new avatar is all this patch means to say; the
            // status column is somebody else's business.
            custom_status: None,
        };
        self.request(Opcode::ProfileUpdate, &message).await;
    }

    /// Abandons an upload ticket, best-effort, so the server does not hold bytes nobody is
    /// coming back for. A failure of the abort itself is not an error worth reporting: the
    /// ticket expires on its own and the bytes die with it.
    async fn abort_avatar(&mut self, upload_id: Id) {
        let message = migo_protocol::MediaAbort { upload_id };
        self.request(Opcode::MediaUploadAbort, &message).await;
    }

    /// Attaches a local file to a conversation: the three-step upload, then the message that
    /// references it.
    ///
    /// The worker judges the file the way the server will, from the bytes: an image that
    /// decodes is sent as an image (with its real dimensions, so receivers can lay out
    /// before downloading), anything else is a document. The size caps are checked before
    /// any bytes cross the wire, because a cap the server enforces is cheaper refused here.
    async fn send_attachment(
        &mut self,
        conversation_id: Id,
        path: PathBuf,
        expires_in_ms: Option<u32>,
    ) {
        let bytes = match tokio::fs::read(&path).await {
            Ok(bytes) => bytes,
            Err(error) => {
                self.sink.toast(
                    format!("Could not read the file: {error}"),
                    ToastKind::Error,
                );
                return;
            }
        };
        if bytes.is_empty() {
            self.sink.toast("The file is empty.", ToastKind::Error);
            return;
        }
        // The decoded image, when the bytes are one. Kept as a question (`ok()`) rather than
        // a match on the result so the document arm does not have to spell out every way a
        // decode refuses — anything that does not decode is a document, which is the honest
        // claim for it.
        let decoded = image::load_from_memory(&bytes).ok();
        // `dimensions` is a `GenericImageView` method, not an inherent one, so the trait has to
        // be in scope for the call to resolve at all — image's own docs import it in exactly
        // this one-method form.
        let (plan, plaintext) = match decoded.as_ref().map(|img| {
            use image::GenericImageView as _;
            img.dimensions()
        }) {
            Some((width, height)) => {
                if bytes.len() as u64 > media::IMAGE_MAX_BYTES {
                    self.sink.toast(
                        "That image is larger than the 16 MB limit.",
                        ToastKind::Error,
                    );
                    return;
                }
                let plan = media::OutgoingMedia {
                    kind: media::KIND_IMAGE,
                    mime_type: media::image_mime_of_bytes(&bytes, &path),
                    size_bytes: bytes.len() as u64,
                    // Filled by the begin, which is where the seal (or the room's plaintext
                    // slots) is decided.
                    key: Vec::new(),
                    nonce: Vec::new(),
                    width: Some(width),
                    height: Some(height),
                    duration_ms: None,
                    waveform: None,
                    caption: None,
                    expires_in_ms,
                };
                (plan, bytes)
            }
            None => {
                if bytes.len() as u64 > media::DOCUMENT_MAX_BYTES {
                    self.sink.toast(
                        "That file is larger than the 32 MB limit.",
                        ToastKind::Error,
                    );
                    return;
                }
                let plan = media::OutgoingMedia {
                    kind: media::KIND_DOCUMENT,
                    mime_type: media::document_mime_of(&path).to_owned(),
                    size_bytes: bytes.len() as u64,
                    key: Vec::new(),
                    nonce: Vec::new(),
                    width: None,
                    height: None,
                    duration_ms: None,
                    waveform: None,
                    caption: None,
                    expires_in_ms,
                };
                (plan, bytes)
            }
        };
        self.begin_attachment(conversation_id, plan, plaintext, None)
            .await;
    }

    /// Begins one attachment upload: decides the seal, claims what the wire's sniffer
    /// expects, and sends the BEGIN whose ticket starts the real work.
    ///
    /// What the upload *claims* and what the message later claims are deliberately different
    /// facts. An end-to-end upload claims `application/octet-stream` and the sealed length —
    /// the server must not read the type off bytes it cannot read at all, and both the claim
    /// and the digest describe the bytes actually PUT. The message claims the real MIME type
    /// and the plaintext size, because its readers are the recipients, who hold the key.
    /// A room's upload is the server-readable path: the real type, the plaintext, the
    /// zero-filled key slots — except documents, which every client seals even into rooms.
    async fn begin_attachment(
        &mut self,
        conversation_id: Id,
        mut plan: media::OutgoingMedia,
        plaintext: Vec<u8>,
        voice_note: Option<Id>,
    ) {
        let sealed = match self.signed.as_ref() {
            // Documents are always sealed. Everything else follows the conversation: an
            // end-to-end conversation seals, a room takes the plaintext path.
            Some(signed) => {
                plan.kind == media::KIND_DOCUMENT || signed.e2e.contains(&conversation_id)
            }
            // Not signed in: nothing can be sent, and a voice note's draft goes back to the
            // preview rather than being spent on a refusal.
            None => {
                self.restore_voice_note_preview(voice_note);
                return;
            }
        };
        let (key, nonce, wire_bytes, content_type, size) = if sealed {
            let domain = if plan.kind == media::KIND_VOICE_NOTE {
                media::VOICE_DOMAIN
            } else {
                media::MEDIA_DOMAIN
            };
            let sealed = match media::seal_media(&plaintext, domain, &mut OsRandom) {
                Ok(sealed) => sealed,
                Err(reason) => {
                    self.sink.toast(reason, ToastKind::Error);
                    self.restore_voice_note_preview(voice_note);
                    return;
                }
            };
            let size = sealed.sealed.len() as u64;
            (
                sealed.key,
                sealed.nonce,
                sealed.sealed,
                "application/octet-stream".to_owned(),
                size,
            )
        } else {
            let (key, nonce) = media::OutgoingMedia::plaintext_slots();
            let content_type = plan.mime_type.clone();
            let size = plan.size_bytes;
            (key, nonce, plaintext, content_type, size)
        };
        plan.key = key;
        plan.nonce = nonce;

        let message_id = Id::generate_at(Timestamp::now(), &mut OsRandom);
        let begin = migo_protocol::MediaBegin {
            kind: plan.kind,
            content_type,
            size,
            // The one field that decides the server's whole policy: with a conversation the
            // upload is conversation-scoped (and an end-to-end conversation's is never
            // sniffed or scanned), without one it is profile media.
            conversation_id: Some(conversation_id),
            width: plan.width,
            height: plan.height,
            duration_ms: plan.duration_ms,
        };
        let Some(correlation) = self
            .send_and_remember(Opcode::MediaUploadBegin, &begin)
            .await
        else {
            self.restore_voice_note_preview(voice_note);
            return;
        };
        self.attachment_begins.insert(
            correlation,
            AttachmentBegin {
                conversation_id,
                message_id,
                plan,
                wire_bytes,
                voice_note,
            },
        );
    }

    /// Hands a failed voice-note upload back to the preview, from the draft that outlived
    /// the attempt: the note is not lost, and a retry is the Send button rather than another
    /// five minutes at the microphone. An attachment that was not a voice note restores
    /// nothing.
    fn restore_voice_note_preview(&mut self, voice_note: Option<Id>) {
        let Some(conversation_id) = voice_note else {
            return;
        };
        let Some(draft) = self.drafts.load(conversation_id) else {
            return;
        };
        let Some(samples) = self.drafts.read_samples(conversation_id) else {
            self.drafts.clear(conversation_id);
            return;
        };
        if (samples.len() as u64) < u64::from(media::VOICE_NOTE_SAMPLE_RATE) / 4 {
            self.drafts.clear(conversation_id);
            return;
        }
        self.hold_note(HeldNote {
            conversation_id,
            // The descriptor's own statement of how long the note ran — the same figure the
            // preview would have shown before the upload failed, rather than a recount from
            // the bytes that could differ by the tick's last, unwritten second.
            duration_ms: draft.duration_ms,
            amplitudes: draft.amplitudes,
            // The arm the recording began under is gone with the note it rode on; the
            // restored preview sends without it rather than inventing a promise nobody made.
            expires_in_ms: None,
        });
    }

    /// A `MEDIA_UPLOAD_BEGIN` reply arrived for an attachment: PUT the bytes and commit them
    /// with the real SHA-256 of what was sent.
    ///
    /// The digest is the one thing the server records about the bytes; a placeholder would
    /// make this client the odd one out in every future integrity check. For an end-to-end
    /// upload the bytes hashed are the sealed ones — they are the bytes that were PUT.
    async fn attachment_ticket_arrived(
        &mut self,
        correlation: u32,
        ticket: migo_protocol::MediaTicket,
    ) {
        let Some(pending) = self.attachment_begins.remove(&correlation) else {
            return;
        };
        let Some(signed) = self.signed.as_ref() else {
            self.restore_voice_note_preview(pending.voice_note);
            return;
        };
        if let Err(error) = signed
            .rest
            .put_upload_bytes(&ticket.upload_url, pending.wire_bytes.clone())
            .await
        {
            self.abort_attachment(ticket.upload_id).await;
            self.sink.toast(
                format!("Could not upload the attachment: {error}"),
                ToastKind::Error,
            );
            self.restore_voice_note_preview(pending.voice_note);
            return;
        }
        let digest = {
            use sha2::{Digest, Sha256};
            let mut hasher = Sha256::new();
            hasher.update(&pending.wire_bytes);
            hasher.finalize().to_vec()
        };
        let commit = migo_protocol::MediaCommit {
            upload_id: ticket.upload_id,
            digest,
        };
        let Some(correlation) = self
            .send_and_remember(Opcode::MediaUploadCommit, &commit)
            .await
        else {
            self.abort_attachment(ticket.upload_id).await;
            self.restore_voice_note_preview(pending.voice_note);
            return;
        };
        self.attachment_commits.insert(
            correlation,
            AttachmentCommit {
                conversation_id: pending.conversation_id,
                message_id: pending.message_id,
                media_id: ticket.upload_id,
                plan: pending.plan,
                voice_note: pending.voice_note,
            },
        );
    }

    /// A `MEDIA_UPLOAD_COMMIT` acknowledgement arrived: the object exists, so the message
    /// that references it can go out — the optimistic row first, the same shape a text send
    /// takes, then the sealed content through the shared send path.
    ///
    /// The row appears here rather than at the click because the media id is minted by the
    /// ticket, and the content can only be built once it exists — the same ordering the web
    /// client's sendAttachment takes (upload, then send).
    async fn attachment_committed(&mut self, correlation: u32) {
        let Some(pending) = self.attachment_commits.remove(&correlation) else {
            return;
        };
        // The object exists, so a voice note's draft has finished its journey: the bytes are
        // the server's ciphertext now, and the draft's own door closes behind them. Every
        // earlier failure on the way left it open on purpose.
        if let Some(conversation_id) = pending.voice_note {
            self.drafts.clear(conversation_id);
        }
        // File the keying now, from the plan that minted it: a fetch of our own object —
        // from this device after an eviction, or another device entirely — opens with the
        // same slots the message carries.
        if let Some(signed) = self.signed.as_mut() {
            signed
                .media_keys
                .entry(pending.media_id)
                .or_insert_with(|| MediaKeying {
                    key: pending.plan.key.clone(),
                    nonce: pending.plan.nonce.clone(),
                    mime_type: pending.plan.mime_type.clone(),
                    voice: pending.plan.kind == media::KIND_VOICE_NOTE,
                });
        }
        let sender_id = self.signed.as_ref().map(|signed| signed.account.account_id);
        let Some(sender_id) = sender_id else {
            return;
        };
        self.sink.send(Event::Message(Message {
            message_id: pending.message_id,
            conversation_id: pending.conversation_id,
            seq: 0,
            sender_id,
            outgoing: true,
            body: pending.plan.body(pending.media_id),
            sent_at: Timestamp::now(),
            delivery: Delivery::Sending,
            deleted: false,
            edited: false,
            expires_at: pending
                .plan
                .expires_in_ms
                .map(|lifetime| Timestamp::now().saturating_add_millis(i64::from(lifetime))),
        }));
        let plaintext = match content::encode(&pending.plan.content(pending.media_id), true) {
            Ok(bytes) => bytes,
            Err(_) => {
                return self.sink.send(Event::SendFailed {
                    message_id: pending.message_id,
                })
            }
        };
        let Some((envelope, chain_id)) =
            self.envelope_for(pending.conversation_id, &plaintext).await
        else {
            return self.sink.send(Event::SendFailed {
                message_id: pending.message_id,
            });
        };
        // The coarse kind travels in the clear and the server routes and counts by it, so it
        // must say what the envelope actually carries: Media for an image or document,
        // Voice for a voice note — the same mapping the web SDK's `kindForContent` makes
        // for MediaRef and VoiceNoteRef. Text would be a lie the counters keep.
        let kind = if pending.plan.kind == media::KIND_VOICE_NOTE {
            MessageKind::Voice
        } else {
            MessageKind::Media
        };
        let message = migo_protocol::MessageSend {
            message_id: pending.message_id,
            conversation_id: pending.conversation_id,
            kind,
            envelope,
            reply_to: None,
            // The wire copy of the sealed lifetime, so the server's own sweeper agrees with
            // every receiver's countdown. `plan.content` sealed the same value inside the
            // envelope one call above.
            expires_in_ms: pending.plan.expires_in_ms,
            sender_key_id: Some(chain_id),
        };
        self.request(Opcode::MessageSend, &message).await;
    }

    /// Abandons an attachment's upload ticket, the same best-effort the avatar's takes.
    async fn abort_attachment(&mut self, upload_id: Id) {
        let message = migo_protocol::MediaAbort { upload_id };
        self.request(Opcode::MediaUploadAbort, &message).await;
    }

    /// Reacts to one message with one emoji, over the same seal as any content.
    ///
    /// `REACTION_SET` is fire-and-forget here for the same reason the web client's is: the
    /// server mints a deterministic message id from the envelope, so a retry is a duplicate
    /// suppressed server-side and a lost reply is a reaction that still landed. Our own echo
    /// comes back suppressed (this device's messages never re-render), so the UI adds its
    /// own chip on the click — nothing here has an event to send.
    async fn send_reaction(&mut self, conversation_id: Id, target_message_id: Id, emoji: String) {
        let content = Content::Reaction {
            target_message_id,
            emoji,
            // Add-only, the same shape every Migo client sends: no client offers a retract,
            // so the flag exists on the wire but not in anyone's hands.
            remove: false,
        };
        let plaintext = match content::encode(&content, true) {
            Ok(bytes) => bytes,
            Err(_) => return,
        };
        let Some((envelope, _chain_id)) = self.envelope_for(conversation_id, &plaintext).await
        else {
            return;
        };
        let reaction = migo_protocol::ReactionSet {
            target_message_id,
            conversation_id,
            envelope,
        };
        self.request(Opcode::ReactionSet, &reaction).await;
    }

    /// Withdraws one of this account's own messages for everyone. The wire is delete-for-
    /// everyone or nothing — the server refuses `for_everyone: false` outright, because it
    /// keeps no per-member hide table — so the flag is sent `true` and never anything else.
    ///
    /// The tombstone comes back as an ordinary message event (a `MessageEvent` with `deleted`
    /// set and the envelope cleared), which the chat layer already knows how to fold; this
    /// send needs no echo of its own, the same posture the reaction send takes.
    async fn delete_message(&mut self, conversation_id: Id, message_id: Id) {
        let message = migo_protocol::MessageDelete {
            message_id,
            conversation_id,
            for_everyone: true,
        };
        self.request(Opcode::MessageDelete, &message).await;
    }

    /// Replaces one of this account's own text messages. The replacement is sealed through
    /// the same conversation chain that sealed the original — an edit is a send that happens
    /// to land on an old sequence — so the audience and the distributions are the same
    /// `envelope_for` machinery every send path uses.
    ///
    /// On acceptance the server fans the edited message out as a `MessageEvent` with an
    /// `edited_at` stamp, and this device's own echo is suppressed by the decrypt layer's
    /// `mine` rule. The local row moves when that event arrives, not before: the sender's
    /// screen says "edited" when the server says so, the same moment everyone else's does.
    async fn edit_message(&mut self, conversation_id: Id, message_id: Id, text: String) {
        // The replacement carries no lifetime of its own, exactly like the web's
        // `sealTextEdit`: an edit is a correction of what was said, not a new send, so the
        // row keeps the deadline the original sealed. A receiver that re-reads the edit's
        // content finds no `expiresInMs` and falls back to the row it already holds, whose
        // deadline the fold never withdraws.
        let plaintext = match content::encode(&Content::text(text), true) {
            Ok(bytes) => bytes,
            Err(_) => {
                self.sink
                    .toast("The edit could not be sealed", ToastKind::Error);
                return;
            }
        };
        let Some((envelope, _chain_id)) = self.envelope_for(conversation_id, &plaintext).await
        else {
            return;
        };
        let edit = migo_protocol::MessageEdit {
            message_id,
            conversation_id,
            envelope,
        };
        self.request(Opcode::MessageEdit, &edit).await;
    }

    /// Blocks an account. The server tears down the friendship and every pending edge in
    /// both directions, tells the blocked party nothing, and the graph re-read that follows
    /// the acknowledgement is what moves the friends pane — the block itself answers with
    /// nothing to show.
    async fn block_user(&mut self, user_id: Id) {
        let message = FriendTarget { user_id };
        self.request(Opcode::BlockSet, &message).await;
    }

    /// Sets or clears the caller's personal mute on an account. The wire carries the switch;
    /// the graph re-read after the acknowledgement is what carries it back.
    async fn mute_user(&mut self, user_id: Id, on: bool) {
        let message = migo_protocol::MuteSet { user_id, on };
        self.request(Opcode::MuteSet, &message).await;
    }

    /// Begins a voice-note recording: opens the microphone and hands its chunks to a pump
    /// that appends them to the draft file at the note's own rate, counting samples and
    /// folding the live waveform as it goes.
    fn start_recording(&mut self, conversation_id: Id, expires_in_ms: Option<u32>) {
        if self.recording.is_some() {
            return;
        }
        // A call owns the microphone while it runs, and section 179 answers a call with a
        // pause rather than a fight over the device: the note waits for the call to end.
        if self.calls.busy() {
            self.sink.toast(
                "A call is using the microphone. The note can be recorded once it ends.",
                ToastKind::Info,
            );
            return;
        }
        // `mut` because `take_frames` hands the receiver out of the microphone by &mut self —
        // the handle keeps working afterwards (mute and stop go through it), but the one-time
        // handover is still a mutation of the source.
        let mut microphone = match call_audio::open_microphone() {
            Ok(microphone) => microphone,
            Err(error) => {
                self.sink.toast(
                    format!("Could not open the microphone: {error}"),
                    ToastKind::Error,
                );
                return;
            }
        };
        // The draft's file is created before the pump exists, so the recording is on disk from
        // its first chunk — an app death a second in leaves a draft, not nothing. Creating it
        // also truncates whatever draft this conversation held before, the one-draft-per-
        // conversation rule's own mechanics.
        let out = match self.drafts.create_pcm(conversation_id) {
            Ok(file) => std::io::BufWriter::new(file),
            Err(error) => {
                self.sink.toast(
                    format!("Could not open the recording's draft file: {error}"),
                    ToastKind::Error,
                );
                return;
            }
        };
        let rate = microphone.rate;
        let frames = microphone.take_frames();
        let shared = Arc::new(RecordShared {
            samples: AtomicU64::new(0),
            amplitudes: Mutex::new(Vec::new()),
            stop: AtomicBool::new(false),
            paused: AtomicBool::new(false),
        });
        let pump = spawn_recording_pump(frames, rate, Arc::clone(&shared), out);
        // A new recording is a new note: whatever the composer was holding — a preview, an
        // undo window's cancelled note — stands down, the same handoff the Android composer
        // makes. The bytes stay in the store, where this conversation's own file is the one
        // just truncated and any other conversation's draft keeps waiting for its own window.
        self.held_note = None;
        self.note_undo_until = None;
        self.recording = Some(Recording {
            conversation_id,
            microphone,
            shared,
            pump: Some(pump),
            expires_in_ms,
            paused_by_interruption: false,
            persisted_ms: 0,
        });
        self.sink.send(Event::RecordingStarted { conversation_id });
    }

    /// Ends the capture and holds the finished note for the composer's word: the two-step
    /// mode's Stop. The bytes stay in the draft store, so the preview survives a death the
    /// same way a mid-recording draft does.
    fn stop_recording(&mut self) {
        if let Some(note) = self.finalize_recording() {
            self.hold_note(note);
        }
    }

    /// Sends the note: the preview's Send, or a hold-mode release while the recorder is still
    /// live, which finalises and sends in one motion — the whole journey for a recording
    /// stopped by the cap or released to send.
    async fn send_voice_note(&mut self) {
        if self.recording.is_some() {
            let note = self.finalize_recording();
            if let Some(note) = note {
                self.upload_held_note(note).await;
            }
            return;
        }
        let Some(note) = self.held_note.take() else {
            return;
        };
        self.note_undo_until = None;
        self.upload_held_note(note).await;
    }

    /// Cancels — the bar's Cancel, the preview's Delete, or a hold-mode slide away. Nothing
    /// is deleted outright: the note becomes the undo window's draft, because a slide nobody
    /// meant is the one mistake the hold mode makes easy.
    fn cancel_voice_note(&mut self) {
        if self.recording.is_some() {
            let note = self.finalize_recording();
            if let Some(note) = note {
                self.held_note = Some(note);
            }
        }
        let Some(note) = self.held_note.as_ref() else {
            return;
        };
        let conversation_id = note.conversation_id;
        self.note_undo_until = Some(std::time::Instant::now() + NOTE_UNDO_WINDOW);
        self.sink.send(Event::NoteDiscarded {
            conversation_id,
            undoable: true,
        });
    }

    /// Restores a cancelled note from its undo window, back into the preview it was stopped
    /// at. The bytes never moved: only the window's deadline does.
    fn undo_note_discard(&mut self) {
        if self.held_note.is_some() {
            self.note_undo_until = None;
            self.emit_note_preview();
        }
    }

    /// The undo window closed on its own: the note is gone for real now, bytes and all, and
    /// the chip that offered the restore is withdrawn.
    fn note_undo_expired(&mut self) {
        self.note_undo_until = None;
        if let Some(note) = self.held_note.take() {
            self.drafts.clear(note.conversation_id);
            self.sink.send(Event::NoteDiscarded {
                conversation_id: note.conversation_id,
                undoable: false,
            });
        }
    }

    /// Holds a finished note as the composer's preview.
    fn hold_note(&mut self, note: HeldNote) {
        self.note_undo_until = None;
        self.held_note = Some(note);
        self.emit_note_preview();
    }

    /// States the held note to the composer: the duration and the folded waveform, so the
    /// preview lays out before any byte is read back.
    fn emit_note_preview(&self) {
        if let Some(note) = self.held_note.as_ref() {
            self.sink.send(Event::RecordingPreview {
                conversation_id: note.conversation_id,
                duration_ms: u32::try_from(note.duration_ms).unwrap_or(0),
                waveform: media::downsample_waveform(&note.amplitudes),
            });
        }
    }

    /// Finds a draft the last session left behind — the app-death rule of section 179 — and
    /// offers it as the preview the composer would show a note it had just stopped. A
    /// recording this session already holds recovers nothing, and a draft whose bytes cannot
    /// be read or hold less than a quarter-second of speech is a draft nobody can hear,
    /// cleared quietly rather than offered as speech it is not. A recovered note makes no
    /// disappearing promise: the arm belonged to the recording that died, not this one.
    fn recover_voice_draft(&mut self, conversation_id: Id) {
        if self.recording.is_some() || self.held_note.is_some() {
            return;
        }
        let Some(draft) = self.drafts.load(conversation_id) else {
            return;
        };
        let Some(samples) = self.drafts.read_samples(conversation_id) else {
            self.drafts.clear(conversation_id);
            return;
        };
        if (samples.len() as u64) < u64::from(media::VOICE_NOTE_SAMPLE_RATE) / 4 {
            self.drafts.clear(conversation_id);
            return;
        }
        self.hold_note(HeldNote {
            conversation_id,
            // The descriptor's own statement of how long the recording ran, which is the
            // figure the bar was ticking when the app died — the bytes can hold a fraction
            // of a second more, and the send settles the final figure from them.
            duration_ms: draft.duration_ms,
            amplitudes: draft.amplitudes,
            expires_in_ms: None,
        });
    }

    /// Ends the live capture and collects the finished note, or `None` when there was
    /// nothing live or the note never became speech. The draft file is the recording's own
    /// memory, so finalising is a join of the pump, a last descriptor write, and the
    /// judgement on what was said.
    fn finalize_recording(&mut self) -> Option<HeldNote> {
        let mut recording = self.recording.take()?;
        let conversation_id = recording.conversation_id;
        let expires_in_ms = recording.expires_in_ms;
        recording.shared.stop.store(true, Ordering::Relaxed);
        // Dropping the microphone closes the pump's channel, and joining the pump waits for
        // its last write to hit the file — the read-back below must not race a buffered tail.
        drop(recording.microphone);
        if let Some(pump) = recording.pump.take() {
            let _ = pump.join();
        }
        let count = recording.shared.samples.load(Ordering::Relaxed);
        let amplitudes = recording
            .shared
            .amplitudes
            .lock()
            .map(|bars| bars.clone())
            .unwrap_or_default();
        self.sink.send(Event::RecordingStopped { conversation_id });
        // A note shorter than a quarter-second is a click, not a message: refuse it as one,
        // on every path a recording can end by.
        if count < u64::from(media::VOICE_NOTE_SAMPLE_RATE) / 4 {
            self.drafts.clear(conversation_id);
            self.sink.toast(
                "Hold the microphone a moment longer to record a note.",
                ToastKind::Info,
            );
            return None;
        }
        let duration_ms = count * 1_000 / u64::from(media::VOICE_NOTE_SAMPLE_RATE);
        // The final descriptor, so a death between here and the send still finds a draft
        // that states the note's whole length rather than the last tick's.
        self.drafts.save(conversation_id, duration_ms, &amplitudes);
        Some(HeldNote {
            conversation_id,
            duration_ms,
            amplitudes,
            expires_in_ms,
        })
    }

    /// The recording's own tick: state the live note to the composer, persist the draft
    /// descriptor once a second, and end the note at the cap — a send, not a stop, because a
    /// recording that ran to its ceiling finishes its journey rather than waiting on nobody
    /// to press Send.
    async fn recording_ticked(&mut self) {
        // The cap, read from the sample count: a pause stood it still, and the pump's own
        // backstop stopped appending at the same place, so the tick's job is only to notice.
        let max_samples =
            media::VOICE_NOTE_MAX_MS * u64::from(media::VOICE_NOTE_SAMPLE_RATE) / 1_000;
        let (conversation_id, elapsed_ms, paused, count) = {
            let Some(recording) = self.recording.as_mut() else {
                return;
            };
            let count = recording.shared.samples.load(Ordering::Relaxed);
            let elapsed_ms = count * 1_000 / u64::from(media::VOICE_NOTE_SAMPLE_RATE);
            // The descriptor rides the recording's own once-a-second cadence — a death
            // between ticks costs at most a second of the timer's precision.
            if elapsed_ms.saturating_sub(recording.persisted_ms) >= 1_000 {
                recording.persisted_ms = elapsed_ms;
                let amplitudes = recording
                    .shared
                    .amplitudes
                    .lock()
                    .map(|bars| bars.clone())
                    .unwrap_or_default();
                self.drafts
                    .save(recording.conversation_id, elapsed_ms, &amplitudes);
            }
            (
                recording.conversation_id,
                elapsed_ms,
                recording.shared.paused.load(Ordering::Relaxed),
                count,
            )
        };
        // The event carries only the newest bars — the bar draws the last of them, and the
        // descriptor above is where the whole stream is kept.
        let amplitudes = self
            .recording
            .as_ref()
            .and_then(|live| live.shared.amplitudes.lock().ok())
            .map(|bars| {
                let kept = bars.len().saturating_sub(media::WAVEFORM_BARS);
                bars[kept..].to_vec()
            })
            .unwrap_or_default();
        self.sink.send(Event::RecordingProgress {
            conversation_id,
            elapsed_ms,
            paused,
            amplitudes,
        });
        if count >= max_samples {
            let note = self.finalize_recording();
            if let Some(note) = note {
                self.upload_held_note(note).await;
            }
        }
    }

    /// Pauses the live capture. `by_interruption` marks a pause the worker lifts itself when
    /// the interruption passes — a call arriving, a call being placed — while a pause the
    /// speaker chose is theirs to lift.
    fn pause_recording(&mut self, by_interruption: bool) {
        let Some(recording) = self.recording.as_mut() else {
            return;
        };
        if recording.shared.paused.load(Ordering::Relaxed) {
            return;
        }
        recording.shared.paused.store(true, Ordering::Relaxed);
        recording.paused_by_interruption = by_interruption;
        self.emit_recording_progress();
    }

    /// Resumes a paused capture; an interruption's resume only lifts an interruption's pause.
    fn resume_recording(&mut self, after_interruption: bool) {
        let Some(recording) = self.recording.as_mut() else {
            return;
        };
        if after_interruption && !recording.paused_by_interruption {
            return;
        }
        if !recording.shared.paused.load(Ordering::Relaxed) {
            return;
        }
        recording.shared.paused.store(false, Ordering::Relaxed);
        recording.paused_by_interruption = false;
        self.emit_recording_progress();
    }

    /// States the live recording to the composer at once — the pause and resume paths' own
    /// event, so the bar's glyph flips on the press and not on the next tick.
    fn emit_recording_progress(&self) {
        if let Some(recording) = self.recording.as_ref() {
            let count = recording.shared.samples.load(Ordering::Relaxed);
            self.sink.send(Event::RecordingProgress {
                conversation_id: recording.conversation_id,
                elapsed_ms: count * 1_000 / u64::from(media::VOICE_NOTE_SAMPLE_RATE),
                paused: recording.shared.paused.load(Ordering::Relaxed),
                amplitudes: recording
                    .shared
                    .amplitudes
                    .lock()
                    .map(|bars| bars.clone())
                    .unwrap_or_default(),
            });
        }
    }

    /// Uploads and sends a held note. The note is read back from the draft store — the same
    /// bytes the pump wrote — and the draft survives every failure on the way: the commit's
    /// success is the one door it leaves by, so a dropped request costs a retry and not five
    /// minutes of speech.
    async fn upload_held_note(&mut self, note: HeldNote) {
        let conversation_id = note.conversation_id;
        let Some(samples) = self.drafts.read_samples(conversation_id) else {
            self.sink.toast(
                "The recording's draft could not be read back.",
                ToastKind::Error,
            );
            self.drafts.clear(conversation_id);
            return;
        };
        // The cap, enforced on the samples the pump actually wrote. The tick ends the
        // recording at the same cap, so this truncation is the backstop for a pump that
        // appended past it in its last chunk.
        let max_samples =
            media::VOICE_NOTE_MAX_MS * u64::from(media::VOICE_NOTE_SAMPLE_RATE) / 1_000;
        let mut samples = samples;
        samples.truncate(max_samples as usize);
        let duration_ms = samples.len() as u64 * 1_000 / u64::from(media::VOICE_NOTE_SAMPLE_RATE);
        let wav = media::wav_bytes(&samples, media::VOICE_NOTE_SAMPLE_RATE);
        if wav.len() as u64 > media::VOICE_NOTE_MAX_BYTES {
            // Cannot happen at this rate and cap (a capped recording is 4.8 MB of the 8 MB
            // budget), but the byte cap is the policy and the policy is checked, not assumed.
            self.sink
                .toast("That recording is too long to send.", ToastKind::Error);
            self.drafts.clear(conversation_id);
            return;
        }
        let plan = media::OutgoingMedia {
            kind: media::KIND_VOICE_NOTE,
            // Plain WAV: PCM 16-bit, mono, at the note's own rate — the one container every
            // client plays and the server's sniffer reads from the RIFF magic alone.
            mime_type: "audio/wav".to_owned(),
            size_bytes: wav.len() as u64,
            key: Vec::new(),
            nonce: Vec::new(),
            width: None,
            height: None,
            duration_ms: Some(duration_ms),
            // The waveform the pump sampled, folded to the fixed width every client renders —
            // computed here, before the seal, the only moment the plaintext exists.
            waveform: (!note.amplitudes.is_empty())
                .then(|| media::downsample_waveform(&note.amplitudes)),
            caption: None,
            // The lifetime the composer was armed with when recording began — a disappearing
            // arm covers the voice note too, the same rule every body follows.
            expires_in_ms: note.expires_in_ms,
        };
        self.begin_attachment(conversation_id, plan, wav, Some(conversation_id))
            .await;
    }

    /// Plays one voice note, or stops it if it is the one already playing. Any other note
    /// playing is stopped first — one speaker, one note.
    async fn play_voice_note(&mut self, media_id: Id) {
        if let Some(playing) = self.playing.as_ref() {
            if playing.media_id == media_id {
                self.stop_voice_note();
                return;
            }
            self.stop_voice_note();
        }
        if let Some(media::CachedMedia::Audio { samples, rate }) = self.media_cache.get(&media_id) {
            let samples = Arc::clone(samples);
            let rate = *rate;
            self.start_playback(media_id, samples, rate);
            return;
        }
        // Not cached: fetch it, and play when it opens. The want carries the intent so the
        // fetch's completion knows what the bytes were fetched for.
        self.want_media(media_id, MediaIntent::Play).await;
    }

    /// Starts one note playing on a fresh speaker, with a paced pump feeding it.
    fn start_playback(&mut self, media_id: Id, samples: Arc<Vec<i16>>, rate: u32) {
        let speaker = match call_audio::open_speaker() {
            Ok(speaker) => speaker,
            Err(error) => {
                self.sink.toast(
                    format!("Could not open the speaker: {error}"),
                    ToastKind::Error,
                );
                return;
            }
        };
        let stop = Arc::new(AtomicBool::new(false));
        // The pace the note starts at is the pace the player last chose, so a saved 2x
        // survives a restart without a press of the speed control.
        let speed = Arc::new(AtomicU32::new(self.voice_speed.percent()));
        spawn_playback_pump(PlaybackPump {
            frames: speaker.frames.clone(),
            sink_rate: speaker.rate,
            samples,
            rate,
            stop: Arc::clone(&stop),
            speed: Arc::clone(&speed),
            commands: self.commands.clone(),
            media_id,
        });
        self.playing = Some(Playing {
            media_id,
            speaker,
            stop,
            speed,
        });
        self.sink.send(Event::VoicePlaying { media_id });
    }

    /// Stops the note playing, if one is. The button, a second note starting, sign-out, and
    /// the loop's own teardown all arrive here.
    fn stop_voice_note(&mut self) {
        let Some(playing) = self.playing.take() else {
            return;
        };
        let media_id = playing.media_id;
        playing.stop.store(true, Ordering::Relaxed);
        drop(playing);
        self.sink.send(Event::VoiceStopped { media_id });
    }

    /// A playback pump ran out of samples: the note finished on its own. The pump that
    /// reports is identified, because a stopped pump can outlive its stop by a chunk and a
    /// late report must not end the note that replaced it.
    fn voice_note_ended(&mut self, media_id: Id) {
        let is_current = self
            .playing
            .as_ref()
            .is_some_and(|playing| playing.media_id == media_id);
        if !is_current {
            return;
        }
        self.stop_voice_note();
    }

    /// Sets the playback speed (§179): the pump feeding the speaker now is told through its
    /// shared flag, so the note playing keeps its position and only changes pace — the
    /// client-side rule is that the media is never asked for again, and the pump already
    /// holds every sample. The choice is also the pace the next note starts at.
    fn set_voice_speed(&mut self, speed: VoiceSpeed) {
        self.voice_speed = speed;
        if let Some(playing) = self.playing.as_ref() {
            playing.speed.store(speed.percent(), Ordering::Relaxed);
        }
    }

    /// Marks one voice note listened or unlistened by hand, and persists the set. §179's
    /// receiver-local rule: nothing is sent — a mark is the receiver's own memory of what it
    /// has heard, and marking unlistened does not unsay a receipt that already went either.
    fn set_voice_listened(&mut self, media_id: Id, listened: bool) {
        let changed = if listened {
            self.listened.insert(media_id)
        } else {
            self.listened.remove(&media_id)
        };
        if changed {
            self.save_listened();
        }
    }

    /// A playback pump crossed the listened threshold: the note has been heard to (near) its
    /// end, so the receiver's own mark goes on without a press. Reported by the thread the
    /// same way an ending is, and filed the same way a hand mark is.
    fn voice_note_heard(&mut self, media_id: Id) {
        if self.listened.insert(media_id) {
            self.save_listened();
            self.sink.send(Event::VoiceNoteListened { media_id });
        }
    }

    /// Persists the account's listened marks, best-effort: the store's own rule is that a
    /// failed write costs one restart's worth of memory, never the click that caused it.
    fn save_listened(&self) {
        if let Some(signed) = self.signed.as_ref() {
            self.voice_listened
                .save(signed.account.account_id, &self.listened);
        }
    }

    /// Wants one attachment: serves it from the cache when this session already has it, and
    /// fetches it otherwise. Asking twice while a fetch is in flight costs nothing — the
    /// second ask is folded into the first by [`Self::media_fetching`].
    async fn want_media(&mut self, media_id: Id, intent: MediaIntent) {
        if self.media_cache.get(&media_id).is_some() {
            // Already opened this session: serve it without a round trip. A kind and an
            // intent that do not match (a play asked of a document) are serve_media's own
            // nothing, not something to catch here.
            self.serve_media(media_id, intent);
            return;
        }
        if !self.media_fetching.insert(media_id) {
            return;
        }
        let has_key = self
            .signed
            .as_ref()
            .is_some_and(|signed| signed.media_keys.contains_key(&media_id));
        if !has_key {
            self.media_fetching.remove(&media_id);
            self.sink.send(Event::MediaFailed {
                media_id,
                reason: "This attachment's message is not in this session, so its key is \
                         not held. Reopen the conversation's history and try again."
                    .to_owned(),
            });
            return;
        }
        let fetch = migo_protocol::MediaFetch {
            object_id: media_id,
            // The server ignores this field; the object id is the whole address. None, so a
            // future server that reads it is not told a wrong conversation.
            conversation_id: None,
        };
        let Some(correlation) = self.send_and_remember(Opcode::MediaFetchUrl, &fetch).await else {
            self.media_fetching.remove(&media_id);
            return;
        };
        self.media_wants
            .insert(correlation, MediaWant { media_id, intent });
    }

    /// A `MEDIA_FETCH_URL` reply arrived: download, open, decode, cache, and serve.
    ///
    /// Every failure collapses into a [`Event::MediaFailed`] against the media id — the
    /// fetch, the seal, the decode — because the bubble is where the sentence belongs, and
    /// each of them leaves the attachment unfetchable for the same reason the retry exists:
    /// the press of the button again.
    async fn media_url_arrived(&mut self, correlation: u32, url: migo_protocol::MediaUrl) {
        let Some(want) = self.media_wants.remove(&correlation) else {
            return;
        };
        let media_id = want.media_id;
        let failed = |worker: &mut Self, reason: String| {
            worker.media_fetching.remove(&media_id);
            worker.sink.send(Event::MediaFailed { media_id, reason });
        };
        let Some(signed) = self.signed.as_ref() else {
            return;
        };
        let bytes = match signed.rest.get_download_bytes(&url.url).await {
            Ok(bytes) => bytes,
            Err(error) => {
                failed(self, format!("Could not fetch the attachment: {error}"));
                return;
            }
        };
        let Some(keying) = signed.media_keys.get(&media_id).cloned() else {
            failed(
                self,
                "The key for this attachment is no longer held.".to_owned(),
            );
            return;
        };
        let opened = if media::is_legacy_plaintext(&keying.key) {
            // A room's plaintext upload: the bytes are the attachment, nothing to open.
            Ok(bytes)
        } else {
            let domain = if keying.voice {
                media::VOICE_DOMAIN
            } else {
                media::MEDIA_DOMAIN
            };
            media::open_media(&keying.key, &keying.nonce, domain, &bytes)
        };
        let opened = match opened {
            Ok(opened) => opened,
            Err(reason) => {
                failed(self, reason.to_owned());
                return;
            }
        };
        let cached = if keying.voice {
            match media::decode_audio(&opened) {
                Ok(decoded) => media::CachedMedia::Audio {
                    samples: Arc::new(decoded.samples),
                    rate: decoded.rate,
                },
                Err(reason) => {
                    failed(self, reason);
                    return;
                }
            }
        } else if keying.mime_type.starts_with("image/") {
            // Cached as the sender's original bytes; the pixels are decoded per serve, so a
            // save writes the file that was sent.
            media::CachedMedia::Image { bytes: opened }
        } else {
            media::CachedMedia::Document { bytes: opened }
        };
        self.media_cache.insert(media_id, cached);
        self.media_fetching.remove(&media_id);
        self.serve_media(media_id, want.intent);
    }

    /// Serves one cached attachment for one intent: pixels for a show, the speaker for a
    /// play, the original bytes for a save.
    fn serve_media(&mut self, media_id: Id, intent: MediaIntent) {
        let Some(cached) = self.media_cache.get(&media_id) else {
            return;
        };
        match (cached, intent) {
            (media::CachedMedia::Image { bytes, .. }, MediaIntent::Show) => {
                match media::decode_image(bytes) {
                    Ok((width, height, rgba)) => self.sink.send(Event::MediaImage {
                        media_id,
                        width,
                        height,
                        rgba,
                    }),
                    Err(reason) => self.sink.send(Event::MediaFailed { media_id, reason }),
                }
            }
            (media::CachedMedia::Audio { samples, rate }, MediaIntent::Play) => {
                let samples = Arc::clone(samples);
                let rate = *rate;
                self.start_playback(media_id, samples, rate);
            }
            (cached, MediaIntent::SaveTo(path)) => match cached.bytes() {
                Some(bytes) => match std::fs::write(&path, bytes) {
                    Ok(()) => self
                        .sink
                        .toast(format!("Saved to {}", path.display()), ToastKind::Success),
                    Err(error) => self
                        .sink
                        .toast(format!("Could not save: {error}"), ToastKind::Error),
                },
                None => self.sink.toast(
                    "A voice note has no file to save — it plays here.".to_owned(),
                    ToastKind::Info,
                ),
            },
            // A kind and an intent that do not match (a play asked of a document, a show of
            // a note) is nothing: the UI only offers the affordance the body type names.
            _ => {}
        }
    }

    /// Files what one decrypted content said about the media object it references.
    ///
    /// Called on every path that sees content — live, history, held-and-drained — and after
    /// our own commits, because the keys travel only inside the message: the one message a
    /// session misses is the one whose attachment can never be fetched again. An
    /// `or_insert_with`, not an insert, so a peer's own echo of a reference we sent does not
    /// overwrite the slots this device minted.
    fn file_media_keys(&mut self, content: &Content) {
        let Some(signed) = self.signed.as_mut() else {
            return;
        };
        match content {
            Content::MediaRef {
                media_id,
                mime_type,
                key,
                nonce,
                ..
            } => {
                signed
                    .media_keys
                    .entry(*media_id)
                    .or_insert_with(|| MediaKeying {
                        key: key.clone(),
                        nonce: nonce.clone(),
                        mime_type: mime_type.clone(),
                        voice: false,
                    });
            }
            Content::VoiceNoteRef {
                media_id,
                mime_type,
                key,
                nonce,
                ..
            } => {
                signed
                    .media_keys
                    .entry(*media_id)
                    .or_insert_with(|| MediaKeying {
                        key: key.clone(),
                        nonce: nonce.clone(),
                        mime_type: mime_type.clone(),
                        voice: true,
                    });
            }
            _ => {}
        }
    }

    /// The MIME type to claim for an avatar file, from its extension.
    ///
    /// `image/*` for the formats the server's sniffer recognises as images, and a neutral claim
    /// for anything else — the server re-judges from the bytes at commit, so the claim only has
    /// to be honest, never right.
    fn image_mime_of(path: &std::path::Path) -> &'static str {
        match path
            .extension()
            .and_then(|extension| extension.to_str())
            .map(|extension| extension.to_ascii_lowercase())
            .as_deref()
        {
            Some("png") => "image/png",
            Some("jpg") | Some("jpeg") => "image/jpeg",
            Some("webp") => "image/webp",
            Some("gif") => "image/gif",
            Some("avif") => "image/avif",
            _ => "application/octet-stream",
        }
    }

    /// Reads the admin surface's own gate, then the list it guards — one answer either way.
    ///
    /// The standing is asked first and decides whether the list is even requested: a
    /// non-owner's list read is not an error to catch, it is a read this client never makes.
    async fn fetch_admins(&mut self) {
        let Some(signed) = self.signed.as_ref() else {
            return;
        };
        let standing = signed.rest.admin_standing(&signed.access_token).await;
        let answer = match standing {
            Err(error) => AdminsAnswer::Failed(error.to_string()),
            Ok(standing) if !standing.owner => AdminsAnswer::Closed,
            Ok(_) => match signed.rest.global_admins(&signed.access_token).await {
                Ok(list) => AdminsAnswer::Owner(
                    list.into_iter()
                        .map(|admin| crate::model::AdminRow {
                            account_id: admin.account_id,
                            username: admin.username,
                            granted_at: Timestamp::from_unix_ms(admin.granted_at_ms),
                        })
                        .collect::<Vec<_>>(),
                ),
                Err(error) => AdminsAnswer::Failed(error.to_string()),
            },
        };
        self.sink.send(Event::Admins(answer));
    }

    /// Appoints a global admin, then re-reads the list so the pane shows the server's truth
    /// rather than its own echo of the request.
    async fn grant_admin(&mut self, username: String) {
        let outcome = match self.signed.as_ref() {
            Some(signed) => signed
                .rest
                .grant_global_admin(&signed.access_token, &username)
                .await
                .map(|view| view.username),
            None => return,
        };
        match outcome {
            Ok(name) => {
                self.sink
                    .toast(format!("{name} is now a global admin."), ToastKind::Success);
                self.fetch_admins().await;
            }
            Err(error) => {
                self.sink.send(Event::AdminChangeFailed {
                    reason: error.to_string(),
                });
            }
        }
    }

    /// Revokes a global admin, then re-reads the list. The revoke arrives already confirmed by
    /// the pane — a two-step destructive action on an accidental click is the pane's rule, not
    /// the worker's, because only the pane knows what a row's click meant.
    async fn revoke_admin(&mut self, account_id: Id) {
        let outcome = match self.signed.as_ref() {
            Some(signed) => {
                signed
                    .rest
                    .revoke_global_admin(&signed.access_token, account_id)
                    .await
            }
            None => return,
        };
        match outcome {
            Ok(()) => {
                self.sink.toast("Admin revoked.", ToastKind::Success);
                self.fetch_admins().await;
            }
            Err(error) => {
                self.sink.send(Event::AdminChangeFailed {
                    reason: error.to_string(),
                });
            }
        }
    }

    /// Fetches the device/session list over REST and reduces it to rows the settings screen can
    /// draw without knowing what JSON is.
    async fn fetch_sessions(&mut self) {
        let Some(signed) = self.signed.as_ref() else {
            return;
        };
        let outcome = signed.rest.sessions(&signed.access_token).await;
        let event = match outcome {
            Ok(list) => {
                let rows = list
                    .into_iter()
                    .map(|session| SessionRow {
                        session_id: session.session_id,
                        device: session
                            .device
                            .as_ref()
                            .and_then(|device| {
                                let name = device.display_name.as_deref();
                                name.filter(|name| !name.is_empty()).or_else(|| {
                                    device
                                        .platform
                                        .as_deref()
                                        .filter(|platform| !platform.is_empty())
                                })
                            })
                            .map(str::to_owned)
                            .unwrap_or_else(|| model::short_id(session.session_id)),
                        created_at: session.created_at,
                        last_active_at: session.last_active_at,
                        current: session.current,
                    })
                    .collect::<Vec<_>>();
                Event::Sessions(Ok(rows))
            }
            Err(error) => Event::Sessions(Err(error.to_string())),
        };
        self.sink.send(event);
    }

    /// Ends one session of the account, then re-reads the list.
    ///
    /// Revoking the session this window runs on would leave a signed-out client holding
    /// decrypted history, so the settings panel does not offer the button for it; if a revoke
    /// arrives here anyway (or the server ends the session out from under the list), the
    /// refresh that follows is what reconciles the UI with the truth.
    async fn revoke_session(&mut self, session_id: Id) {
        let outcome = match self.signed.as_ref() {
            Some(signed) => {
                signed
                    .rest
                    .revoke_session(&signed.access_token, session_id)
                    .await
            }
            None => return,
        };
        match outcome {
            Ok(()) => {
                self.sink.toast("Session ended", ToastKind::Success);
                self.fetch_sessions().await;
            }
            Err(error) => {
                self.sink.toast(error.to_string(), ToastKind::Error);
            }
        }
    }

    // --- the account-root surface -----------------------------------------------

    /// Reads the account's devices over REST for the security panel.
    async fn fetch_devices(&mut self) {
        let Some(signed) = self.signed.as_ref() else {
            return;
        };
        let outcome = signed.rest.devices(&signed.access_token).await;
        let event = match outcome {
            Ok(list) => {
                let rows = list
                    .into_iter()
                    .map(|device| DeviceRow {
                        device_id: device.device_id,
                        display_name: if device.display_name.is_empty() {
                            model::short_id(device.device_id)
                        } else {
                            device.display_name
                        },
                        platform: device.platform,
                        status: device.status,
                        created_at: Some(Timestamp::from_unix_ms(device.created_at_ms)),
                        last_seen: Some(Timestamp::from_unix_ms(device.last_seen_at_ms)),
                        has_credential: device.has_credential,
                        is_current: device.is_current,
                    })
                    .collect::<Vec<_>>();
                Event::Devices(Ok(rows))
            }
            Err(error) => Event::Devices(Err(error.to_string())),
        };
        self.sink.send(event);
    }

    /// Removes one of the account's devices over REST, then re-reads the list.
    ///
    /// The toast names how many sessions ended with the device, because "gone" and "gone, with
    /// its two sessions" are different facts to the person who pressed the button.
    async fn revoke_device(&mut self, device_id: Id) {
        let Some(signed) = self.signed.as_ref() else {
            return;
        };
        let outcome = signed
            .rest
            .revoke_device(&signed.access_token, device_id)
            .await;
        match outcome {
            Ok(answer) => {
                self.sink.toast(
                    format!(
                        "Device removed; {} session{} ended",
                        answer.revoked,
                        if answer.revoked == 1 { "" } else { "s" }
                    ),
                    ToastKind::Success,
                );
                self.fetch_devices().await;
                self.fetch_sessions().await;
            }
            Err(error) => {
                self.sink.toast(error.to_string(), ToastKind::Error);
            }
        }
    }

    /// Reads the account's registered wallet addresses over REST.
    async fn fetch_wallets(&mut self) {
        let Some(signed) = self.signed.as_ref() else {
            return;
        };
        let outcome = signed.rest.wallets(&signed.access_token).await;
        let event = match outcome {
            Ok(list) => {
                let rows = list
                    .into_iter()
                    .map(|wallet| EvmWalletRow {
                        wallet_id: wallet.wallet_id,
                        address: wallet.address,
                        derivation_index: wallet.derivation_index,
                        status: wallet.status,
                        label: wallet.label,
                    })
                    .collect::<Vec<_>>();
                Event::Wallets(Ok(rows))
            }
            Err(error) => Event::Wallets(Err(error.to_string())),
        };
        self.sink.send(event);
    }

    /// Publishes the identity and device-credential public keys: the legacy upgrade door.
    ///
    /// Idempotent by the server's design, so the worker calls it after every sign-in on a device
    /// that holds the root rather than tracking whether it already did — a retry reconciles to the
    /// rows that exist. Only the public halves cross the wire; nothing here can leak the root.
    async fn publish_root_material(&mut self) {
        let Some(signed) = self.signed.as_ref() else {
            return;
        };
        let Some(identity) = signed.sessions.keys().identity_key() else {
            return;
        };
        let Some(credential) = signed.sessions.keys().device_credential() else {
            return;
        };
        if let Err(error) = signed
            .rest
            .publish_identity_key(
                &signed.access_token,
                &identity.public_key(),
                Some(&credential.public_key()),
            )
            .await
        {
            // A toast rather than a failed sign-in: the passphrase already worked, and the keys
            // publish again on the next sign-in.
            self.sink.toast(
                format!("could not publish the account identity: {error}"),
                ToastKind::Error,
            );
        }
    }

    /// Registers any of the root's first wallets the server does not know yet.
    ///
    /// The address is a pure function of the root, so "which wallets exist" is server state, not a
    /// matter of opinion: every address the root derives that is not registered gets registered,
    /// which after a container restore re-creates the wallet list in derivation order, and on a
    /// brand-new account registers the one wallet that has existed since the root did.
    async fn sync_wallets(&mut self) {
        let Some(signed) = self.signed.as_ref() else {
            return;
        };
        let Some(root) = signed.sessions.keys().root() else {
            return;
        };
        let Ok(known) = signed.rest.wallets(&signed.access_token).await else {
            return;
        };
        // The registry speaks canonical form (lowercase, no prefix) and the derivation speaks
        // EIP-55 with the 0x prefix; both are folded before comparing, and archived rows count
        // as known — a wallet the user archived stays archived, and this sync must not
        // resurrect it by re-registering the address.
        let registered: HashSet<String> = known
            .into_iter()
            .map(|wallet| canonical_address(&wallet.address))
            .collect();
        // The first eight indexes cover a personal account generously; a user past that has made a
        // habit of wallet rotation and can archive and register from a client that shows the list.
        for index in 0..8u32 {
            let Ok(wallet) = migo_account::EvmWallet::from_root(&root, index) else {
                return;
            };
            let address = wallet.address_checksummed();
            if registered.contains(&wallet.address_canonical()) {
                continue;
            }
            if let Err(error) = signed
                .rest
                .register_wallet(&signed.access_token, &address, index as i32, None)
                .await
            {
                self.sink.toast(
                    format!("could not register wallet {index}: {error}"),
                    ToastKind::Error,
                );
                return;
            }
        }
        self.fetch_wallets().await;
    }

    /// Archives one registered wallet address.
    ///
    /// The address stays the address — it is a pure function of the root — but it leaves the
    /// account's active list, which is what other clients read. Deriving it again is not
    /// "restoring" it; registering it again is.
    async fn archive_wallet(&mut self, wallet_id: Id) {
        let Some(signed) = self.signed.as_ref() else {
            return;
        };
        let outcome = signed
            .rest
            .archive_wallet(&signed.access_token, wallet_id)
            .await;
        match outcome {
            Ok(()) => {
                self.sink
                    .toast("Wallet address archived", ToastKind::Success);
                self.fetch_wallets().await;
            }
            Err(error) => {
                self.sink.toast(error.to_string(), ToastKind::Error);
            }
        }
    }

    /// Records (or replaces) the account's recoverable contact.
    ///
    /// The server keeps exactly one value, normalised on arrival, so this is a replace rather
    /// than an append — which is what the form's helper text says, so nobody expects a list.
    async fn set_contact(&mut self, email_or_phone: String) {
        let Some(signed) = self.signed.as_ref() else {
            return;
        };
        let outcome = signed
            .rest
            .set_contact(&signed.access_token, email_or_phone.trim())
            .await;
        match outcome {
            Ok(()) => {
                self.sink
                    .toast("Recovery contact saved", ToastKind::Success);
            }
            Err(error) => {
                self.sink.toast(error.to_string(), ToastKind::Error);
            }
        }
    }

    /// Reads whether the account has a recoverable contact, for the checkup's Recovery row.
    ///
    /// Reduced to the one bit the row asks: the standing struct carries nothing else today, and
    /// a worker that passed it through whole would be promising a surface the server does not
    /// offer. A save through [`Self::set_contact`] does not refresh the row — the server's 204
    /// says nothing about the value's shape, so the row re-checks on its own click, the same
    /// rule the device list holds after a revoke.
    async fn fetch_contact_standing(&mut self) {
        let Some(signed) = self.signed.as_ref() else {
            return;
        };
        let outcome = signed
            .rest
            .contact_standing(&signed.access_token)
            .await
            .map(|standing| standing.configured);
        let event = match outcome {
            Ok(configured) => Event::ContactStanding(Ok(configured)),
            Err(error) => Event::ContactStanding(Err(error.to_string())),
        };
        self.sink.send(event);
    }

    /// Changes the account's sign-in passphrase.
    ///
    /// The server ends every session of the account — this one included — and answers with a
    /// replacement grant, which this worker adopts so the window stays signed in (a reconnect
    /// after the server drops the old socket presents the new token). The vault cannot be
    /// re-sealed from here — it opens under the device's own passphrase, which no session holds,
    /// by design — so the saved refresh token stays retired: the next unlock falls back to the
    /// ML-DSA ceremony where the device can run one, and asks for the new passphrase otherwise.
    /// That is the honest cost of the change, and the form states it before the button unlocks.
    async fn change_passphrase(&mut self, current: String, next: String) {
        let Some(signed) = self.signed.as_mut() else {
            return;
        };
        let outcome = signed
            .rest
            .change_passphrase(&signed.access_token, &current, &next)
            .await;
        match outcome {
            Ok(grant) => {
                signed.access_token = grant.access_token;
                signed.account.session_id = grant.session_id;
                self.sink.toast(
                    "Passphrase changed; every other session was signed out",
                    ToastKind::Success,
                );
            }
            Err(error) => {
                self.sink.toast(error.to_string(), ToastKind::Error);
            }
        }
    }

    /// Rotates the account's ML-DSA identity key: mint a successor, seal it into the vault, then
    /// ask the server to accept it.
    ///
    /// # What rotates, and what does not
    ///
    /// The server retires the account's active identity key and installs the successor the answer
    /// carries, in one transaction: sessions are unaffected, and the E2EE layer is untouched — the
    /// device credential, the Ed25519/X25519 identity, every ratchet and every safety number peers
    /// have verified stay exactly as they were. The ceremony exists for the day the identity key
    /// itself is the thing suspected, and it replaces nothing else.
    ///
    /// # The successor, and why it is not derived from the root
    ///
    /// The current key is the root's identity derivation, and the reference crate defines no
    /// second derivation — there is no `V2` domain to derive a successor with, and inventing one
    /// here would fork the protocol from the other three clients. So the successor is a fresh
    /// random seed, `IdentityKey::from_seed`, sealed into the vault as FIELD_ROTATED_IDENTITY.
    /// `DeviceKeys::identity_key` prefers that seed from the moment it is written, which is what
    /// keeps every later ceremony — the unlock fallback's login, a container restore, the next
    /// rotation — signing with the key the server actually knows.
    ///
    /// # The ordering, and the crash window it leaves
    ///
    /// The vault is re-sealed *before* the rotate call, not after. The other order has the worse
    /// failure: a crash between the server's acceptance and the vault's save would retire the key
    /// every copy of the vault still holds, and the retired key cannot sign for its own successor
    /// — no rotation and no identity login, ever again from this device. Pre-committing inverts
    /// the window: a crash between the save and the call leaves the vault holding a successor the
    /// server never accepted, and the next attempt here signs with that successor and is refused
    /// as invalid credentials. That window is healed below — the same challenge is answered again
    /// under the root-derived key, which the refusal did not consume, because the server verifies
    /// the signature *before* it retires the challenge. A device that can rotate always holds the
    /// root: without one there is no key to sign with at all.
    ///
    /// A refusal the server actually answered rolls the vault back to what it held before the
    /// attempt, so a refused rotation costs nothing. A failure with no answer at all — the
    /// request may have landed and only its reply been lost — keeps the pre-commit sealed
    /// instead, because rolling back then would strand the account: the next attempt signs with
    /// the sealed successor, which resolves it in either direction. The one unrecoverable split
    /// is another root-holding device rotating first: the server's active key becomes one this
    /// vault never saw, both signatures are refused, and the rollback leaves this device signed
    /// in but unable to run further identity ceremonies — the toast reports the refusal rather
    /// than guessing which case it is.
    ///
    /// # What the successor does not fix
    ///
    /// A `.migo` container sealed before the rotation carries only the root, so restoring onto a
    /// new device signs with the root-derived key and will be refused until a fresh container is
    /// sealed on this device. Other root-holding devices are in the same position. Rotation is
    /// real rotation exactly because it breaks the derivation, and the settings dialog says so
    /// before the passphrase is ever asked for. The checkup's backup date is retired with the
    /// same stroke ([`Self::retire_backup_stamp`]): a row that dated a container the account can
    /// no longer restore from would be the one lie a security screen must never tell.
    async fn rotate_identity(&mut self, passphrase: String) {
        let Some(account_id) = self.signed.as_ref().map(|signed| signed.account.account_id) else {
            return;
        };

        // The vault has to open: the successor is sealed under this passphrase, because the
        // worker holds no passphrase after unlock, and a successor that never reached the vault
        // is a key nobody holds the moment the window closes.
        let mut keys = match vault::load(&self.vault_path, &passphrase) {
            Ok(keys) => keys,
            Err(error) => {
                return self.sink.toast(
                    format!("the vault did not open ({error}); the identity key was not rotated"),
                    ToastKind::Error,
                );
            }
        };
        // The vault belongs to this session's account or the ceremony is not this device's to run.
        if keys
            .session
            .as_ref()
            .is_some_and(|saved| saved.account_id != account_id)
        {
            return self.sink.toast(
                "this vault belongs to another account; the identity key was not rotated"
                    .to_owned(),
                ToastKind::Error,
            );
        }
        let Some(current) = keys.identity_key() else {
            return self.sink.toast(
                "this device holds no identity key, so it cannot rotate one".to_owned(),
                ToastKind::Error,
            );
        };

        let challenge = {
            let Some(signed) = self.signed.as_ref() else {
                return;
            };
            signed
                .rest
                .identity_rotation_challenge(&signed.access_token)
                .await
        };
        let challenge = match challenge {
            Ok(challenge) => challenge,
            Err(error) => return self.sink.toast(error.to_string(), ToastKind::Error),
        };
        // Signed exactly as received, never re-encoded — the same rule every ceremony holds.
        let Some(payload) = base64_decode(&challenge.payload) else {
            return self.sink.toast(
                "the server's challenge payload was not base64".to_owned(),
                ToastKind::Error,
            );
        };
        let signature = match current.sign_rotate(&payload) {
            Ok(signature) => signature,
            Err(error) => return self.sink.toast(error.to_string(), ToastKind::Error),
        };

        // The successor: a fresh random seed, minted here. Sealed before the call, adopted after
        // the server's acceptance — the order the doc comment above spends its paragraphs on.
        let mut seed = [0u8; 32];
        OsRandom.fill_bytes(&mut seed);
        let successor = match migo_account::IdentityKey::from_seed(&seed) {
            Ok(key) => key,
            Err(error) => return self.sink.toast(error.to_string(), ToastKind::Error),
        };

        let prior_seed = keys.rotated_identity_seed;
        keys.rotated_identity_seed = Some(seed);
        self.carry_live_records(&mut keys, account_id);
        if let Err(error) = vault::save(&self.vault_path, &passphrase, &keys) {
            return self.sink.toast(
                format!("the vault could not be re-sealed ({error}); nothing was rotated"),
                ToastKind::Error,
            );
        }

        let outcome = {
            let Some(signed) = self.signed.as_ref() else {
                return;
            };
            signed
                .rest
                .identity_rotate(
                    &signed.access_token,
                    challenge.challenge_id,
                    &signature,
                    &successor.public_key(),
                )
                .await
        };
        match outcome {
            Ok(()) => {
                if let Some(signed) = self.signed.as_mut() {
                    signed.sessions.adopt_rotated_identity_seed(seed);
                }
                self.sink.toast(
                    "Identity key rotated; sessions, chats and safety numbers continue unchanged",
                    ToastKind::Success,
                );
                // The rotation is accepted, so every container sealed before it is dead as a
                // vouch: the stamp that dated one must go, in the vault and in the checkup.
                self.retire_backup_stamp(account_id, &mut keys, &passphrase);
            }
            Err(error) => {
                // A failure with no answer at all is not a refusal: the call may have landed and
                // only its answer been lost, and rolling back then would strand the account — the
                // vault would go on holding the retired root derivation while the successor it
                // minted exists nowhere. The pre-commit stays sealed, and the next attempt
                // resolves it either way: it signs with the sealed successor, which the server
                // accepts if the call did land, and refuses invalid credentials otherwise — the
                // heal below, which signs with the root.
                if matches!(error, RestError::Transport) {
                    // The stamp retires here too, even though the rotation's landing is unknown:
                    // a checkup must fail closed, and "Never backed up on this device" is true in
                    // both branches of the unknown — either the container is dead, or the account
                    // sits mid-rotation with a fresh one owed. A date kept here would vouch for a
                    // backup the rotation may already have retired.
                    self.retire_backup_stamp(account_id, &mut keys, &passphrase);
                    return self.sink.toast(
                        "the server's answer never arrived; the new key is sealed in this \
                         vault — rotate again to finish the change",
                        ToastKind::Info,
                    );
                }
                // The crash-window heal: the vault already carried a successor this server never
                // accepted, and the signature above was made with it. The root-derived key is
                // the key the server still knows, and a refused signature did not consume the
                // challenge — answer the same one again with the root's key.
                let recoverable = matches!(
                    &error,
                    RestError::Server { symbol, .. } if symbol == "INVALID_CREDENTIALS"
                ) && prior_seed.is_some();
                if recoverable {
                    if let Some(root) = keys.root() {
                        let root_key = migo_account::IdentityKey::from_root(&root);
                        if let Ok(retry) = root_key.sign_rotate(&payload) {
                            let second = {
                                let Some(signed) = self.signed.as_ref() else {
                                    return;
                                };
                                signed
                                    .rest
                                    .identity_rotate(
                                        &signed.access_token,
                                        challenge.challenge_id,
                                        &retry,
                                        &successor.public_key(),
                                    )
                                    .await
                            };
                            if let Ok(()) = second {
                                if let Some(signed) = self.signed.as_mut() {
                                    signed.sessions.adopt_rotated_identity_seed(seed);
                                }
                                self.sink.toast(
                                    "Identity key rotated; sessions, chats and safety numbers \
                                     continue unchanged",
                                    ToastKind::Success,
                                );
                                // The heal is an acceptance like any other: the stamp goes.
                                self.retire_backup_stamp(account_id, &mut keys, &passphrase);
                                return;
                            }
                        }
                    }
                }
                // Rollback: the vault goes back to the seed it held before the attempt. The live
                // records folded in above stay — they are this process's newer copies, exactly as
                // at unlock — but the successor does not, and a refused rotation costs nothing.
                keys.rotated_identity_seed = prior_seed;
                if let Err(save_error) = vault::save(&self.vault_path, &passphrase, &keys) {
                    self.sink.toast(
                        format!(
                            "the identity key was not rotated ({error}), and the vault could not \
                             be restored: {save_error} — rotate again to retry the ceremony"
                        ),
                        ToastKind::Error,
                    );
                } else {
                    self.sink.toast(
                        format!("the identity key was not rotated: {error}"),
                        ToastKind::Error,
                    );
                }
            }
        }
    }

    /// Retires the last-backup stamp everywhere it lives, because a rotation just made every
    /// container sealed before it unable to vouch the account.
    ///
    /// Three places hold the date: the vault (which this re-seals with the stamp gone, the
    /// passphrase still in hand from the ceremony), the worker's live record (so the next
    /// passphrase moment does not fold a dead date back in), and the checkup row (the event).
    /// A re-seal failure is reported but changes nothing about the rotation, which already
    /// happened: the worker's cleared live record wins at the next door over whatever the vault
    /// kept, which is exactly why the record exists.
    fn retire_backup_stamp(&mut self, account_id: Id, keys: &mut DeviceKeys, passphrase: &str) {
        keys.last_backup_at = None;
        self.last_backup_at = Some((account_id, None));
        if let Err(error) = vault::save(&self.vault_path, passphrase, keys) {
            self.sink.toast(
                format!(
                    "the rotation stands, but the vault could not be re-sealed to forget the old \
                     backup date: {error}"
                ),
                ToastKind::Error,
            );
        }
        self.sink.send(Event::BackupState {
            last_backup_at: None,
        });
    }

    // --- the chain wallet (§184) --------------------------------------------------

    /// What a device without the root is told, in one sentence, wherever the AVAX wallet is
    /// asked for. Additional devices have no wallet here at all — the address is a function of
    /// the root — and pretending otherwise would be a wallet surface that cannot send.
    const NO_ROOT_ON_DEVICE: &str =
        "this device does not hold the account root, so it has no AVAX \
     wallet; open the wallet on the device that holds the account backup";

    /// A chain client for one operation, pinned to the network's own RPC constant.
    fn chain_client(&self, network: ChainNetwork) -> ChainClient {
        ChainClient::connect(network.network(), self.chain_http.clone())
    }

    /// The account's first wallet: its EIP-55 address and its AVAX balance on one network.
    ///
    /// Wallet 0 is the wallet a registration mints and the only one the send flow offers; a
    /// user past index zero rotates addresses on purpose and is not this surface's caller.
    async fn chain_balance(&mut self, network: ChainNetwork) {
        let Some(signed) = self.signed.as_ref() else {
            return;
        };
        let Some(root) = signed.sessions.keys().root() else {
            return self.sink.send(Event::ChainBalance {
                network,
                address: None,
                balance: Err(Self::NO_ROOT_ON_DEVICE.to_owned()),
            });
        };
        let Ok(wallet) = migo_account::EvmWallet::from_root(&root, 0) else {
            return self.sink.send(Event::ChainBalance {
                network,
                address: None,
                balance: Err("the account root did not derive a wallet".to_owned()),
            });
        };
        let address = wallet.address_checksummed();
        let mut client = self.chain_client(network);
        let balance = client
            .get_balance(wallet.address())
            .await
            .map_err(|error| error.to_string());
        self.sink.send(Event::ChainBalance {
            network,
            address: Some(address),
            balance,
        });
    }

    /// Builds one AVAX transfer from the RPC's own answers, and nothing else.
    ///
    /// Parse failures happen before a single RPC leaves: a bad recipient or a bad amount is a
    /// form problem, and the network is not asked to confirm the shape of a text field.
    async fn chain_prepare(
        &mut self,
        network: ChainNetwork,
        recipient: String,
        amount_avax: String,
    ) {
        let Some(signed) = self.signed.as_ref() else {
            return;
        };
        let to = match migo_account::parse_address(recipient.trim()) {
            Ok(to) => to,
            Err(error) => return self.sink.send(Event::ChainPrepared(Err(error.to_string()))),
        };
        let Some(value) = model::parse_avax(&amount_avax) else {
            return self.sink.send(Event::ChainPrepared(Err(
                "the amount is not a valid AVAX amount, e.g. 1.5".to_owned(),
            )));
        };
        let Some(root) = signed.sessions.keys().root() else {
            return self.sink.send(Event::ChainPrepared(
                Err(Self::NO_ROOT_ON_DEVICE.to_owned()),
            ));
        };
        let Ok(wallet) = migo_account::EvmWallet::from_root(&root, 0) else {
            return self.sink.send(Event::ChainPrepared(Err(
                "the account root did not derive a wallet".to_owned(),
            )));
        };

        let mut client = self.chain_client(network);
        // The fees, the gas, and the nonce are three reads the confirm screen quotes, so all
        // three are asked before the prepared transaction exists — a prepared transaction with a
        // guessed field is a confirmation screen that lies about one of its lines.
        let fees = match client.get_fees().await {
            Ok(fees) => fees,
            Err(error) => return self.sink.send(Event::ChainPrepared(Err(error.to_string()))),
        };
        let gas_limit = match client
            .estimate_gas(Some(wallet.address()), &to, value)
            .await
        {
            Ok(gas) => gas,
            Err(error) => return self.sink.send(Event::ChainPrepared(Err(error.to_string()))),
        };
        let nonce = match client.get_nonce(wallet.address()).await {
            Ok(nonce) => nonce,
            Err(error) => return self.sink.send(Event::ChainPrepared(Err(error.to_string()))),
        };

        self.sink.send(Event::ChainPrepared(Ok(PreparedTx {
            network,
            from: wallet.address_checksummed(),
            to: migo_account::evm::eip55(&to),
            value_wei: value,
            max_priority_fee_per_gas: fees.max_priority_fee_per_gas,
            max_fee_per_gas: fees.max_fee_per_gas,
            gas_limit,
            nonce,
        })));
    }

    /// Signs and broadcasts exactly the transaction the confirm screen displayed.
    ///
    /// Every field is re-derived from the prepared struct the UI sent back: the recipient is
    /// re-parsed (an EIP-55 checksum that survived a tamper fails here), the sender is checked
    /// against this device's own wallet 0, and the chain id comes from the named network — never
    /// from a field a screen could have edited. What is signed is what was shown.
    async fn chain_send(&mut self, tx: PreparedTx) {
        let Some(signed) = self.signed.as_ref() else {
            return;
        };
        let to = match migo_account::parse_address(tx.to.trim()) {
            Ok(to) => to,
            Err(error) => return self.sink.send(Event::ChainSent(Err(error.to_string()))),
        };
        let Some(root) = signed.sessions.keys().root() else {
            return self
                .sink
                .send(Event::ChainSent(Err(Self::NO_ROOT_ON_DEVICE.to_owned())));
        };
        let Ok(wallet) = migo_account::EvmWallet::from_root(&root, 0) else {
            return self.sink.send(Event::ChainSent(Err(
                "the account root did not derive a wallet".to_owned(),
            )));
        };
        // The `from` on screen must be this device's wallet 0: a prepared transaction carried
        // over from another device, or an older derivation, is refused rather than signed with
        // the wrong key for the right-looking screen.
        if tx.from != wallet.address_checksummed() {
            return self.sink.send(Event::ChainSent(Err(
                "the prepared transaction names a different sender; prepare it again here"
                    .to_owned(),
            )));
        }

        let body = migo_account::Eip1559Tx {
            chain_id: tx.network.network().chain_id,
            nonce: tx.nonce,
            max_priority_fee_per_gas: tx.max_priority_fee_per_gas,
            max_fee_per_gas: tx.max_fee_per_gas,
            gas_limit: tx.gas_limit,
            to,
            value: tx.value_wei,
            data: Vec::new(),
        };
        let signed_tx = match body.sign(&wallet) {
            Ok(signed) => signed,
            Err(error) => return self.sink.send(Event::ChainSent(Err(error.to_string()))),
        };

        let mut client = self.chain_client(tx.network);
        let tx_hash = match client.broadcast(&signed_tx).await {
            Ok(answered) => answered,
            Err(error) => return self.sink.send(Event::ChainSent(Err(error.to_string()))),
        };

        // The record is written at broadcast, not at settle: a crash mid-tracking loses the
        // ending, never the fact that value left.
        let record = TxRecord {
            tx_hash: *signed_tx.tx_hash(),
            chain_id: body.chain_id,
            to,
            value_wei: body.value,
            fee_wei: body
                .max_fee_per_gas
                .saturating_mul(u128::from(body.gas_limit)),
            gas_limit: body.gas_limit,
            at_unix: u64::try_from(Timestamp::now().as_unix_ms().max(0) / 1000)
                .expect("unix seconds fit in u64 by construction"),
            outcome: "PENDING".to_owned(),
            block: None,
            gas_used: None,
        };
        let account_id = signed.account.account_id;
        let records = self.txs.get_or_insert_with(|| (account_id, Vec::new()));
        records.1.insert(0, record);

        // Acceptance, not confirmation — the tracker task below is the only thing that can say
        // CONFIRMED, and it says so through this worker's own command loop.
        self.sink.send(Event::ChainSent(Ok(tx_hash.clone())));
        self.sink.send(Event::ChainActivity(self.chain_rows()));

        let sink = self.sink.clone();
        let commands = self.commands.clone();
        let network = tx.network;
        let http = self.chain_http.clone();
        let hash = tx_hash;
        tokio::spawn(async move {
            let mut client = ChainClient::connect(network.network(), http);
            let states_sink = sink.clone();
            let states_hash = hash.clone();
            let (outcome, block, gas_used) = match client
                .track(&hash, &TrackOptions::default(), move |state| {
                    states_sink.send(Event::ChainState {
                        tx_hash: states_hash.clone(),
                        state: state.to_owned(),
                    });
                })
                .await
            {
                Ok(result) => (
                    result.outcome.label().to_owned(),
                    result.block_number,
                    result.gas_used,
                ),
                // An endpoint that cannot be asked at all is still an unresolved ending, and
                // EXPIRED is the honest name for one this client watched for its whole deadline.
                Err(_) => ("EXPIRED".to_owned(), None, None),
            };
            let _ = commands.send(Command::ChainSettled {
                network,
                tx_hash: hash,
                outcome,
                block,
                gas_used,
            });
        });
    }

    /// A tracker finished: the record's ending is written where the vault will next read it.
    ///
    /// `network` is carried for the command's own readability and the record is keyed by hash —
    /// the hash is the one thing the chain, the tracker and the user all agree on.
    async fn chain_settled(
        &mut self,
        network: ChainNetwork,
        tx_hash: String,
        outcome: String,
        block: Option<u64>,
        gas_used: Option<u128>,
    ) {
        let _ = network;
        if let Some((_, records)) = self.txs.as_mut() {
            for record in &mut *records {
                if hex_of(&record.tx_hash) == tx_hash {
                    record.outcome.clone_from(&outcome);
                    if block.is_some() {
                        record.block = block;
                    }
                    if gas_used.is_some() {
                        record.gas_used = gas_used;
                    }
                    break;
                }
            }
        }
        self.sink.send(Event::ChainSettled {
            tx_hash,
            outcome: outcome.clone(),
        });
        self.sink.send(Event::ChainActivity(self.chain_rows()));
    }

    /// The tracked-transaction list as the wallet surface draws it, newest first.
    fn chain_rows(&self) -> Vec<ChainTxRow> {
        self.txs
            .as_ref()
            .map(|(_, records)| {
                records
                    .iter()
                    .map(|record| ChainTxRow {
                        tx_hash: format!("0x{}", hex_of(&record.tx_hash)),
                        network: ChainNetwork::of_chain_id(record.chain_id).map_or_else(
                            || format!("chain {}", record.chain_id),
                            |n| n.label().to_owned(),
                        ),
                        to: migo_account::evm::eip55(&record.to),
                        value_wei: record.value_wei,
                        fee_wei: record.fee_wei,
                        at: Timestamp::from_unix_ms(
                            i64::try_from(record.at_unix).expect("unix seconds fit in i64") * 1000,
                        ),
                        outcome: record.outcome.clone(),
                        block: record.block,
                        gas_used: record.gas_used,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Seals the account root into a `.migo` recovery container at `path`.
    ///
    /// The container names the account, so the next device can run the add-device ceremony from
    /// the file alone; the recovery credential that opens it never leaves the user's head.
    async fn export_container(&mut self, path: PathBuf, credential: String) {
        let Some(signed) = self.signed.as_ref() else {
            return;
        };
        let Some(root) = signed.sessions.keys().root() else {
            self.sink.toast(
                "this device does not hold the account root; make the backup on a device that \
                 does, or keep the container you already have",
                ToastKind::Error,
            );
            return;
        };
        let account_id = signed.account.account_id;
        let now = u64::try_from(Timestamp::now().as_unix_ms().max(0) / 1000)
            .expect("unix seconds fit in u64 by construction");
        let mut file =
            migo_account::AccountFile::new(&root, now).for_account(&account_id.to_text());
        // A device that has rotated its identity seals the successor's seed beside the root:
        // the successor is fresh randomness with no derivation from the root, so a container
        // without it restores a device whose add-device signature the server refuses — the
        // container format grew the field for exactly this export.
        if let Some(seed) = signed.sessions.keys().rotated_identity_seed {
            file = file.for_identity(&seed);
        }
        let container = match migo_account::seal_container(&credential, &file, &mut OsRandom) {
            Ok(bytes) => bytes,
            Err(error) => return self.sink.toast(error.to_string(), ToastKind::Error),
        };
        if let Err(error) = std::fs::write(&path, &container) {
            return self.sink.toast(error.to_string(), ToastKind::Error);
        }
        // The stamp is the worker's until the next passphrase moment re-seals the vault — the
        // export path holds the container's credential, never the vault's passphrase — and the
        // same instant the container itself was stamped with, so the checkup's date and the
        // container's own idea of its birthday can never disagree.
        self.last_backup_at = Some((account_id, Some(now)));
        self.sink.send(Event::BackupState {
            last_backup_at: Some(now),
        });
        self.sink.toast(
            format!("account backup written to {}", path.display()),
            ToastKind::Success,
        );
    }

    /// Restores the account from a `.migo` container onto this device, through one of two doors —
    /// see [`Command::ImportContainer`] for which door opens when.
    async fn import_container(
        &mut self,
        path: PathBuf,
        credential: String,
        passphrase: String,
        username: String,
        server: ServerEndpoint,
    ) {
        self.sink.send(Event::Connection(Connection::Connecting));

        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) => return self.fail(error.to_string()),
        };
        let file = match migo_account::open_container(&credential, &bytes) {
            Ok(file) => file,
            Err(error) => return self.fail(error.to_string()),
        };
        let root = match file.root() {
            Ok(root) => root,
            Err(error) => return self.fail(error.to_string()),
        };
        let Some(account_id) = file
            .account_id
            .as_deref()
            .and_then(|text| Id::parse(text).ok())
        else {
            return self.fail(
                "this container does not name its account (it was sealed by an older build); \
                 sign in with your passphrase instead"
                    .to_owned(),
            );
        };
        let root_bytes: [u8; 32] = root
            .as_bytes()
            .try_into()
            .expect("the root is 32 bytes by construction");

        // The vault decides the door. A vault the passphrase opens, holding the container's own
        // account and root, is tier one: the same device returning with its file. A vault that
        // opens as anything else keeps the refusal it has always had, and a vault the passphrase
        // does not open gets the same refusal with the honest reason attached — another account's
        // vault, or a passphrase typed for the new vault rather than the existing one, are both
        // live possibilities and the sentence does not choose between them for the user.
        if vault::exists(&self.vault_path) {
            return match vault::load(&self.vault_path, &passphrase) {
                Ok(keys) if vault_holds_this_account(&keys, account_id, root_bytes) => {
                    // The matcher requires the saved session, so the device record tier one
                    // names the device with is present by construction.
                    let saved = keys
                        .session
                        .clone()
                        .expect("the matcher requires a session");
                    let rest = match Rest::new(&crate::config::rest_base_url(&server)) {
                        Ok(rest) => rest,
                        Err(error) => return self.fail(error.to_string()),
                    };
                    // The signing discipline is the unlock fallback's own: the challenge is
                    // requested for the SAVED username and device id, the payload is signed
                    // exactly as received with the identity key and the device credential the
                    // vault already holds. `None` is this client's refusal to guess a reason —
                    // the server answers an unknown device the way it answers a wrong
                    // passphrase — so the sentence below stays on what actually failed.
                    match self.ceremony_login(&rest, &keys, &saved).await {
                        Some(grant) => {
                            self.resume_restored_device(
                                rest, keys, saved, grant, passphrase, server,
                            )
                            .await;
                        }
                        None => self.fail(
                            "this device already holds this account's keys, but signing back in \
                             as this device was refused; unlock the vault with its passphrase \
                             instead"
                                .to_owned(),
                        ),
                    }
                }
                Ok(_) => self.fail(
                    "this device already has a vault; remove it deliberately before restoring \
                     onto this machine"
                        .to_owned(),
                ),
                Err(_) => self.fail(
                    "this device already has a vault and this passphrase did not open it — the \
                     vault may belong to another account, or the passphrase may be the one for \
                     the new vault rather than the existing vault's own; remove the vault \
                     deliberately before restoring onto this machine"
                        .to_owned(),
                ),
            };
        }

        let rest = match Rest::new(&crate::config::rest_base_url(&server)) {
            Ok(rest) => rest,
            Err(error) => return self.fail(error.to_string()),
        };

        // The new device: fresh E2EE identity, fresh credential, plus the root the container carried.
        // A container sealed after a rotation carries the successor's seed as well, and that half
        // decides the ceremony's signature: the server knows the successor as the account's active
        // key, while the root's own derivation — the only key a pre-rotation container restores —
        // is retired, and a ceremony signed with it is refused. Installing the seed into the vault
        // alongside is what keeps every later ceremony on this device signing with the active key.
        let mut keys = DeviceKeys::additional();
        keys.root = Some(root_bytes);
        let identity = match file.rotated_identity_seed() {
            Ok(Some(seed)) => {
                keys.rotated_identity_seed = Some(seed);
                match migo_account::IdentityKey::from_seed(&seed) {
                    Ok(key) => key,
                    Err(error) => return self.fail(error.to_string()),
                }
            }
            Ok(None) => migo_account::IdentityKey::from_root(&root),
            Err(error) => return self.fail(error.to_string()),
        };
        let device_credential = keys
            .device_credential()
            .expect("additional() mints a credential");

        // The ceremony: describe the new device, sign the canonical payload with the identity key
        // (the account half) and the new credential (the device half), and present both.
        let device = DeviceRequest::describe(None);
        let challenge = match rest
            .identity_add_device_challenge(account_id, &device)
            .await
        {
            Ok(challenge) => challenge,
            Err(error) => return self.fail(error.to_string()),
        };
        let payload = match base64_decode(&challenge.payload) {
            Some(bytes) => bytes,
            None => return self.fail("the server's challenge payload was not base64".to_owned()),
        };
        let identity_signature = match identity.sign_login(&payload) {
            Ok(signature) => signature,
            Err(error) => return self.fail(error.to_string()),
        };
        let device_signature = match device_credential.sign_login(&payload) {
            Ok(signature) => signature,
            Err(error) => return self.fail(error.to_string()),
        };
        let grant = match rest
            .identity_add_device(
                challenge.challenge_id,
                &identity_signature,
                &device_credential.public_key(),
                &device_signature,
            )
            .await
        {
            Ok(grant) => grant,
            Err(error) => return self.fail(error.to_string()),
        };

        // The grant identifies the account by id; the username is a profile concern, not a session
        // one, and the server does not echo it back. The form's field is the greeting and the
        // identifier a later passphraseless login needs; if the person left it blank, the account id
        // stands in, honestly unpronounceable.
        let username = {
            let typed = username.trim();
            if typed.is_empty() {
                account_id.to_text()
            } else {
                typed.to_owned()
            }
        };
        keys.session = Some(SavedSession {
            server_url: crate::config::rest_base_url(&server),
            account_id,
            device_id: grant.device_id,
            username: username.clone(),
            refresh_token: grant.refresh_token.clone(),
        });
        // A container restore onto the device that already tracked this account's activity and
        // peers keeps the newer in-memory copies; anything else keeps the vault's own.
        self.carry_live_records(&mut keys, account_id);
        if let Err(error) = vault::save(&self.vault_path, &passphrase, &keys) {
            return self.fail(error.to_string());
        }

        self.establish(
            server,
            rest,
            keys,
            grant.account_id,
            grant.device_id,
            grant.session_id,
            username,
            grant.access_token,
        )
        .await;
        self.publish_root_material().await;
        self.sync_wallets().await;
    }

    /// The tier-one restore's second half: a session established on the keys the vault already
    /// held, not on fresh ones.
    ///
    /// This is the unlock path's own tail, deliberately. The grant's rotated refresh token is the
    /// only thing that changes inside the vault; the existing keys are re-sealed untouched — no
    /// `DeviceKeys::additional`, no fresh identity, no device slot spent — and the session picks
    /// up this device's ratchets where they left off. The new-device follow-ups of the tier-two
    /// path (publishing the account material, syncing the wallets) are skipped for the same
    /// reason: this is not a new device, and the login ceremony just proved the server already
    /// knows its credential.
    async fn resume_restored_device(
        &mut self,
        rest: Rest,
        keys: DeviceKeys,
        saved: SavedSession,
        grant: Grant,
        passphrase: String,
        server: ServerEndpoint,
    ) {
        let mut keys = keys;
        // The server rotates the refresh token on every exchange, so the vault has to be rewritten
        // or the next unlock would present a token the server has already retired — which it
        // treats as refresh reuse, and rightly so.
        keys.session = Some(SavedSession {
            refresh_token: grant.refresh_token.clone(),
            ..saved.clone()
        });
        // As at unlock: this process's own record of the same account's transactions and peer
        // fingerprints is the newer copy, and it is the one that gets sealed.
        self.carry_live_records(&mut keys, grant.account_id);
        if let Err(error) = vault::save(&self.vault_path, &passphrase, &keys) {
            return self.fail(error.to_string());
        }

        self.establish(
            server,
            rest,
            keys,
            grant.account_id,
            grant.device_id,
            grant.session_id,
            saved.username,
            grant.access_token,
        )
        .await;
    }

    /// Sends one text message: the sender-key path the web and Android clients speak.
    ///
    /// Every message — rooms and direct chats both — is sealed once with the conversation's chain
    /// and fanned out. Before that, the chain itself is distributed to any recipient device that
    /// lacks it, one pairwise-sealed `ControlEvent` per device, so the server never holds a chain
    /// key. The audience is every member's devices plus this account's own other devices (a
    /// message must reach the account's phone as surely as the peer's), minus this device.
    async fn send_text(&mut self, conversation_id: Id, text: String, expires_in_ms: Option<u32>) {
        let Some(signed) = self.signed.as_mut() else {
            return;
        };
        let mut random = OsRandom;
        let message_id = Id::generate_at(Timestamp::now(), &mut random);

        // Show it immediately, marked as sending. A message that appears only after the server
        // acknowledges it makes a slow link feel broken; one that appears at once and then gains a
        // tick tells the truth about what has happened so far.
        self.sink.send(Event::Message(Message {
            message_id,
            conversation_id,
            seq: 0,
            sender_id: signed.account.account_id,
            outgoing: true,
            body: Body::Text(text.clone()),
            sent_at: Timestamp::now(),
            delivery: Delivery::Sending,
            deleted: false,
            edited: false,
            // The deadline the arm promised, from this send's own moment. The server's clock
            // recomputes it at accept time; the difference is the same skew every other
            // receiver's countdown tolerates.
            expires_at: expires_in_ms
                .map(|lifetime| Timestamp::now().saturating_add_millis(i64::from(lifetime))),
        }));

        // The lifetime is sealed inside the content as well as carried on the wire: the wire
        // copy reaches only the server (which never echoes it), while this copy is what each
        // receiver's own countdown reads. One sender decision, two rides.
        let content = Content::Text {
            text,
            mentions: Vec::new(),
            expires_in_ms,
        };
        let plaintext = match content::encode(&content, true) {
            Ok(bytes) => bytes,
            Err(_) => return self.sink.send(Event::SendFailed { message_id }),
        };
        let Some((envelope, chain_id)) = self.envelope_for(conversation_id, &plaintext).await
        else {
            return self.sink.send(Event::SendFailed { message_id });
        };
        let message = migo_protocol::MessageSend {
            message_id,
            conversation_id,
            kind: MessageKind::Text,
            envelope,
            reply_to: None,
            expires_in_ms,
            sender_key_id: Some(chain_id),
        };
        self.request(Opcode::MessageSend, &message).await;
    }

    /// Everything a message needs after its content is encoded: the audience, the
    /// distributions that reach it, and the content seal.
    ///
    /// The shared body of every send path — text, an attachment's reference, a reaction —
    /// because the three differ only in what they seal, never in who it goes to or how.
    /// [`Self::send_text`] states the reasoning in full; this is that reasoning, once.
    ///
    /// Returns `None` when the message cannot be sealed now — an incomplete roster, a device
    /// list not yet fetched — each of which has already been reported as a toast, because the
    /// same sentences are worth reading whatever the message would have been. The caller
    /// fails its own optimistic row.
    async fn envelope_for(
        &mut self,
        conversation_id: Id,
        plaintext: &[u8],
    ) -> Option<(Vec<u8>, u32)> {
        let signed = self.signed.as_ref()?;
        let Some(cached) = signed.members.get(&conversation_id) else {
            // Never primed: the send path is only reached from a conversation the list or a
            // create seeded, but a defensive refusal beats sealing for no one.
            return None;
        };
        let incomplete = !cached.complete;
        if incomplete {
            let roster = migo_protocol::ConversationRosterRequest { conversation_id };
            self.pending_roster = Some(conversation_id);
            self.request(Opcode::ConversationRoster, &roster).await;
            self.sink.toast(
                "reading this group's full roster, try again in a moment",
                ToastKind::Info,
            );
            return None;
        }
        // The audience the SDK's `recipientDevices` computes: members ∪ this account (the account's
        // other devices must receive this message for sync), devices of each, minus this sending
        // device.
        // The `?` re-borrow is not redundant with the one at the top: the roster request
        // and the toasts between them took `&mut self`, so this arm needs its own take.
        let signed = self.signed.as_ref()?;
        let members: Vec<Id> = signed
            .members
            .get(&conversation_id)
            .map(|cached| cached.ids.clone())
            .unwrap_or_default();
        let mut audience: Vec<Id> = members;
        if !audience.contains(&signed.account.account_id) {
            audience.push(signed.account.account_id);
        }
        let my_device = signed.account.device_id;
        let mut targets: Vec<Id> = Vec::new();
        for user in &audience {
            match signed.devices.get(user) {
                Some(devices) => {
                    targets.extend(devices.iter().copied().filter(|id| *id != my_device));
                }
                None => {
                    // No device list yet. Ask for one; the message is retried when it arrives.
                    let request = migo_protocol::KeyBundleRequest {
                        user_id: *user,
                        device_id: None,
                    };
                    self.request(Opcode::KeyBundleFetch, &request).await;
                    self.sink.toast(
                        "fetching keys for this conversation, try again in a moment",
                        ToastKind::Info,
                    );
                    return None;
                }
            }
        }

        // The pairwise layer carries only distributions. Each one holds the chain key as of *now*,
        // so it must be taken before the content seal — handing it out after the first message
        // would gate that message out of the receiver's chain, which is the late-joiner property
        // applied to everyone.
        let signed = self.signed.as_mut()?;
        let distribution = signed.groups.distribution(conversation_id);
        for device in &targets {
            let signed = self.signed.as_mut()?;
            if !signed.groups.needs_distribution(conversation_id, *device) {
                continue;
            }
            let bundle = signed.bundles.get(device).cloned();
            let control = content::encode(
                &Content::ControlEvent {
                    event: "sender-key".to_owned(),
                    data: Some(distribution.clone()),
                },
                true,
            );
            let Ok(control) = control else {
                continue;
            };
            let envelope =
                match signed
                    .sessions
                    .seal(conversation_id, *device, bundle.as_ref(), &control)
                {
                    Ok(envelope) => envelope,
                    // A device whose bundle will not start a session is skipped, not fatal: the
                    // content message still reaches it only if a distribution did, and its next
                    // send re-offers one.
                    Err(_) => continue,
                };
            let Ok(bytes) = envelope.encode() else {
                continue;
            };
            let exchange = migo_protocol::MessageSend {
                message_id: Id::generate_at(Timestamp::now(), &mut OsRandom),
                conversation_id,
                kind: MessageKind::KeyExchange,
                envelope: bytes,
                reply_to: None,
                expires_in_ms: None,
                sender_key_id: None,
            };
            self.request(Opcode::MessageSend, &exchange).await;
            if let Some(signed) = self.signed.as_mut() {
                signed.groups.mark_distributed(conversation_id, *device);
            }
        }

        // One seal for the whole conversation, fanned out by the server to every device the
        // distribution reached. This is the entire point of the sender-key design — the
        // pairwise cost is paid per device once, not per message.
        let signed = self.signed.as_mut()?;
        match signed.groups.seal(conversation_id, plaintext) {
            Ok(sealed) => Some((sealed.envelope, sealed.chain_id)),
            Err(_) => None,
        }
    }

    /// Requests the public room directory, narrowed by a query when one is held.
    async fn request_rooms(&mut self, query: String) {
        let query = query.trim().to_owned();
        let message = RoomListRequest {
            limit: 50,
            query: (!query.is_empty()).then_some(query),
            category: None,
            language: None,
            country: None,
            cursor: None,
        };
        self.request(Opcode::RoomList, &message).await;
    }

    /// Creates a room and enters it. The wire's create call resolves with a join handle —
    /// creation is entry, the creator is the first member and its Owner — so the reply is
    /// handled by the join path and nothing here has a second flow to keep in step.
    async fn create_room(
        &mut self,
        slug: String,
        name: String,
        managed: bool,
        topic: Option<String>,
    ) {
        let message = RoomCreate {
            slug,
            name,
            kind: if managed { 2 } else { 1 },
            topic,
            max_members: None,
        };
        self.request(Opcode::RoomCreate, &message).await;
    }

    /// Leaves a room. The server closes the conversation for this account; the conversation list
    /// re-reads behind the acknowledgement, and the rooms pane drops the room from its joined set.
    async fn leave_room(&mut self, room_id: Id) {
        self.pending_leave = Some(room_id);
        let message = RoomLeaveRequest { room_id };
        self.request(Opcode::RoomLeave, &message).await;
    }

    /// Joins a room. The reply names both halves — the room and the conversation — and the
    /// conversation list re-reads behind it, exactly as a started direct chat does.
    async fn join_room(&mut self, room_id: Id) {
        let message = RoomJoinRequest {
            room_id,
            invite_code: None,
        };
        self.request(Opcode::RoomJoin, &message).await;
    }

    /// Requests the durable notification inbox.
    async fn request_notifications(&mut self) {
        // The server keeps no pagination cursor for the inbox, so the page is asked for plainly.
        let message = InboxReq {
            limit: 50,
            cursor: None,
        };
        self.request(Opcode::NotificationList, &message).await;
    }

    /// Marks every notification at or before one instant read.
    ///
    /// The wire carries an id rather than a timestamp, and the server reads the id's embedded time
    /// prefix as the watermark — so this synthesises an id whose prefix *is* the instant: the six
    /// leading bytes of the millisecond count, then zeros. It names an instant, not an entity.
    async fn acknowledge_alerts(&mut self, through_unix_ms: i64) {
        let ms = through_unix_ms.max(0) as u64;
        let mut bytes = [0u8; 16];
        bytes[0] = (ms >> 40) as u8;
        bytes[1] = (ms >> 32) as u8;
        bytes[2] = (ms >> 24) as u8;
        bytes[3] = (ms >> 16) as u8;
        bytes[4] = (ms >> 8) as u8;
        bytes[5] = ms as u8;
        let message = NotificationAck {
            id: migo_core::Id::from_bytes(bytes),
        };
        self.request(Opcode::NotificationAck, &message).await;
    }

    /// Fires the wallet's whole economy: six reads, each arriving as its own event.
    async fn request_wallet(&mut self) {
        self.request(Opcode::BalanceFetch, &WalletReq {}).await;
        let ledger = LedgerReq {
            limit: Some(10),
            cursor: None,
        };
        self.request(Opcode::LedgerHistory, &ledger).await;
        if let Some(signed) = self.signed.as_ref() {
            let me = signed.account.account_id;
            self.request(Opcode::Progression, &ProgressionReq { of_account: me })
                .await;
            self.request(Opcode::Badges, &BadgesReq { of_account: me })
                .await;
        }
        let board = LeaderboardReq {
            board: "xp".to_owned(),
            limit: Some(10),
        };
        self.request(Opcode::Leaderboard, &board).await;
        self.request(Opcode::GiftCatalogue, &GiftCatalogueReq {})
            .await;
    }

    /// Buys and delivers a gift. On acceptance the wallet re-reads, so the balance and the
    /// statement move to the server's arithmetic rather than a local guess.
    async fn send_gift(&mut self, sku: String, recipient: Id, client_key: Option<String>) {
        let message = GiftSend {
            gift: sku,
            recipient,
            conversation_id: None,
            client_key,
        };
        self.request(Opcode::GiftSend, &message).await;
    }

    /// Buys one Kick Point pack. On acceptance the wallet re-reads, so the new balance and the
    /// charge arrive as the server's arithmetic rather than a local guess — the same rule the
    /// gift buy follows.
    async fn buy_kick_points(&mut self, pack_kp: u32, client_key: String) {
        let message = KickPointsBuy {
            pack_kp,
            client_key,
        };
        self.request(Opcode::KickPointsBuy, &message).await;
    }

    /// Searches public profiles by username prefix.
    async fn search_people(&mut self, query: String) {
        let message = SearchReq {
            query: query.trim().to_owned(),
            limit: Some(10),
        };
        self.request(Opcode::Search, &message).await;
    }

    /// Asks the social graph for its own suggestions.
    async fn request_suggestions(&mut self) {
        let message = SuggestReq { limit: Some(8) };
        self.request(Opcode::Suggestions, &message).await;
    }

    /// The room directory came back: reduce it to rows.
    fn on_rooms(&mut self, frame: &migo_protocol::Frame) {
        let Ok(response) = gateway::decode::<migo_protocol::RoomListResponse>(frame) else {
            return;
        };
        let rows = response
            .rooms
            .into_iter()
            .map(|room| RoomRow {
                room_id: room.room_id,
                name: room.name,
                topic: room.topic.filter(|topic| !topic.is_empty()),
                member_count: room.member_count,
                online_count: room.online_count,
                category: room.category,
                verified: room.verified.unwrap_or(false),
            })
            .collect();
        self.sink.send(Event::Rooms(rows));
    }

    /// A join was accepted: watch the room's own topic, note the conversation's (empty) member
    /// set, re-read the list, and tell the UI which thread to open.
    ///
    /// The room topic is the join's other half. The conversation topic (subscribed by the list
    /// read below) carries the messages; the room topic carries who is in the room — the member
    /// and state events the notices and the live counts are drawn from. Without it a joined room
    /// shows its opening snapshot forever, silently.
    async fn on_room_joined(&mut self, frame: &migo_protocol::Frame) {
        let Ok(joined) = gateway::decode::<migo_protocol::RoomJoinResponse>(frame) else {
            return;
        };
        let mut to_watch: Vec<Id> = Vec::new();
        if let Some(signed) = self.signed.as_mut() {
            // A room's member set is served by the roster, not the join; the empty set here only
            // seeds the map so a send before the list re-reads does not mis-address. Seeded
            // incomplete — empty is the most truncated a preview can be — so the first send
            // reads the roster rather than sealing for nobody.
            signed.members.entry(joined.conversation_id).or_default();
            signed
                .room_conversations
                .insert(joined.room.room_id, joined.conversation_id);
            if signed.rooms_watched.insert(joined.room.room_id) {
                to_watch.push(joined.room.room_id);
            }
        }
        // The bridge is written the moment the join names it: the join's answer is the one wire
        // moment that states the pair, and the next process's restart rehydration reads this
        // file for exactly these ids.
        self.persist_room_bridges();
        self.sink.send(Event::RoomJoined {
            conversation_id: joined.conversation_id,
            room_id: joined.room.room_id,
            title: joined.room.name,
        });
        self.watch_topics(TopicKind::Room, to_watch).await;
        self.request_conversations().await;
    }

    /// A leave was accepted: stop watching the room's topic, drop the rooms pane's entry, and
    /// re-read the list so the closed conversation stops being offered.
    ///
    /// The room's crypto state goes with it: the membership that authorised this device's chain
    /// and its receiver states is gone, and a re-join must start fresh chains rather than
    /// re-using keys the departed members may still hold.
    async fn on_room_left(&mut self, frame: &migo_protocol::Frame) {
        let Ok(acknowledged) = gateway::decode::<migo_protocol::Acknowledged>(frame) else {
            return;
        };
        let Some(room_id) = self.pending_leave.take() else {
            return;
        };
        if !acknowledged.ok {
            return;
        }
        // The conversation id the room maps to, remembered from the join: the leave ack names
        // only the room, but the crypto state is keyed by conversation.
        let conversation_id = self.forget_room(room_id);
        self.sink.send(Event::RoomLeft {
            room_id,
            conversation_id,
            self_left: true,
        });
        self.request_conversations().await;
    }

    /// Drops every piece of worker state a room owns: the topic watch, the room-to-conversation
    /// bridge, and — when the bridge still knew the conversation — the crypto state keyed under
    /// it (the outbound sender-key chain, the pairwise ratchets, the messages held awaiting a
    /// distribution). The room twin of [`Self::forget_group`], read through the bridge map
    /// because a room's crypto state is keyed by the conversation that carries its messages.
    ///
    /// Returns the conversation the room mapped to, so the caller can tell the UI which thread
    /// to close; `None` when the map had already forgotten the room.
    fn forget_room(&mut self, room_id: Id) -> Option<Id> {
        let conversation = self
            .signed
            .as_ref()
            .and_then(|signed| signed.room_conversations.get(&room_id).copied());
        if let Some(signed) = self.signed.as_mut() {
            signed.rooms_watched.remove(&room_id);
            signed.room_conversations.remove(&room_id);
            if let Some(conversation) = conversation {
                // The tracked conversation topic goes with the room's own: the server revoked
                // both the moment the leave landed, and a set that kept the id would re-ask
                // (and be refused) on the next reconnect. The wire UNSUBSCRIBE is not sent —
                // the revocation already happened server-side, and this is the local book.
                signed.conversations_watched.remove(&conversation);
                signed.groups.forget(conversation);
                signed.sessions.forget(conversation, None);
                signed.pending.retain(|(id, _), _| *id != conversation);
            }
        }
        // The bridge forgets with the room, on every path that ends one — the leave's ack,
        // another device's departure, a kick, a ban — so a restart does not re-subscribe a
        // topic the account can no longer authorize.
        self.persist_room_bridges();
        conversation
    }

    /// Writes the account's room bridge to disk, best-effort, beside the vault.
    ///
    /// Called from the two moments the bridge moves — a join naming a new pair, and any path
    /// that forgets a room — so the next process reads back exactly the rooms this one held.
    /// The store's own contract covers the failure modes: a write that fails is skipped
    /// quietly, and the next join or leave is the retry.
    fn persist_room_bridges(&self) {
        let Some(signed) = self.signed.as_ref() else {
            return;
        };
        let bridges: Vec<(Id, Id)> = signed
            .room_conversations
            .iter()
            .map(|(room, conversation)| (*room, *conversation))
            .collect();
        self.room_bridges.save(signed.account.account_id, &bridges);
    }

    /// The join-bell's membership probe answered: the roster read only succeeds for a member,
    /// so a room that answers is a room this account is still in — re-join it, idempotently,
    /// and let `on_room_joined` run the join's other half (the room topic watch, the room to
    /// conversation bridge, the list re-read). A refusal arrives as an error frame and fails
    /// the decode below, which drops the probe just as quietly: a bell for a room the account
    /// has since left is not this device's to act on.
    async fn on_room_probe(&mut self, frame: &migo_protocol::Frame) {
        let Some(room_id) = self.pending_room_probe.take() else {
            return;
        };
        if gateway::decode::<migo_protocol::RosterResponse>(frame).is_err() {
            return;
        }
        self.join_room(room_id).await;
    }

    /// A member event off a watched room's topic — or off the account's own user topic, the
    /// join-bell: someone came, went, dropped, or was removed.
    ///
    /// Forwarded whole rather than reduced to a sentence here: the display name is a profile
    /// fetch away and belongs with the chat pane's other name lookups, and the change enum and
    /// member total are the two facts both a notice line and a live count are drawn from.
    ///
    /// A removal naming *this* account is the kicked member's one last frame — the server
    /// publishes the event and then takes the room's topics away — so it ends the room for this
    /// device the same way a leave's ack does: the keys go, the window's copy of the thread
    /// goes, and the list re-reads so the row stops being offered. Without this the room would
    /// sit in the list with a composer whose every send can only fail server-side (audit area
    /// 4: the kicked member must lose the surface, not just the delivery).
    ///
    /// The bell is the join's other half, and rooms owe conversations the same one. A `Joined`
    /// naming *this* account, for a room this session has never heard of, is the one frame the
    /// server sends a member who cannot be a subscriber of the room yet — the join happened on
    /// another device, and this session learns the room exists from it. The reaction is the
    /// join flow re-run behind a membership probe (see `pending_room_probe`): the re-join is
    /// idempotent — an already-member is answered with the full handle and the room hears
    /// nothing — and `on_room_joined` does the rest, so this device hears everything the room
    /// says next.
    async fn on_room_member(&mut self, frame: &migo_protocol::Frame) {
        let Ok(event) = gateway::decode::<migo_protocol::RoomMemberEvent>(frame) else {
            return;
        };
        let change = event
            .change
            .filter(|change| *change != migo_protocol::MemberChange::Unknown)
            .unwrap_or(if event.joined {
                migo_protocol::MemberChange::Joined
            } else {
                migo_protocol::MemberChange::Left
            });
        // The join-bell, run before the folds below: they key off rooms this session knows,
        // and the bell is precisely the room it does not.
        if change == migo_protocol::MemberChange::Joined {
            let known = self.signed.as_ref().is_some_and(|signed| {
                signed.rooms_watched.contains(&event.room_id)
                    || signed.room_conversations.contains_key(&event.room_id)
            });
            let self_join = self
                .signed
                .as_ref()
                .is_some_and(|signed| signed.account.account_id == event.user_id);
            if self_join && !known && self.pending_room_probe.is_none() {
                self.pending_room_probe = Some(event.room_id);
                let probe = migo_protocol::RosterReq {
                    room_id: event.room_id,
                    limit: Some(1),
                    after: None,
                };
                self.request(Opcode::RoomRoster, &probe).await;
            }
        }
        // A departure from a room this session is in kills the room's outbound chain: the member
        // who left may still hold its key, and the one thing a chain must not do after a member
        // leaves is keep sealing. The next send builds a fresh chain and distributes it to
        // everyone remaining. A member *joining* needs no rotation — the chain key they are
        // handed starts at the current position, so history stays sealed to them.
        if matches!(
            change,
            migo_protocol::MemberChange::Left
                | migo_protocol::MemberChange::Disconnected
                | migo_protocol::MemberChange::Kicked
                | migo_protocol::MemberChange::Banned
        ) {
            if let Some(signed) = self.signed.as_mut() {
                if let Some(conversation) = signed.room_conversations.get(&event.room_id) {
                    signed.groups.rotate(*conversation);
                }
            }
        }
        // The event is also the conversation's membership moving: without this fold, a member
        // who joins a room after this client did never lands in its audience, and the next
        // send's sender key is sealed for a membership the room has already outgrown — the
        // desktop twin of the bug the SDK and Android fixed. The bridge map names the
        // conversation; the cache patch is the same one a `CONVERSATION_MEMBER_EVENT` applies.
        if let Some(signed) = self.signed.as_mut() {
            if let Some(conversation) = signed.room_conversations.get(&event.room_id) {
                if let Some(cached) = signed.members.get_mut(conversation) {
                    cached.apply_change(event.user_id, change);
                }
            }
        }
        // This account's own departure. The wire's `Left` reaches this device only when another
        // device of the same account left (the acting socket is excluded from the fan-out), so
        // it is the multi-device twin of the leave button's own teardown rather than a second
        // copy of it; `Kicked` and `Banned` are the paths with no local echo at all.
        if matches!(
            change,
            migo_protocol::MemberChange::Left
                | migo_protocol::MemberChange::Kicked
                | migo_protocol::MemberChange::Banned
        ) && self
            .signed
            .as_ref()
            .is_some_and(|signed| signed.account.account_id == event.user_id)
        {
            let room_id = event.room_id;
            let conversation_id = self.forget_room(room_id);
            self.sink.send(Event::RoomLeft {
                room_id,
                conversation_id,
                self_left: change == migo_protocol::MemberChange::Left,
            });
            self.request_conversations().await;
            return;
        }
        self.sink.send(Event::RoomMember {
            room_id: event.room_id,
            user_id: event.user_id,
            change,
            member_count: event.member_count,
        });
    }

    /// A conversation's roster arrived: the whole membership, where a list row carried only a
    /// preview. This is the answer the send path asks for when its cached membership is
    /// incomplete — the promotion that makes the next send's audience the truth.
    ///
    /// The departed are filtered out (`left_at` is the roster's own word for "no longer in the
    /// group"), and the active ids replace the preview wholesale rather than merging into it: a
    /// merge would keep a preview's truncation and call the result complete.
    fn on_roster(&mut self, frame: &migo_protocol::Frame) {
        let Ok(response) = gateway::decode::<migo_protocol::ConversationRosterResponse>(frame)
        else {
            return;
        };
        // Which conversation the answer names: the request carried the id, but the reply does
        // not repeat it. Two askers can be waiting — the send path's single-slot ask
        // (`pending_roster`) and the roster panel's asks (`group_rosters`) — and both are
        // answered by this one frame, because both asked the same wire the same question.
        let send_path_subject = self.pending_roster.take();
        let panel_subjects: Vec<Id> = self.group_rosters.keys().copied().collect();
        let subjects = send_path_subject.into_iter().chain(panel_subjects);
        let active: Vec<Id> = response
            .entries
            .iter()
            .filter(|entry| entry.left_at.is_none())
            .map(|entry| entry.account_id)
            .collect();
        let mut panel_event: Option<(Id, Vec<crate::model::RosterMember>)> = None;
        // The panel's copy, reduced once for whichever asker wants it: every field the roster
        // panel draws, in the server's own order. Built outside the loop — the loop's subjects
        // share one answer, and the answer's subject is whichever of them was the panel's.
        let panel_rows: Vec<crate::model::RosterMember> = response
            .entries
            .iter()
            .map(|entry| crate::model::RosterMember {
                account_id: entry.account_id,
                role: entry.role,
                joined_at: entry.joined_at,
                muted_until: entry.muted_until,
                left_at: entry.left_at,
            })
            .collect();
        for conversation_id in subjects {
            let Some(signed) = self.signed.as_mut() else {
                return;
            };
            if let Some(cached) = signed.members.get_mut(&conversation_id) {
                cached.promote(active.clone());
            }
            if panel_event.is_none() && self.group_rosters.remove(&conversation_id).is_some() {
                panel_event = Some((conversation_id, panel_rows.clone()));
            }
        }
        if let Some((conversation_id, members)) = panel_event {
            self.sink.send(Event::GroupRoster {
                conversation_id,
                members,
            });
        }
        // The send that asked for this roster failed on purpose (the audience was not knowable
        // yet); the UI's retry affordance — the same one a missing device list surfaces — sends
        // it again now that the membership is whole.
    }

    /// A member event off a watched conversation's topic: someone joined, left, or was removed.
    ///
    /// The room twin of this event rotates the chain on a departure; the conversation twin
    /// cannot yet — it names no room, and the rotation the group path performs lives behind
    /// `room_conversations`. What it always carries is the one fact the send audience is built
    /// from, so the membership cache is patched here, exactly the way the SDK's
    /// `applyMemberEvent` patches its own.
    async fn on_conversation_member(&mut self, frame: &migo_protocol::Frame) {
        let Ok(event) = gateway::decode::<migo_protocol::ConversationMemberEvent>(frame) else {
            return;
        };
        if let Some(signed) = self.signed.as_mut() {
            if let Some(cached) = signed.members.get_mut(&event.conversation_id) {
                cached.apply(&event);
            }
        }
        // Section 163's first trigger: a membership change is a change of audience, and the
        // audience is exactly what a sender key is sealed for. The chain rotates to the event's
        // own `group_key_epoch` — the generation the server's own membership operation produced,
        // so every member that rotates on the same event names the same epoch — and the new
        // chain goes out to every member device, one pairwise distribution each, before the
        // next message of the conversation could be sealed under a key its new audience lacks.
        if is_membership_change(event.change) {
            // This account's own departure ends the group for this device the way a leave's
            // ack does: the keys go, the window's copy of the thread goes, and the member
            // event stops arriving the moment the subscription dies with the membership. The
            // rotation still happens — the held chain belongs to an audience this account has
            // left, and a group state must not survive its membership — but there is no one
            // this device may distribute to anymore, so the redistribution is skipped.
            let own_departure = self.signed.as_ref().is_some_and(|signed| {
                matches!(
                    event.change,
                    migo_protocol::MemberChange::Left
                        | migo_protocol::MemberChange::Kicked
                        | migo_protocol::MemberChange::Banned
                ) && event.user_id == signed.account.account_id
            });
            if let Some(signed) = self.signed.as_mut() {
                signed
                    .groups
                    .rotate_on_membership(event.conversation_id, event.group_key_epoch);
            }
            if own_departure {
                let conversation_id = event.conversation_id;
                self.forget_group(conversation_id);
                self.sink.send(Event::GroupLeft { conversation_id });
            } else {
                self.redistribute_group_key(event.conversation_id).await;
            }
        }
        self.sink.send(Event::GroupMember {
            conversation_id: event.conversation_id,
            user_id: event.user_id,
            change: event.change,
        });
    }

    /// One member's sealed copy of the group's rotated sender-key chain, addressed to one
    /// device of one account: section 163's redistribution frame. The server forwards it to
    /// the target account's user topic, where the target account's every device — the named
    /// one included — can see it, but only the named device can open it.
    async fn redistribute_group_key(&mut self, conversation_id: Id) {
        // The audience is read under one borrow and then let go, because the sends below need
        // the worker back — the same re-take discipline `envelope_for` follows for the same
        // reason.
        let plan = {
            let Some(signed) = self.signed.as_ref() else {
                return;
            };
            let Some(cached) = signed.members.get(&conversation_id) else {
                return;
            };
            // An incomplete roster is the preview the conversation list seeded; a
            // redistribution built on it would reach a prefix of the group and the rest would
            // keep sealing under the chain that just died. This event's redistribution is
            // skipped rather than half-made — the honest limit of a preview — and the full
            // roster is requested below so the *next* membership change finds it complete
            // (and the next send's own distribution pass reaches anyone this one missed,
            // because the rotation cleared `distributed`).
            if !cached.complete {
                None
            } else {
                let my_device = signed.account.device_id;
                // The audience of `envelope_for`: members ∪ this account (a distribution is
                // also this account's own sync), devices of each, minus this sending device.
                let mut audience: Vec<Id> = cached.ids.clone();
                let my_account = signed.account.account_id;
                if !audience.contains(&my_account) {
                    audience.push(my_account);
                }
                let mut targets: Vec<(Id, Id)> = Vec::new();
                let mut missing: Vec<Id> = Vec::new();
                for user in &audience {
                    match signed.devices.get(user) {
                        Some(devices) => {
                            targets.extend(
                                devices
                                    .iter()
                                    .copied()
                                    .filter(|id| *id != my_device)
                                    .map(|device| (*user, device)),
                            );
                        }
                        None => missing.push(*user),
                    }
                }
                Some((my_device, targets, missing))
            }
        };
        let Some((my_device, targets, missing)) = plan else {
            let roster = migo_protocol::ConversationRosterRequest { conversation_id };
            self.request(Opcode::ConversationRoster, &roster).await;
            return;
        };
        if !missing.is_empty() {
            // Same patience as the send path, minus the toast: this is a background
            // redistribution the person never asked for, and its retry is the next event's
            // own redistribution rather than a press they would have to make again.
            for user in missing {
                let request = migo_protocol::KeyBundleRequest {
                    user_id: user,
                    device_id: None,
                };
                self.request(Opcode::KeyBundleFetch, &request).await;
            }
            return;
        }
        // The distribution is taken once, as of now, and every device gets the same bytes.
        let Some(distribution) = self
            .signed
            .as_mut()
            .map(|signed| signed.groups.distribution(conversation_id))
        else {
            return;
        };
        for (to_account, to_device) in targets {
            let Some(sealed) = self
                .seal_group_distribution(conversation_id, to_account, to_device, &distribution)
                .await
            else {
                continue;
            };
            let frame = migo_protocol::GroupKeyDistribution {
                conversation_id,
                from_device: my_device,
                to_account,
                to_device,
                sealed_distribution: sealed,
            };
            self.request(Opcode::GroupKeyDistribute, &frame).await;
        }
    }

    /// Seals one member device's copy of a sender-key distribution over the pairwise session,
    /// marking it distributed when the seal succeeds. Returns the sealed bytes, or `None` when
    /// this device cannot seal for that target yet — a bundle that has not arrived, which the
    /// next membership event or send retries once the fetch answers.
    async fn seal_group_distribution(
        &mut self,
        conversation_id: Id,
        to_account: Id,
        device: Id,
        distribution: &[u8],
    ) -> Option<Vec<u8>> {
        let signed = self.signed.as_mut()?;
        if !signed.groups.needs_distribution(conversation_id, device) {
            return None;
        }
        let bundle = signed.bundles.get(&device).cloned();
        let control = content::encode(
            &Content::ControlEvent {
                event: "sender-key".to_owned(),
                data: Some(distribution.to_vec()),
            },
            true,
        );
        let Ok(control) = control else {
            return None;
        };
        let envelope = signed
            .sessions
            .seal(conversation_id, device, bundle.as_ref(), &control);
        let Ok(envelope) = envelope else {
            // No session and no bundle for this device: fetch the bundle so the next event's
            // redistribution can reach it. This one is skipped, not fatal — the same
            // patience `envelope_for` shows, and for the same reason.
            let request = migo_protocol::KeyBundleRequest {
                user_id: to_account,
                device_id: Some(device),
            };
            self.request(Opcode::KeyBundleFetch, &request).await;
            return None;
        };
        let Ok(bytes) = envelope.encode() else {
            return None;
        };
        if let Some(signed) = self.signed.as_mut() {
            signed.groups.mark_distributed(conversation_id, device);
        }
        Some(bytes)
    }

    /// A group's metadata moved as a delta: the only field on this wire today is the title,
    /// and a present one is a rename the group's members hear. The conversation list's row is
    /// patched by the shell, off the event below — the worker holds no rows, it only reduces
    /// the wire — so the member who renamed is heard by every member whose window is open,
    /// not only the one whose list re-read happens to land next.
    fn on_conversation_state(&mut self, frame: &migo_protocol::Frame) {
        let Ok(event) = gateway::decode::<migo_protocol::ConversationStateEvent>(frame) else {
            return;
        };
        if let Some(title) = event.title {
            self.sink.send(Event::GroupRenamed {
                conversation_id: event.conversation_id,
                title,
            });
        }
    }

    /// This account's own vote landed: the tally as the caller sees it, straight off the
    /// reply the request was owed. The reply names neither conversation nor target, so both
    /// are read back out of the remembered ask. `open: false` is the moment the vote carried —
    /// the member event for the removal follows separately, and the UI's tally line retires
    /// when it does.
    fn on_group_vote_reply(&mut self, frame: &migo_protocol::Frame) {
        let Ok(response) = gateway::decode::<migo_protocol::ConversationVoteKickResponse>(frame)
        else {
            return;
        };
        let Some((conversation_id, target_id)) = self.pending_vote.take() else {
            return;
        };
        self.sink.send(Event::GroupVoteStatus {
            conversation_id,
            target_id,
            votes: response.votes,
            needed: response.needed,
            member_count: response.member_count,
            open: response.open,
        });
    }

    /// A kick vote's tally, for everyone the vote concerns: the same numbers the voter's own
    /// reply carries, arriving as the event the fan-out publishes. `closed` retires a tally a
    /// client was still drawing.
    fn on_group_vote_event(&mut self, frame: &migo_protocol::Frame) {
        let Ok(event) = gateway::decode::<migo_protocol::ConversationVoteEvent>(frame) else {
            return;
        };
        self.sink.send(Event::GroupVoteBroadcast {
            conversation_id: event.conversation_id,
            target_id: event.target_id,
            votes: event.votes,
            needed: event.needed,
            member_count: event.member_count,
            closed: event.closed,
        });
    }

    /// A rename's reply: the refreshed summary, the same shape a create answers with. The
    /// list re-read that follows is what carries the new title to the row; other members hear
    /// it through the state event. A toast says it took, because a rename that failed silently
    /// would leave the renamer typing it again to find out.
    async fn on_conversation_update_reply(&mut self, frame: &migo_protocol::Frame) {
        let Ok(summary) = gateway::decode::<migo_protocol::ConversationSummary>(frame) else {
            return;
        };
        if let Some(title) = &summary.title {
            self.sink
                .toast(format!("Group renamed to {title}"), ToastKind::Success);
        }
        self.request_conversations().await;
    }

    /// A group leave's acknowledgement: the bare ack names nothing, so the conversation the
    /// request carried is read back out of the remembered ask. Everything this device held
    /// under the conversation — the sender-key chain, the ratchets, the held messages — goes
    /// before the event crosses the channel, the same order a sign-out follows for the same
    /// reason: state must not outlive the keys it was sealed with.
    async fn on_group_left(&mut self, frame: &migo_protocol::Frame) {
        let Ok(acknowledged) = gateway::decode::<migo_protocol::Acknowledged>(frame) else {
            return;
        };
        if !acknowledged.ok {
            return;
        }
        let Some(conversation_id) = self.group_leave.take() else {
            return;
        };
        self.forget_group(conversation_id);
        self.sink.send(Event::GroupLeft { conversation_id });
        self.request_conversations().await;
    }

    /// Drops every piece of worker state a group conversation owns: the outbound sender-key
    /// chain, the pairwise ratchets, the messages held awaiting a distribution, the caches,
    /// and the watch. The group twin of the room leave's teardown — the same contents, keyed
    /// by the conversation itself because a group has no room id to map through.
    fn forget_group(&mut self, conversation_id: Id) {
        self.group_rosters.remove(&conversation_id);
        if self.group_leave == Some(conversation_id) {
            self.group_leave = None;
        }
        // The group's call seat goes with its keys: the membership that authorised the seat
        // is gone, and a call about a conversation this device no longer belongs to has no
        // reason to keep ringing in the header.
        if self.forget_group_call(conversation_id) {
            self.sink.send(Event::GroupCallEnded { conversation_id });
        }
        if let Some(signed) = self.signed.as_mut() {
            signed.groups.forget(conversation_id);
            signed.sessions.forget(conversation_id, None);
            signed.pending.retain(|(id, _), _| *id != conversation_id);
            signed.members.remove(&conversation_id);
            signed.e2e.remove(&conversation_id);
            signed.conversations_watched.remove(&conversation_id);
        }
    }

    /// A state event off a watched room's topic: the counters moved. A delta, folded — never a
    /// snapshot — so an event that carries only the online tally leaves the member total alone.
    fn on_room_state(&mut self, frame: &migo_protocol::Frame) {
        let Ok(event) = gateway::decode::<migo_protocol::RoomStateEvent>(frame) else {
            return;
        };
        self.sink.send(Event::RoomState {
            room_id: event.room_id,
            online_count: event.online_count,
            member_count: event.member_count,
        });
    }

    /// The inbox came back: reduce it to rows.
    fn on_alerts(&mut self, frame: &migo_protocol::Frame) {
        let Ok(response) = gateway::decode::<migo_protocol::InboxResponse>(frame) else {
            return;
        };
        let rows = response
            .items
            .into_iter()
            .map(|item| AlertRow {
                id: item.id,
                kind: item.kind,
                title: item.title.filter(|title| !title.is_empty()),
                at: item.at,
            })
            .collect();
        self.sink.send(Event::Alerts(rows));
    }

    /// A notification was pushed. The push is droppable by design and carries no plaintext, so it
    /// is a cue to re-read, never a row: the event says "look again" and nothing more.
    fn on_alert_pushed(&mut self, frame: &migo_protocol::Frame) {
        let Ok(_event) = gateway::decode::<migo_protocol::NotificationEvent>(frame) else {
            return;
        };
        self.sink.send(Event::AlertPushed);
    }

    /// A game event was pushed: one delta from the referee, handed to the games pane's feed.
    ///
    /// The decode is the whole job. The pane owns no board to patch, because a delta is not a
    /// state: a client that wants the score asks GAME_VIEW, and the feed only says that the game
    /// moved.
    fn on_game_event(&mut self, frame: &migo_protocol::Frame) {
        let Ok(event) = gateway::decode::<migo_protocol::GameEvent>(frame) else {
            return;
        };
        self.sink.send(Event::GamePushed {
            conversation_id: event.room_id,
            game_id: event.game_id,
            event: event.event,
            actor_id: event.actor_id,
            state_version: event.state_version,
        });
    }

    /// An economy event was pushed: the cue to re-read the wallet. See [`Event::EconomyPushed`].
    fn on_economy_pushed(&mut self, frame: &migo_protocol::Frame) {
        let Ok(_event) = gateway::decode::<migo_protocol::EconomyEvent>(frame) else {
            return;
        };
        self.sink.send(Event::EconomyPushed);
    }

    /// The wallet came back.
    fn on_balance(&mut self, frame: &migo_protocol::Frame) {
        let Ok(wallet) = gateway::decode::<migo_protocol::WalletView>(frame) else {
            return;
        };
        self.sink.send(Event::Balance {
            coins: wallet.balance,
            points: wallet.points,
            kick_points: wallet.kick_points,
        });
    }

    /// The statement came back: the sign comes from each line's reason, never its amount.
    fn on_ledger(&mut self, frame: &migo_protocol::Frame) {
        let Ok(response) = gateway::decode::<migo_protocol::LedgerResponse>(frame) else {
            return;
        };
        let rows = response
            .entries
            .into_iter()
            .map(|entry| LedgerRow {
                credit: model::ledger_credit(&entry.reason),
                reason: entry.reason,
                amount: entry.amount,
                balance_after: entry.balance_after,
                at: entry.at,
            })
            .collect();
        self.sink.send(Event::Ledger(rows));
    }

    /// The progression came back. The wire names the account it ranks, so the reply sorts
    /// itself: the caller's own standing files for the wallet, another account's answers the
    /// member view's ask — and the ask is spent with the reply, matched or not, so a stale
    /// ask cannot claim a later page.
    fn on_progression(&mut self, frame: &migo_protocol::Frame) {
        let Ok(wire) = gateway::decode::<migo_protocol::ProgressionWire>(frame) else {
            return;
        };
        let progression = Progression {
            level: wire.level,
            xp: wire.xp,
            xp_into_level: wire.xp_into_level,
            xp_for_next_level: wire.xp_for_next_level,
        };
        if let Some((conversation_id, asked)) = self.pending_member_progression {
            if asked == wire.account_id {
                self.pending_member_progression = None;
                self.sink.send(Event::MemberProgression {
                    conversation_id,
                    progression,
                });
                return;
            }
        }
        // The wallet's card files only the caller's own standing: a reply the member view
        // asked for must not overwrite the caller's level with somebody else's.
        let me = self.signed.as_ref().map(|signed| signed.account.account_id);
        if me == Some(wire.account_id) {
            self.sink.send(Event::ProgressionArrived(progression));
        }
    }

    /// The badges came back, by code and day. The reply names no account, so the member view's
    /// ask — when one stands — is spent on the next badge frame: the one race the pattern
    /// allows (a wallet refresh firing in the same round trip), and the same trade every
    /// reply-without-a-name in this worker already makes.
    fn on_badges(&mut self, frame: &migo_protocol::Frame) {
        let Ok(response) = gateway::decode::<migo_protocol::BadgesResponse>(frame) else {
            return;
        };
        if let Some((conversation_id, _asked)) = self.pending_member_badges.take() {
            let badges = response
                .badges
                .into_iter()
                .map(|badge| crate::model::BadgeRow {
                    code: badge.badge_code,
                    awarded_at: badge.awarded_at,
                })
                .collect();
            self.sink.send(Event::MemberBadges {
                conversation_id,
                badges,
            });
            return;
        }
        let codes = response
            .badges
            .into_iter()
            .map(|badge| badge.badge_code)
            .collect();
        self.sink.send(Event::Badges(codes));
    }

    /// The leaderboard came back. A member rank ask spends itself on the page — the position
    /// the asked account holds, or the honest `None` of standing off the first page — and
    /// every other page files for the wallet's board.
    fn on_leaderboard(&mut self, frame: &migo_protocol::Frame) {
        let Ok(response) = gateway::decode::<migo_protocol::LeaderboardResponse>(frame) else {
            return;
        };
        if let Some((conversation_id, asked)) = self.pending_member_rank.take() {
            let position = response
                .ranks
                .iter()
                .find(|rank| rank.account_id == asked)
                .map(|rank| rank.position);
            self.sink.send(Event::MemberRank {
                conversation_id,
                position,
            });
            return;
        }
        let rows = response
            .ranks
            .into_iter()
            .map(|rank| LeaderRow {
                position: rank.position,
                account_id: rank.account_id,
                xp: rank.xp,
                level: rank.level,
            })
            .collect();
        self.sink.send(Event::Leaderboard(rows));
    }

    /// The entitlements came back: the catalogue codes the account owns, for the composer's
    /// picker. Codes this client's pack table cannot render are kept anyway — ownership is a
    /// server fact, and renderability is the picker's own cut to make.
    fn on_entitlements(&mut self, frame: &migo_protocol::Frame) {
        let Ok(response) = gateway::decode::<migo_protocol::EntitlementsResponse>(frame) else {
            return;
        };
        let skus = response.items.into_iter().map(|item| item.sku).collect();
        self.sink.send(Event::Entitlements(skus));
    }

    /// The gift catalogue came back.
    fn on_gifts(&mut self, frame: &migo_protocol::Frame) {
        let Ok(response) = gateway::decode::<migo_protocol::GiftCatalogueResponse>(frame) else {
            return;
        };
        let rows = response
            .gifts
            .into_iter()
            .map(|gift| GiftRow {
                sku: gift.sku,
                name: gift.name,
                price: gift.price,
                category: gift.category,
            })
            .collect();
        self.sink.send(Event::Gifts(rows));
    }

    /// A gift was sent (or refused): toast the outcome, then re-read the money-side facts.
    async fn on_gift_sent(&mut self, frame: &migo_protocol::Frame) {
        let Ok(result) = gateway::decode::<migo_protocol::GiftSendResult>(frame) else {
            return;
        };
        if result.ok {
            // A duplicate is the first send returned again: the gift stands, nothing was charged
            // twice — say the fact rather than the mechanism.
            if result.duplicate == Some(true) {
                self.sink.toast("Gift already sent", ToastKind::Success);
            } else {
                self.sink.toast("Gift sent", ToastKind::Success);
            }
            self.request_wallet().await;
        } else {
            // The server judged the send — an unknown gift, an unroutable recipient, a balance
            // that does not cover it — and refused it. The wire carries only the refusal, not
            // the reason, so the toast says the fact it can stand behind; a silent refusal
            // would leave a closed picker and no word about why nothing arrived.
            self.sink
                .toast("The server refused the gift send", ToastKind::Error);
        }
    }

    /// A Kick Point pack was bought (or refused): toast the outcome, then re-read the wallet, so
    /// the balance the surface shows moves to the server's arithmetic. A refusal — most often a
    /// coin balance that does not cover the pack — arrives as an error frame, which the loop's
    /// own refusal arm toasts before this handler could; this arm sees only the accepted buys.
    async fn on_kick_points_bought(&mut self, frame: &migo_protocol::Frame) {
        let Ok(result) = gateway::decode::<migo_protocol::KickPointsBuyResult>(frame) else {
            return;
        };
        // A duplicate is the first buy returned again: the pack stands, nothing was charged
        // twice — say the fact rather than the mechanism.
        if result.duplicate {
            self.sink
                .toast("Kick Points already bought", ToastKind::Success);
        } else {
            self.sink.toast("Kick Points bought", ToastKind::Success);
        }
        self.request_wallet().await;
    }

    /// People came back, from search or suggestions: one event for both, because a row cannot tell
    /// them apart and neither can the screen that draws it.
    fn on_people(&mut self, frame: &migo_protocol::Frame) {
        let Ok(response) = gateway::decode::<migo_protocol::SearchResponse>(frame) else {
            return;
        };
        let rows = response
            .results
            .into_iter()
            .map(|person| PersonRow {
                account_id: person.account_id,
                username: person.username,
                display_name: person.display_name,
                mutual_friends: person.mutual_friends,
            })
            .collect();
        self.sink.send(Event::People(rows));
    }

    /// Signs out: forgets the keys locally first, then tells the server.
    ///
    /// The order matters. Local first means an unreachable server cannot leave a signed-out client
    /// still holding the material to decrypt its history.
    async fn sign_out(&mut self) {
        if let Some(gateway) = self.gateway.take() {
            gateway.close().await;
        }
        if let Some(mut signed) = self.signed.take() {
            signed.sessions.clear();
            let _ = signed
                .rest
                .logout(&signed.access_token, signed.account.session_id)
                .await;
        }
        self.retry = None;
        // A call cannot survive the keys that sealed it: rings, placements, and any live call
        // all end now, without a wire message — the gateway is already gone above, and the
        // server tears the session down on its own.
        self.calls_sign_out();
        // The group-call seat is the same story with more seats: the frame key it shares with
        // the roster was sealed under keys this session minted.
        self.group_calls_sign_out();
        // Media cannot survive them either: uploads in flight have no session to commit
        // under, fetches no keys to open with, and a recording no conversation to land in.
        // The abandoned tickets die on their own expiry; the pumps stop with their handles.
        self.attachment_begins.clear();
        self.attachment_commits.clear();
        self.media_wants.clear();
        self.media_fetching.clear();
        self.media_cache.clear();
        self.recording = None;
        self.playing = None;
        // The listened marks go with the account that earned them: they are this account's
        // own memory, and the next sign-in reads its own back from the store beside the
        // vault — an empty set here means a second account inherits nothing.
        self.listened.clear();
        // The gateway session and its walks go with the account that minted them: a sign-in
        // over the same window must not resume a session it cannot speak for, nor continue a
        // walk whose pages belong to a conversation list it has not read.
        self.session = None;
        self.catchups.clear();
        self.earlier_asks.clear();
        self.sync_asks.clear();
        self.sink.send(Event::Connection(Connection::Offline));
        self.sink.send(Event::SignedOut);
        self.sink.toast(
            "Signed out. Every key on this device has been forgotten.",
            ToastKind::Success,
        );
    }

    /// Sends a request frame, reporting a transport failure as a disconnect.
    async fn request<T: migo_protocol::Encode>(&mut self, opcode: Opcode, value: &T) {
        let Some(gateway) = self.gateway.as_mut() else {
            self.sink.toast("not connected", ToastKind::Error);
            return;
        };
        let correlation = gateway.correlate();
        if let Err(error) = gateway.send(opcode, correlation, value).await {
            self.on_disconnect(error);
        }
    }

    /// Dispatches one inbound record, batch or not.
    ///
    /// The transports return whole records off the wire; a record carrying the BATCH flag is
    /// an envelope of frames, so this is the one place every inbound frame is funnelled
    /// through [`migo_wire::decode_batch`] — which hands a bare frame back as a one-element
    /// list, keeping a single dispatch path whether a frame arrived alone or inside a batch.
    /// Replies ride the same funnel: they are matched by correlation inside `on_frame`, so a
    /// reply that arrived inside an envelope is dispatched exactly like a solo one.
    ///
    /// A batch the decoder refuses is dropped rather than fatal: the envelope is a transport
    /// optimisation, and the penalty for a broken one is the same as the frames having been
    /// lost in transit — the resume path and the user's retries recover. Disconnecting here
    /// would turn a benign envelope fault into a session outage.
    async fn on_record(&mut self, frame: migo_protocol::Frame) {
        match migo_wire::decode_batch(&frame) {
            Ok(elements) => {
                for element in elements {
                    self.on_frame(element).await;
                }
            }
            Err(error) => {
                tracing::warn!(%error, "an inbound batch envelope was refused");
            }
        }
    }

    /// Dispatches one inbound frame.
    async fn on_frame(&mut self, frame: migo_protocol::Frame) {
        // The resume counter first, before any branch can return early: every sequenced frame
        // this process reads — errors included, since an error reply rides a Critical header —
        // moves the count the next disconnect's resume asks from, and a frame counted twice or
        // skipped by a dispatch arm would hand the server a cursor it cannot honour (§150).
        if is_sequenced(&frame) {
            if let Some(session) = self.session.as_mut() {
                session.sequenced += 1;
            }
        }
        if gateway::is_error(&frame) {
            let error = gateway::refusal(&frame);
            // A refusal to a remembered SYNC ask retires the walk that asked, whatever the
            // code: the walk's continuation is driven by the page that now never comes, and a
            // walk left in `catchups` blocks every later one for its conversation until a
            // reconnect. SEQUENCE_GAP (§152: the range the repair asked for was purged) is
            // also the stall the watermark already knows how to keep — without it, every later
            // above-gap event would re-ask a hole the server has already said it cannot fill.
            if let Some(conversation_id) = self.sync_asks.remove(&frame.header.correlation) {
                self.catchups.remove(&conversation_id);
                let gap_gone = match &error {
                    GatewayError::Refused { code, .. } => {
                        *code == migo_protocol::codes::SEQUENCE_GAP
                    }
                    _ => false,
                };
                if gap_gone {
                    if let Some(signed) = self.signed.as_mut() {
                        if let Some(account) = signed.sequences.get_mut(&conversation_id) {
                            account.stalled_at = Some(account.watermark);
                        }
                    }
                }
            }
            self.sink.toast(error.to_string(), ToastKind::Error);
            return;
        }
        let Some(opcode) = Opcode::from_wire(frame.header.opcode) else {
            // An opcode this build does not know is not an error: the server may be newer. Ignoring it
            // is exactly what forward compatibility means.
            return;
        };
        match opcode {
            Opcode::Ping => self.on_ping(&frame).await,
            Opcode::MessageEvent => {
                if let Ok(event) = gateway::decode::<migo_protocol::MessageEvent>(&frame) {
                    self.route_event(&event).await;
                }
            }
            Opcode::MessageSend => self.on_accepted(&frame),
            // A delete's answer carries the tombstone's sequence — the same `MessageAccepted`
            // shape a send answers with — and an edit's answer is a bare acknowledgement. Both
            // leave the transcript move to the fan-out every participant hears, which this
            // device's own copy arrives as; the acks themselves carry nothing left to show.
            Opcode::MessageDelete => self.on_accepted(&frame),
            Opcode::MessageEdit => {}
            // Someone's delivery or read watermark moved. The event reaches the conversation's
            // subscribers only — one more reason the SUBSCRIBE-on-list-read path above matters.
            Opcode::MessageReceipt => self.on_receipt(&frame),
            Opcode::ConversationList => self.on_conversations(&frame).await,
            Opcode::ConversationCreate => self.on_conversation_created(&frame).await,
            // The roster the send path asks for when its cached membership is a list preview:
            // the whole truth the audience is chosen from. The same answer fills the roster
            // panel's ask when one is waiting — one wire, two askers, one frame.
            Opcode::ConversationRoster => self.on_roster(&frame),
            // A group's membership moved. Patched onto the cache the next audience is built
            // from, the way `on_room_member` feeds the rooms pane — and section 163's own
            // trigger: the chain rotates to the event's generation and the new one is
            // redistributed, so a key a departed member may still hold stops sealing and a
            // joined one is handed the chain before the next message of the group.
            Opcode::ConversationMemberEvent => self.on_conversation_member(&frame).await,
            // The group plane's own replies and pushes: a leave's ack, a rename's delta, and
            // a kick vote's tally from both sides of it (the voter's own reply, and the
            // fan-out everyone else hears).
            Opcode::ConversationLeave => self.on_group_left(&frame).await,
            Opcode::ConversationUpdate => self.on_conversation_update_reply(&frame).await,
            Opcode::ConversationStateEvent => self.on_conversation_state(&frame),
            Opcode::ConversationVoteKick => self.on_group_vote_reply(&frame),
            Opcode::ConversationVoteEvent => self.on_group_vote_event(&frame),
            Opcode::Sync => self.on_history(&frame).await,
            Opcode::KeyBundleFetch => self.on_bundles(&frame).await,
            Opcode::Typing => self.on_typing(&frame),
            Opcode::ProfileFetch => self.on_profiles(&frame),
            Opcode::ProfileUpdate => self.on_profile_saved(&frame),
            // The upload flows' replies, matched by correlation. The avatar and attachment
            // flows share these opcodes; the correlation — minted once per request, never
            // reused within a session — says whose reply this is. An attachment's
            // correlation was stored when its request went out; anything else is the
            // avatar's, whose own pending state ignores a late answer by taking nothing.
            Opcode::MediaUploadBegin => {
                if let Ok(ticket) = gateway::decode::<migo_protocol::MediaTicket>(&frame) {
                    let correlation = frame.header.correlation;
                    if self.attachment_begins.contains_key(&correlation) {
                        self.attachment_ticket_arrived(correlation, ticket).await;
                    } else {
                        self.avatar_ticket_arrived(ticket).await;
                    }
                }
            }
            Opcode::MediaUploadCommit => {
                let correlation = frame.header.correlation;
                if self.attachment_commits.contains_key(&correlation) {
                    self.attachment_committed(correlation).await;
                } else {
                    self.avatar_committed().await;
                }
            }
            // A fetch's signed URL, matched the same way to the want that asked for it.
            Opcode::MediaFetchUrl => {
                if let Ok(url) = gateway::decode::<migo_protocol::MediaUrl>(&frame) {
                    self.media_url_arrived(frame.header.correlation, url).await;
                }
            }
            Opcode::RelationshipList => self.on_relationships(&frame).await,
            // The acknowledgement of a FRIEND_REQUEST or FRIEND_RESPOND. Both mean the graph
            // moved and the list in the UI is now stale, so both take the same action: re-read.
            Opcode::FriendRequest | Opcode::FriendRespond => self.on_social_ack(&frame).await,
            // A BLOCK_SET or MUTE_SET acknowledgement: the graph moved the same way a friend
            // request moves it, so the same re-read answers both — the muted-set view the UI
            // renders is a filtered read of the very list this refreshes.
            Opcode::BlockSet | Opcode::MuteSet => self.on_social_ack(&frame).await,
            Opcode::FriendEvent => self.on_friend_event(&frame),
            Opcode::PresenceEvent => self.on_presence(&frame),
            Opcode::RoomList => self.on_rooms(&frame),
            Opcode::RoomJoin | Opcode::RoomCreate => self.on_room_joined(&frame).await,
            Opcode::RoomLeave => self.on_room_left(&frame).await,
            Opcode::RoomRoster => self.on_room_probe(&frame).await,
            Opcode::RoomMemberEvent => self.on_room_member(&frame).await,
            Opcode::RoomStateEvent => self.on_room_state(&frame),
            Opcode::NotificationList => self.on_alerts(&frame),
            Opcode::NotificationEvent => self.on_alert_pushed(&frame),
            // A game's delta, published to the conversation's subscribers by the referee after a
            // move, a start, or an abandon. The starting connection is excluded from a start's
            // fan-out — its reply is the opening view — and included in every other event's, so
            // this arm hears the games the account's other sessions and the other members play.
            Opcode::GameEvent => self.on_game_event(&frame),
            // The caller's own wallet moved: a spend from any session of the account, pushed on
            // the user topic every session holds from its handshake. A cue to re-read, the same
            // contract as the notification push above.
            Opcode::EconomyEvent => self.on_economy_pushed(&frame),
            Opcode::BalanceFetch => self.on_balance(&frame),
            Opcode::LedgerHistory => self.on_ledger(&frame),
            Opcode::Progression => self.on_progression(&frame),
            Opcode::Badges => self.on_badges(&frame),
            Opcode::Leaderboard => self.on_leaderboard(&frame),
            Opcode::Entitlements => self.on_entitlements(&frame),
            Opcode::GiftCatalogue => self.on_gifts(&frame),
            Opcode::GiftSend => self.on_gift_sent(&frame).await,
            Opcode::KickPointsBuy => self.on_kick_points_bought(&frame).await,
            Opcode::Search | Opcode::Suggestions => self.on_people(&frame),
            // The call plane's own frames. Invite results and TURN answers are replies this
            // device asked for; invite events, SDP/ICE relays, and state events are pushed at
            // the devices a call concerns. All decode failures are the arm's own business —
            // the server never sends a call frame this build did not ask for or cannot be in.
            Opcode::CallInvite => self.on_call_invite_result(&frame).await,
            Opcode::CallInviteEvent => self.on_call_invite_event(&frame).await,
            Opcode::CallSdp => self.on_call_sdp(&frame).await,
            Opcode::CallIce => self.on_call_ice(&frame).await,
            Opcode::CallStateEvent => self.on_call_state(&frame).await,
            Opcode::CallTurnFetch => self.on_call_turn(&frame).await,
            // The group call's own pushes: roster movement off the SFU and the sealed frame
            // key a rotating peer fanned out. The mid-call join's ask and answer ride the
            // `CallSdp` arm above, because that is the frame the server projects both into.
            Opcode::CallSfuEvent => self.on_sfu_event(&frame).await,
            Opcode::CallKeyUpdate => self.on_group_key_update(&frame),
            // A member device's copy of a rotated sender-key chain, sealed pairwise for this
            // device and forwarded to this account's user topic. The receiving half of the
            // membership trigger — the sending half lives in `on_conversation_member`.
            Opcode::GroupKeyDistribute => self.on_group_key_distribute(&frame),
            // Everything else is either an acknowledgement with nothing to show or a feature this
            // client did not negotiate.
            _ => {}
        }
    }

    async fn on_ping(&mut self, frame: &migo_protocol::Frame) {
        let Ok(ping) = gateway::decode::<migo_protocol::Ping>(frame) else {
            return;
        };
        let pong = migo_protocol::Pong {
            client_time: ping.client_time,
            server_time: Timestamp::now(),
        };
        if let Some(gateway) = self.gateway.as_mut() {
            let correlation = frame.header.correlation;
            let _ = gateway.send(Opcode::Ping, correlation, &pong).await;
        }
    }

    /// Routes one message-shaped event — live off the fan-out or out of a history page —
    /// through the sequencing account and on to its kind's own handler.
    ///
    /// The seq is tracked *first*, before any branch can drop the event: a KeyExchange sealed
    /// for another device still consumes a conversation seq, and so does a content event this
    /// build cannot decrypt — §152's numbers are a prefix of the event stream, not of the
    /// rendered one, so a watermark that skipped what the UI never sees would misjudge every
    /// later "have". A gap the event opens returns the hole's top, and the bounded walk that
    /// follows is the one place a missed frame heals without waiting for a reconnect.
    async fn route_event(&mut self, event: &migo_protocol::MessageEvent) {
        let gap = self.track_seq(event.conversation_id, event.seq);
        // Route by kind, exactly the way the SDK's `#onMessageEvent` does. A KeyExchange is a
        // sender-key distribution riding the pairwise channel: it either opens (and is adopted)
        // or it was sealed for another device and is dropped silently — both normal. Anything
        // else is content and goes to the group layer. The reply-frame path `on_accepted` is not
        // involved: this arm is the *event* opcode, pushed by the server.
        if event.kind == MessageKind::KeyExchange {
            self.on_key_exchange(event);
            return;
        }
        let Some(message) = self.decrypt(event) else {
            return;
        };
        self.sink.send(Event::Message(message));
        if let Some(to) = gap {
            // The hole's fill starts from the watermark the event left standing, which is the
            // seq below the hole — the only "have" a fill can ask from without leaving the
            // hole's own tail unfetched.
            let from = self.watermark(event.conversation_id);
            self.catch_up(event.conversation_id, from, Some(to)).await;
        }
    }

    /// Routes one conversation seq through its account, returning the top of the hole the seq
    /// opened, if it opened one. See [`SeqAccount::track`] for the accounting itself.
    fn track_seq(&mut self, conversation_id: Id, seq: u64) -> Option<u64> {
        let signed = self.signed.as_mut()?;
        signed
            .sequences
            .entry(conversation_id)
            .or_default()
            .track(seq)
    }

    /// The conversation's contiguous watermark — the "have" every continuation asks from.
    fn watermark(&self, conversation_id: Id) -> u64 {
        self.signed
            .as_ref()
            .and_then(|signed| signed.sequences.get(&conversation_id))
            .map(|account| account.watermark)
            .unwrap_or(0)
    }

    /// Handles one `GROUP_KEY_DISTRIBUTE`: a member's copy of the rotated sender-key chain,
    /// sealed pairwise for one device of this account and forwarded to the account's user
    /// topic. The receiving half of section 163's membership trigger — `on_conversation_member`
    /// is the sending half.
    ///
    /// A distribution sealed for a sibling device reaches this one too and cannot open, which
    /// is expected for the same reason the message layer's fan-out noise is: only the named
    /// device can open it. One that does open is adopted — the receiving store's own refusal
    /// of a non-advancing epoch is the whole defence against a stale or replayed copy — and
    /// the pending messages held for that sender are drained, because the distribution that
    /// unlocks them just arrived.
    fn on_group_key_distribute(&mut self, frame: &migo_protocol::Frame) {
        let Ok(event) = gateway::decode::<migo_protocol::GroupKeyDistribution>(frame) else {
            return;
        };
        let plaintext = {
            let Some(signed) = self.signed.as_mut() else {
                return;
            };
            match Envelope::decode(&event.sealed_distribution).and_then(|envelope| {
                signed
                    .sessions
                    .open(event.conversation_id, event.from_device, &envelope)
            }) {
                Ok(plaintext) => plaintext,
                // Sealed for another device of this account, or a session this store cannot
                // answer. Expected either way.
                Err(_) => return,
            }
        };
        let Ok(content) = content::decode(&plaintext) else {
            return;
        };
        let Content::ControlEvent { event: name, data } = content else {
            // A control event over the pairwise channel that is not a sender-key distribution.
            return;
        };
        if name != "sender-key" {
            return;
        }
        let Some(data) = data else { return };
        if let Some(signed) = self.signed.as_mut() {
            signed
                .groups
                .accept(event.conversation_id, event.from_device, &data);
        }
        self.drain_pending(event.conversation_id, event.from_device);
    }

    /// Handles one KeyExchange event: a sender-key distribution, or fan-out noise sealed for
    /// another device.
    ///
    /// A distribution sealed for a different device reaches this one too and cannot open — that
    /// is expected, not an error. One that does open is adopted, and the pending messages held
    /// for that sender are drained: the distribution that unlocks them just arrived.
    fn on_key_exchange(&mut self, event: &migo_protocol::MessageEvent) {
        let plaintext = {
            let Some(signed) = self.signed.as_mut() else {
                return;
            };
            match Envelope::decode(&event.envelope).and_then(|envelope| {
                signed
                    .sessions
                    .open(event.conversation_id, event.sender_device, &envelope)
            }) {
                Ok(plaintext) => plaintext,
                // Sealed for another device, or a version this store cannot answer. Expected.
                Err(_) => return,
            }
        };
        let Ok(content) = content::decode(&plaintext) else {
            return;
        };
        let Content::ControlEvent { event: name, data } = content else {
            // A control event over the pairwise channel that is not a sender-key distribution.
            return;
        };
        if name != "sender-key" {
            return;
        }
        let Some(data) = data else { return };
        if let Some(signed) = self.signed.as_mut() {
            signed
                .groups
                .accept(event.conversation_id, event.sender_device, &data);
        }
        self.drain_pending(event.conversation_id, event.sender_device);
    }

    fn on_accepted(&mut self, frame: &migo_protocol::Frame) {
        let Ok(accepted) = gateway::decode::<migo_protocol::MessageAccepted>(frame) else {
            return;
        };
        self.sink.send(Event::Accepted {
            message_id: accepted.message_id,
            conversation_id: accepted.conversation_id,
            seq: accepted.seq,
        });
    }

    /// One receipt watermark arrived: a member of a conversation delivered or read up to `seq`.
    ///
    /// Own receipts are dropped — the UI's read marker means *someone else* read it, and a
    //  watermark this account stamped itself would mark its own outgoing messages read. The
    /// server stamps `user_id` itself, so absence is the only other shape and that too is
    /// nothing to show.
    fn on_receipt(&mut self, frame: &migo_protocol::Frame) {
        let Ok(event) = gateway::decode::<migo_protocol::MessageReceipt>(frame) else {
            return;
        };
        let Some(user_id) = event.user_id else {
            return;
        };
        if let Some(signed) = self.signed.as_ref() {
            if user_id == signed.account.account_id {
                return;
            }
        }
        self.sink.send(Event::Receipt {
            conversation_id: event.conversation_id,
            user_id,
            kind: event.kind,
            seq: event.seq,
        });
    }

    async fn on_conversations(&mut self, frame: &migo_protocol::Frame) {
        let Ok(response) = gateway::decode::<migo_protocol::ConversationListResponse>(frame) else {
            return;
        };
        let me = self.signed.as_ref().map(|signed| signed.account.account_id);
        // The room bridge, reversed: conversation → room, so a summary row can name the room
        // behind it. The summary itself never does (the wire states a room's conversation id in
        // the join's answer alone), so this bridge — held from the joins this account made on
        // this device, persisted across restarts — is the only source. A Room-kind conversation
        // the bridge does not know stays `None`: its notice tail and its rooms-pane membership
        // wait for the next join, which is the same answer every client without a wire change
        // can give.
        let rooms_by_conversation: HashMap<Id, Id> = self
            .signed
            .as_ref()
            .map(|signed| {
                signed
                    .room_conversations
                    .iter()
                    .map(|(room, conversation)| (*conversation, *room))
                    .collect()
            })
            .unwrap_or_default();
        // The conversations the list says moved past what this process contiguously holds: each
        // one gets a catch-up walk, so a restart (or an outage the resume could not cover)
        // recovers the missed events without anyone opening the thread. A conversation with no
        // sequencing account yet is deliberately absent — nothing is held, so the walk would
        // replay the whole history into windows nobody opened; the thread's own open asks for
        // that replay when someone actually wants to read it.
        let mut to_catch_up: Vec<Id> = Vec::new();
        let mut out = Vec::with_capacity(response.conversations.len());
        // The conversation ids to subscribe, gathered across the whole page the way the SDK's
        // `loadConversations` gathers them: one SUBSCRIBE frame for the list rather than one per
        // conversation. Without this the desktop hears no live traffic at all — the hub keys its
        // fan-out by subscriber set, and a session that never asked never receives.
        let mut to_watch: Vec<Id> = Vec::new();
        // The peers whose names a direct conversation's title is drawn from. Gathered across the
        // whole list so one PROFILE_FETCH titles every direct chat, rather than one fetch per
        // row — the list arrives all at once, so the fetch is per-list.
        let mut peers: Vec<Id> = Vec::new();
        for summary in response.conversations {
            let members = summary.members.clone().unwrap_or_default();
            // A direct conversation's title is the peer's name and comes from a profile lookup
            // (the server carries no `title` for it), so those are the members worth naming.
            let untitled = summary.title.as_ref().is_none_or(|t| t.is_empty());
            if untitled && members.len() == 2 {
                if let Some(me) = me {
                    if let Some(peer) = members.iter().find(|id| **id != me) {
                        peers.push(*peer);
                    }
                }
            }
            if let Some(signed) = self.signed.as_mut() {
                // A list row's members field is a capped preview, not the membership: seeded
                // incomplete, so the first send reads the roster before choosing an audience.
                // A cache already complete (a create's answer, or a roster this session read)
                // is *not* demoted by a list re-read — the row says nothing the fuller answer
                // did not, and demotion would cost a roster round trip per list refresh.
                signed
                    .members
                    .entry(summary.conversation_id)
                    .and_modify(|cached| {
                        if cached.complete {
                            return;
                        }
                        cached.ids = members.clone();
                    })
                    .or_insert_with(|| MemberCache {
                        ids: members.clone(),
                        complete: false,
                    });
                // Whether the conversation is end-to-end encrypted: the fact the attachment
                // upload path turns on, filed from the summary because the summary is the one
                // wire moment that states it. End-to-end seals its uploads; anything else is
                // the room path. A removal on the room side matters as much as the insert —
                // a conversation the server turned into a room must stop sealing.
                if summary.encryption == EncryptionMode::EndToEnd {
                    signed.e2e.insert(summary.conversation_id);
                } else {
                    signed.e2e.remove(&summary.conversation_id);
                }
                if signed.conversations_watched.insert(summary.conversation_id) {
                    to_watch.push(summary.conversation_id);
                }
                if let Some(account) = signed.sequences.get(&summary.conversation_id) {
                    if summary.last_seq > account.highest_seen {
                        to_catch_up.push(summary.conversation_id);
                    }
                }
            }
            let preview = summary
                .last_message
                .as_ref()
                .and_then(|event| self.decrypt(event))
                .map(|message| message.body.preview());
            out.push(Conversation {
                conversation_id: summary.conversation_id,
                title: summary.title,
                members,
                encrypted: summary.encryption == EncryptionMode::EndToEnd,
                last_seq: summary.last_seq,
                preview,
                updated_at: summary.last_message.as_ref().map(|event| event.created_at),
                unread: u32::try_from(summary.last_seq.saturating_sub(summary.read_seq))
                    .unwrap_or(u32::MAX),
                kind: summary.kind,
                // A Room-kind conversation has a room behind it, and the bridge read above is
                // the one place this process can learn which: the summary names no room id, so
                // the join (this session's, or the persisted one a restart read back) is the
                // only wire moment that ever did. `None` for a room the bridge does not hold —
                // joined on another device and never here, a gap only a wire change closes.
                room_id: rooms_by_conversation.get(&summary.conversation_id).copied(),
            });
        }
        self.sink.send(Event::Conversations(out));
        self.fetch_profiles(peers).await;
        self.watch_conversations(to_watch).await;
        // The catch-up walks go out after the subscriptions: a walk's pages and the live
        // fan-out both land in the same accounting, and hearing the live tail first is exactly
        // the above-gap event that would have started the walk anyway.
        for conversation_id in to_catch_up {
            let from = self.watermark(conversation_id);
            self.catch_up(conversation_id, from, None).await;
        }
    }

    /// Subscribes the session to a batch of conversation topics, the gate every live event passes
    /// through.
    ///
    /// Best effort by the same reasoning the presence watches are: the server answers a refused
    /// SUBSCRIBE without a reason (the answer cannot become a probe), and a client that toasted
    /// about every declined watch would be inventing reasons the server chose not to give. A
    /// conversation left unwatched still converges through the history read on open.
    async fn watch_conversations(&mut self, ids: Vec<Id>) {
        self.watch_topics(TopicKind::Conversation, ids).await;
    }

    /// The one SUBSCRIBE sender both watch kinds share: one frame for the whole batch, whatever
    /// the topic kind, so a list read or a reconnect costs one round trip rather than one per id.
    async fn watch_topics(&mut self, kind: TopicKind, ids: Vec<Id>) {
        if ids.is_empty() {
            return;
        }
        let topics: Vec<Topic> = ids.into_iter().map(|id| Topic { kind, id }).collect();
        let request = SubscribeRequest { topics };
        self.request(Opcode::Subscribe, &request).await;
    }

    async fn on_conversation_created(&mut self, frame: &migo_protocol::Frame) {
        let Ok(summary) = gateway::decode::<migo_protocol::ConversationSummary>(frame) else {
            return;
        };
        let mut to_watch: Vec<Id> = Vec::new();
        if let Some(signed) = self.signed.as_mut() {
            // The create answer names the whole membership — the caller chose it — so it is
            // cached complete: the first send builds its audience from it with no roster read.
            signed.members.insert(
                summary.conversation_id,
                MemberCache {
                    ids: summary.members.clone().unwrap_or_default(),
                    complete: true,
                },
            );
            // The encryption fact, the same filing the list read makes: the create answer
            // states it, and an attachment sent before the first list refresh needs it.
            if summary.encryption == EncryptionMode::EndToEnd {
                signed.e2e.insert(summary.conversation_id);
            } else {
                signed.e2e.remove(&summary.conversation_id);
            }
            if signed.conversations_watched.insert(summary.conversation_id) {
                to_watch.push(summary.conversation_id);
            }
        }
        self.sink.send(Event::ConversationCreated {
            conversation_id: summary.conversation_id,
        });
        self.watch_conversations(to_watch).await;
        self.request_conversations().await;
    }

    async fn on_history(&mut self, frame: &migo_protocol::Frame) {
        // A page arrived, so the ask it answers is settled; what remains in `sync_asks` is
        // exactly the asks whose replies are still owed — the ones an error frame must retire.
        self.sync_asks.remove(&frame.header.correlation);
        let Ok(response) = gateway::decode::<migo_protocol::SyncResponse>(frame) else {
            return;
        };
        // A load-earlier answer, matched by the correlation its ask minted. The SYNC answer
        // names its conversation but not its direction, so this is the only way to tell a
        // backwards page from a catch-up walk's forward one — and the two must not be confused:
        // an earlier page is the thread's own prepend, while a forward page belongs to whatever
        // walk is running in `catchups`.
        if let Some(conversation_id) = self.earlier_asks.remove(&frame.header.correlation) {
            // The page still routes through the sequencing account, exactly the way the SDK's
            // `ingest` runs inside the web's `loadEarlier`: events above the watermark open a
            // real hole (a budget-stopped walk's missing middle, met from the newest end), and
            // a historical KeyExchange must be adopted or the content sealed under it stays
            // undecryptable. What the page must NOT do is mint windows the way a live message
            // does — it belongs to the thread that asked for it, opened or not.
            let mut messages = Vec::with_capacity(response.messages.len());
            let mut gap = None;
            for event in response.messages {
                if let Some(top) = self.track_seq(conversation_id, event.seq) {
                    gap = Some(top);
                }
                if event.kind == MessageKind::KeyExchange {
                    self.on_key_exchange(&event);
                    continue;
                }
                if let Some(message) = self.decrypt(&event) {
                    messages.push(message);
                }
            }
            self.sink.send(Event::HistoryEarlier {
                conversation_id,
                messages,
                from_seq: response.from_seq,
                more: response.more,
            });
            if let Some(to) = gap {
                // The hole the page's own top opened: filled forward from the watermark, the
                // same walk a live above-gap event triggers — and refused by the same stall
                // guard when the hole is one the server has already said it cannot fill.
                let from = self.watermark(conversation_id);
                self.catch_up(conversation_id, from, Some(to)).await;
            }
            return;
        }
        // A forward page. History replays through the *live* routing, exactly the way the SDK's
        // `catchUp` feeds every fetched event through `messaging.ingest`: a historical
        // KeyExchange is a sender-key distribution the group layer must adopt, or every message
        // sealed under it stays undecryptable — the buffering holds them, and the per-sender
        // bound silently drops the oldest. Replaying in the server's order preserves the
        // "distribution before content" ordering the buffering relies on. The seqs are tracked
        // first and whole, before any branch can drop an event, because the walk's own
        // continuation question — did the watermark move? — is answered by the accounting, not
        // by what decrypted.
        let conversation_id = response.conversation_id;
        let watermark_before = self.watermark(conversation_id);
        let mut messages = Vec::with_capacity(response.messages.len());
        for event in response.messages {
            self.track_seq(conversation_id, event.seq);
            if event.kind == MessageKind::KeyExchange {
                self.on_key_exchange(&event);
                continue;
            }
            if let Some(message) = self.decrypt(&event) {
                messages.push(message);
            }
        }
        let watermark_after = self.watermark(conversation_id);
        // The walk this page belongs to, if one is running: a page that arrives for a
        // conversation with no walk is a stray answer to an ask this process does not remember
        // (a resumed session's replayed SYNC reply, say) — its events are already absorbed
        // above, and there is no continuation to decide.
        let walk = self.catchups.get_mut(&conversation_id).map(|walk| {
            (
                walk.to_seq,
                walk.continues(
                    watermark_after > watermark_before,
                    response.more,
                    watermark_after,
                ),
            )
        });
        let mut more = response.more;
        if let Some((to_seq, continues)) = walk {
            if continues {
                if let Some(walk) = self.catchups.get_mut(&conversation_id) {
                    walk.pages_left -= 1;
                }
                // The walk's next page asks from the watermark the page itself advanced — one
                // contiguous brick at a time, never from the page's top, which may stand above
                // a hole the next page must fill.
                self.sync_page(conversation_id, watermark_after).await;
                // An intermediate page's `more` is not the thread's verdict: the walk is still
                // running, and the row the verdict arms belongs to the final page alone.
                more = false;
            } else {
                self.catchups.remove(&conversation_id);
                if watermark_after == watermark_before {
                    // The page could not move the watermark: the hole below it is the server's
                    // to answer (history gone, or a truncation to render), not ours to re-ask
                    // in a loop. Recorded as the stall every later above-gap event checks
                    // before asking again — the same guard the SDK's `#stalledAt` keeps.
                    if let Some(signed) = self.signed.as_mut() {
                        if let Some(account) = signed.sequences.get_mut(&conversation_id) {
                            account.stalled_at = Some(watermark_after);
                        }
                    }
                }
            }
            if to_seq.is_some() {
                // A bounded walk is a gap repair, not a transcript replay: its final page's
                // `more` speaks about the conversation's live edge, which the repair never
                // reached and was never asked to. The load-earlier row belongs to the
                // thread's own open and its unbounded walk — a repair that armed it would
                // put a button on a thread that is, by the repair's own success, current.
                more = false;
            }
        }
        self.sink.send(Event::History {
            conversation_id,
            messages,
            more,
        });
    }

    async fn on_bundles(&mut self, frame: &migo_protocol::Frame) {
        let Ok(response) = gateway::decode::<migo_protocol::KeyBundleResponse>(frame) else {
            return;
        };
        // This account's id, read before the mutable borrow below: own devices ride the same
        // responses a peer's do (the send audience includes this account's other devices), and a
        // key-change memory that counted them would warn this account about itself.
        let me = self.signed.as_ref().map(|signed| signed.account.account_id);
        // The peer identities this response observed, filed for the map update and the events
        // after the loop — the borrow on `signed` outlives the iteration, and the fingerprint
        // memory belongs to the worker, not to the session.
        let mut observed: Vec<(Id, Id, [u8; 32])> = Vec::new();
        let Some(signed) = self.signed.as_mut() else {
            return;
        };
        // This device's own E2EE fingerprint, read once before the loop: the safety number the
        // UI shows for a peer device is a *pair* number — both sides' keys in one value — and the
        // own half is this session's identity, constant for as long as the session lives.
        let own = signed.sessions.keys().identity_public().fingerprint();
        for wire in response.bundles {
            let bundle = crate::crypto::session::bundle_from_wire(
                &wire.identity_key,
                wire.signed_prekey_id,
                &wire.signed_prekey,
                &wire.signed_prekey_signature,
                match (wire.one_time_prekey_id, wire.one_time_prekey.as_ref()) {
                    (Some(id), Some(bytes)) => Some((id, bytes.as_slice())),
                    // A device out of one-time prekeys still gets a session, just without the fourth
                    // DH input. Refusing to talk to it would be worse than the forward-secrecy loss
                    // for that one first message.
                    _ => None,
                },
            );
            match bundle {
                Ok(bundle) => {
                    if Some(wire.user_id) != me {
                        observed.push((
                            wire.user_id,
                            wire.device_id,
                            bundle.identity.fingerprint(),
                        ));
                    }
                    signed.bundles.insert(wire.device_id, bundle);
                    let devices = signed.devices.entry(wire.user_id).or_default();
                    if !devices.contains(&wire.device_id) {
                        devices.push(wire.device_id);
                    }
                }
                Err(_) => self.sink.toast(
                    "a key bundle from the server did not verify; not sending to that device",
                    ToastKind::Error,
                ),
            }
        }
        // §47/§164: every observed peer device moves the last-seen memory and tells the UI what
        // it is — the pair safety number to read aloud, the same string the peer's own screen
        // shows for this device (see [`model::pair_fingerprint`]'s symmetric order), and whether
        // the fingerprint changed from the last one this vault sealed. First sight is not a
        // change (see [`peer_fingerprint_changed`]).
        if let Some(account_id) = me {
            for (user_id, device_id, fingerprint) in observed {
                let changed = match self.peers.as_mut() {
                    Some((account, seen)) if *account == account_id => {
                        peer_fingerprint_changed(seen, device_id, fingerprint)
                    }
                    _ => false,
                };
                self.sink.send(Event::PeerIdentity {
                    user_id,
                    device_id,
                    safety_number: model::pair_safety_number(&own, &fingerprint),
                    changed,
                });
            }
        }
        // A group-call key ask that was waiting on exactly this fetch — the ask's participant
        // had no pairwise session and no bundle — retries now, while the bundle is fresh.
        self.retry_group_key_ask().await;
    }

    fn on_typing(&mut self, frame: &migo_protocol::Frame) {
        let Ok(event) = gateway::decode::<migo_protocol::TypingEvent>(frame) else {
            return;
        };
        let Some(user_id) = event.user_id else { return };
        self.sink.send(Event::Typing {
            conversation_id: event.conversation_id,
            user_id,
            typing: event.state == migo_protocol::TypingState::Start,
        });
    }

    fn on_profiles(&mut self, frame: &migo_protocol::Frame) {
        let Ok(response) = gateway::decode::<migo_protocol::ProfileResponse>(frame) else {
            return;
        };
        let me = self.signed.as_ref().map(|signed| signed.account.account_id);
        let mut names = HashMap::with_capacity(response.profiles.len());
        for profile in &response.profiles {
            let name = if profile.display_name.is_empty() {
                profile.username.clone()
            } else {
                profile.display_name.clone()
            };
            // Presence rides the same answer the names do: the profile row is the server's own
            // statement of where the account stood when it was read, which is the best seed a
            // presence UI can get before any event arrives. `Unknown` is skipped rather than
            // sent — an event that says "never heard" is no event at all.
            if let Some(state) = profile.presence {
                let state = model::Presence::from_wire(state.to_wire());
                if state != model::Presence::Unknown {
                    self.sink.send(Event::PresenceChanged {
                        user_id: profile.user_id,
                        state,
                    });
                }
            }
            // A card answering for this account is the pane's copy as well as a name. The
            // match is on the id, not the position: a response is untrusted input, and the
            // self fetch asked for exactly this id, so a card that does not carry it is a
            // card the pane was never asking for.
            if Some(profile.user_id) == me {
                let own = Self::own_profile_from_wire(profile);
                self.sink.send(Event::OwnProfile(Ok(own)));
            }
            // A card answering the member menu's ask is the profile view's copy. The same
            // match-on-id rule the self card follows, and for the same reason: the batch is
            // untrusted input, and only the account the menu named is the card the menu was
            // asking for.
            if let Some((conversation_id, asked)) = self.pending_member_profile {
                if asked == profile.user_id {
                    let card = Self::member_card_from_wire(profile);
                    self.sink.send(Event::MemberProfile {
                        conversation_id,
                        card,
                    });
                }
            }
            names.insert(profile.user_id, name);
        }
        self.sink.send(Event::Names(names));
        // The ask is spent with the reply, matched or not: a reply that names every subject
        // it was given and still not the asked-for account is the server's own word that the
        // card is not coming, and a later unrelated batch must not answer a question nobody
        // is asking anymore.
        self.pending_member_profile = None;
    }

    /// The reply a PROFILE_UPDATE carries: the caller's own card, read back through the same
    /// path a fetch takes, so the pane hears about it as a saved fact.
    fn on_profile_saved(&mut self, frame: &migo_protocol::Frame) {
        let Ok(profile) = gateway::decode::<migo_protocol::UserProfile>(frame) else {
            return;
        };
        let me = self.signed.as_ref().map(|signed| signed.account.account_id);
        if me != Some(profile.user_id) {
            // Not this account's card: a mismatched reply is untrusted input, and filing a
            // stranger's profile as this account's save would draw somebody else's name in the
            // pane's form. Drop it rather than guess.
            return;
        }
        let own = Self::own_profile_from_wire(&profile);
        self.sink.toast("Profile saved", ToastKind::Success);
        self.sink.send(Event::ProfileSaved(own));
    }

    /// The relationship list came back: reduce it to model rows, then ask for the two things a
    /// list of ids cannot show on its own — the names and the live presence of those accounts.
    async fn on_relationships(&mut self, frame: &migo_protocol::Frame) {
        let Ok(response) = gateway::decode::<migo_protocol::RelationshipList>(frame) else {
            return;
        };
        let entries: Vec<Relationship> = response
            .entries
            .into_iter()
            .map(|entry| Relationship {
                user_id: entry.user_id,
                kind: RelationshipKind::from_wire(entry.kind),
            })
            .collect();
        // Names for every edge, presence watches only for friendships: a pending request has no
        // presence to show (the dot would say "stranger is offline" at best), and a block is
        // exactly the account whose whereabouts this client must stop asking about.
        let named: Vec<Id> = entries.iter().map(|entry| entry.user_id).collect();
        let friends: Vec<Id> = entries
            .iter()
            .filter(|entry| entry.kind == RelationshipKind::Friend)
            .map(|entry| entry.user_id)
            .collect();
        // The member view's edge ask, answered from the same walk: one graph read serves the
        // pane and the profile card's social line, because the edge is the same fact to both.
        // The ask is spent with the reply whether the graph names the account or not — no edge
        // is an answer ("add friend"), not a lost ask.
        if let Some((conversation_id, asked)) = self.pending_member_edge.take() {
            let kind = entries
                .iter()
                .find(|entry| entry.user_id == asked)
                .map(|entry| entry.kind);
            self.sink.send(Event::MemberEdge {
                conversation_id,
                kind,
            });
        }
        self.sink.send(Event::Relationships(entries));
        self.fetch_profiles(named).await;
        self.watch_users(friends).await;
    }

    /// A FRIEND_REQUEST or FRIEND_RESPOND was accepted by the server: the graph moved, so the
    /// list the UI holds is stale whatever the specifics were.
    async fn on_social_ack(&mut self, frame: &migo_protocol::Frame) {
        let Ok(acknowledged) = gateway::decode::<migo_protocol::Acknowledged>(frame) else {
            return;
        };
        if acknowledged.ok {
            self.request_relationships().await;
        }
    }

    /// The other side of a social event this account was the audience of.
    ///
    /// The state string is a hint, not a source of truth (the server's own doc says so): the UI
    /// learns the new shape of the graph from the re-read that follows, and this event exists to
    /// say *that* something happened. An unknown state is still a graph movement, so it still
    /// carries an event — just one with nothing to claim about what moved.
    fn on_friend_event(&mut self, frame: &migo_protocol::Frame) {
        let Ok(event) = gateway::decode::<migo_protocol::FriendEvent>(frame) else {
            return;
        };
        let accepted = match event.state.as_str() {
            "request" => Some(false),
            "accepted" => Some(true),
            _ => None,
        };
        self.sink.send(Event::FriendChanged {
            user_id: event.user_id,
            accepted,
        });
    }

    /// A presence event off a subscribed user topic.
    fn on_presence(&mut self, frame: &migo_protocol::Frame) {
        let Ok(event) = gateway::decode::<migo_protocol::PresenceEvent>(frame) else {
            return;
        };
        let state = model::Presence::from_wire(event.state.to_wire());
        if state == model::Presence::Unknown {
            return;
        }
        self.sink.send(Event::PresenceChanged {
            user_id: event.user_id,
            state,
        });
    }

    /// Decrypts one MESSAGE_EVENT into something the UI can show.
    ///
    /// The content path runs through the group layer: the message opens under the sender's chain
    /// when a distribution has been accepted, and is *held* — not failed — when one has not, the
    /// same buffering the SDK does, because the distribution may still be in flight. A message
    /// that will not open even with a key becomes [`Body::Undecryptable`] rather than
    /// disappearing: a silently dropped message is indistinguishable from one that was never
    /// sent, and the gap in the sequence numbers would go unexplained.
    fn decrypt(&mut self, event: &migo_protocol::MessageEvent) -> Option<Message> {
        // A tombstone never needs opening: the server clears the envelope when it writes one,
        // and the row's whole meaning is carried by the `deleted` flag. Filed as a body-less
        // message so the chat layer's fold — which matches on id — converts the row it
        // already holds rather than adding a second one.
        if event.deleted.unwrap_or(false) {
            return Some(Message {
                message_id: event.message_id,
                conversation_id: event.conversation_id,
                seq: event.seq,
                sender_id: event.sender_id,
                outgoing: event.sender_id == self.signed.as_ref()?.account.account_id,
                body: Body::Tombstone,
                sent_at: event.created_at,
                delivery: Delivery::Received,
                deleted: true,
                edited: false,
                expires_at: None,
            });
        }
        let (outgoing, mine) = {
            let signed = self.signed.as_ref()?;
            (
                event.sender_id == signed.account.account_id,
                event.sender_device == signed.account.device_id,
            )
        };

        // The deadline a receiver's own sweep reads, computed from the sealed lifetime the
        // body's projection carries. The mine-branch of a non-edit echo leaves it `None` on
        // purpose: that echo is suppressed below, so its row keeps the deadline the optimistic
        // insert already stamped — and an edit's echo re-opens the content, whose sealed
        // lifetime is the original's, so the edit keeps the promise the send made.
        let (body, expires_in_ms) = if mine {
            // Our own message, echoed back. The chain that sealed it is this device's, and its
            // keys advance on seal, so there is nothing to open — the UI already has this text
            // from the optimistic insert. An edit's echo is the exception: the replacement was
            // sealed *after* the original, so the ratchet position the sender-key chain holds
            // now is the edit's, and this is the one place this device can read its own edit
            // back. The body it carries is the replacement text, which the fold below applies
            // to the row the optimistic insert left.
            if event.edited_at.is_some() {
                let signed = self.signed.as_mut()?;
                let opened = signed
                    .groups
                    .open(event.conversation_id, event.sender_device, &event.envelope)
                    .ok()
                    .and_then(|plaintext| content::decode(&plaintext).ok());
                match opened {
                    Some(content) => body_of(content),
                    None => (Body::Text(String::new()), None),
                }
            } else {
                (Body::Text(String::new()), None)
            }
        } else {
            let held = {
                let signed = self.signed.as_mut()?;
                if !signed
                    .groups
                    .has_receiver(event.conversation_id, event.sender_device)
                {
                    None
                } else {
                    Some(
                        signed
                            .groups
                            .open(event.conversation_id, event.sender_device, &event.envelope)
                            .map_err(|error| error.to_string())
                            .and_then(|plaintext| {
                                content::decode(&plaintext)
                                    .map_err(|_| "unreadable content".to_owned())
                            }),
                    )
                }
            };
            match held {
                // The sender's distribution has not arrived yet, or the message names a chain it
                // does not know (a rotation not caught up to). Hold the message in both cases:
                // the distribution that unlocks it may still be in flight, and a message that
                // vanishes silently is indistinguishable from one never sent. Drained the moment
                // a distribution is adopted.
                None => {
                    self.buffer(event.clone());
                    return None;
                }
                Some(Err(_)) => {
                    self.buffer(event.clone());
                    return None;
                }
                Some(Ok(content)) => {
                    // A call key rides a ControlEvent like any other system content, so the
                    // group layer opens it like one and hands it here as opaque bytes. The
                    // engine's intercept runs before the body is classified: an adopted key
                    // is consumed on the spot and the message suppressed — otherwise the
                    // "unsupported message" fallback below would render the caller's key
                    // handoff as noise in the thread (the cross-client bug this fixes).
                    if self.adopt_call_key(&content) {
                        return None;
                    }
                    // An attachment's key material travels only inside its message, so the
                    // one place it can be filed is here — before the content is consumed
                    // into a body, and on every path that opens one (live, history,
                    // held-and-drained), because the fetch it enables may come minutes or
                    // days after the message that carried it.
                    self.file_media_keys(&content);
                    body_of(content)
                }
            }
        };

        if mine && event.edited_at.is_none() {
            // Suppress the echo entirely: ACCEPTED already moved the message from sending to sent.
            return None;
        }

        // The deadline runs from the server's `created_at` on the receiving clock — the same
        // arithmetic the web client's `messageExpired` makes (`createdAt + lifetime`), and the
        // same design decision: a client must not wait for the server to tell it a message
        // is gone. Clock skew between sender and receiver shifts the moment slightly, never
        // the promise.
        let expires_at = expires_in_ms
            .map(|lifetime| event.created_at.saturating_add_millis(i64::from(lifetime)));

        Some(Message {
            message_id: event.message_id,
            conversation_id: event.conversation_id,
            seq: event.seq,
            sender_id: event.sender_id,
            outgoing,
            body,
            sent_at: event.created_at,
            delivery: Delivery::Received,
            deleted: false,
            edited: event.edited_at.is_some(),
            expires_at,
        })
    }

    /// Holds one undecryptable message, dropping the oldest once the per-sender bound is reached.
    ///
    /// The bound is the same defence the pairwise ratchet's skipped-message list makes: a sender
    /// whose distributions never arrive must not grow this store forever. When a distribution does
    /// arrive, [`Self::drain_pending`] replays what it held, in arrival order.
    fn buffer(&mut self, event: migo_protocol::MessageEvent) {
        let Some(signed) = self.signed.as_mut() else {
            return;
        };
        let key = (event.conversation_id, event.sender_device);
        let list = signed.pending.entry(key).or_default();
        list.push(event);
        while list.len() > MAX_PENDING_PER_SENDER {
            list.remove(0);
        }
    }

    /// Replays the messages held for one sender, in arrival order.
    ///
    /// Called when a distribution for that sender is adopted. A message that still does not open
    /// — a second rotation was announced between the hold and the drain — is re-held once; a
    /// message that opens is delivered as if it had just arrived.
    fn drain_pending(&mut self, conversation_id: Id, sender_device: Id) {
        let Some(signed) = self.signed.as_mut() else {
            return;
        };
        let held = signed
            .pending
            .remove(&(conversation_id, sender_device))
            .unwrap_or_default();
        for event in held {
            if let Some(message) = self.decrypt(&event) {
                self.sink.send(Event::Message(message));
            }
        }
    }

    /// Handles a lost connection: drop the socket, report it, arm the retry.
    ///
    /// Deliberately synchronous and deliberately short. The waiting and the retrying happen in
    /// [`Self::reconnect`], driven by the select loop, so this can be called from the middle of a send
    /// path without that path having to await a reconnection it did not ask for.
    fn on_disconnect(&mut self, error: GatewayError) {
        self.gateway = None;
        // The heartbeat belongs to the dead session, not the account: disarmed here so the
        // select loop's beat arm parks while disconnected, and re-armed by `connect` when a
        // WELCOME states a fresh interval.
        self.heartbeat = None;
        // A live call cannot survive the socket its signalling rides — the reconnect that
        // follows is a new session with no call state on the server side. Ended locally with
        // the Network reason; a ring is deliberately left standing, because the invite that
        // caused it is a durable server-side fact and may outlive this reconnect.
        self.calls_offline();
        // The group-call seat is the same story: the roster it held is gone server-side with
        // the session, and a join still waiting on its roster snapshot waits on a socket that
        // will never answer.
        self.group_calls_offline();
        if self.signed.is_none() {
            self.retry = None;
            self.sink.send(Event::Connection(Connection::Offline));
            return;
        }
        self.sink
            .send(Event::Connection(Connection::Failed(error.to_string())));
        // Only arm a fresh schedule if none is running; a second failure mid-backoff must not reset
        // the delay back to the base and turn the backoff into a busy loop.
        if self.retry.is_none() {
            self.retry = Some(Retry::first(&mut OsRandom));
        }
    }

    /// Sends the session's periodic PING (§151: the server closes a socket silent for two
    /// advertised intervals; half the interval answers well before that).
    ///
    /// Fire-and-forget by design: the reply is a PONG nobody correlates, and a failure here is
    /// the same disconnect any other send would see — `on_disconnect` disarms the timer and arms
    /// the retry, which is the whole recovery story. A missing PONG (the server gone quiet
    /// instead of the socket breaking) is caught on the next beat, when the send fails, or by
    /// the read that follows it — the desktop has no RTT budget to defend.
    async fn send_heartbeat(&mut self) {
        let Some(gateway) = self.gateway.as_mut() else {
            return;
        };
        let ping = migo_protocol::Ping {
            client_time: Timestamp::now(),
        };
        // Correlation 0: nothing waits on the answer, and the server's reply rides the same
        // opcode anyway (section 139).
        let _ = gateway.send(Opcode::Ping, 0, &ping).await;
    }

    /// Reports a failure that left the client signed out.
    fn fail(&mut self, text: String) {
        self.sink
            .send(Event::Connection(Connection::Failed(text.clone())));
        self.sink.toast(text, ToastKind::Error);
    }
}

/// Whether a vault's keys are the container's own account come home: the saved session names the
/// same account, and the vault holds the same root bytes the container carries.
///
/// Both halves are required. The account id alone would let a container of the account's root and
/// a vault of the account's passphrase-only device cross wires — the session would then be
/// established for keys that are not the file's. The root alone says nothing about which device
/// the vault is, and a vault with the root but no saved session has no device record to name the
/// KNOWN device with, so tier one cannot run and the deliberate-removal refusal is the honest
/// answer for it too.
fn vault_holds_this_account(keys: &DeviceKeys, account_id: Id, root: [u8; 32]) -> bool {
    keys.session
        .as_ref()
        .is_some_and(|saved| saved.account_id == account_id)
        && keys.root == Some(root)
}

/// Decodes standard base64, as the challenge payloads and signature fields arrive.
///
/// A challenge payload that does not decode is a server the client cannot talk to, so the caller
/// treats `None` as a protocol error rather than retrying bytes that will never be a signature.
fn base64_decode(text: &str) -> Option<Vec<u8>> {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.decode(text).ok()
}

/// Bytes as lowercase hex, no prefix — the form every chain surface and vector file writes.
fn hex_of(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// An address text folded to the registry's canonical form: lowercase hex, no prefix.
///
/// The server's wallet registry stores — and `GET /v1/wallets` returns — exactly this form,
/// while display and derivation hold EIP-55 with the `0x` prefix. The two are the same
/// address but not the same string, and comparing them unfolded is how a registered wallet
/// reads as missing on every sync and gets re-registered, resurrecting one the user archived.
fn canonical_address(address: &str) -> String {
    let trimmed = address.trim();
    let bare = trimmed.strip_prefix("0x").unwrap_or(trimmed);
    bare.to_ascii_lowercase()
}

/// Files one observed peer fingerprint against the last-seen map, and says whether it changed.
///
/// First sight is not a change: a peer's second device is new, not suspicious, and a warning
/// that fired for every new device would be a warning nobody read by the second week. Only a
/// device whose fingerprint this map already held, holding a different one now, is the §47
/// moment — and the map moves to the new fingerprint either way, so the change is reported
/// once, not once per conversation or once per fetch. The old fingerprint is not kept: the
/// last-seen number is on the screen to be compared against, and a memory of every prior key
/// of every peer is a memory that outlives its usefulness the moment it is read.
fn peer_fingerprint_changed(
    seen: &mut HashMap<Id, [u8; 32]>,
    device: Id,
    fingerprint: [u8; 32],
) -> bool {
    match seen.insert(device, fingerprint) {
        Some(previous) => previous != fingerprint,
        None => false,
    }
}

/// Whether a member change moves the group's membership — the audience a sender key is sealed
/// for — as opposed to a presence flicker inside an unchanged audience.
///
/// Section 163's first trigger keys on this: a join, a leave, a kick, and a ban all change who
/// may read what follows, so the chain rotates and the new one is redistributed. A
/// disconnect/reconnect pair does not — the member's devices keep the key across the gap, and
/// a rotation on a flicker would spend an epoch to protect against nobody. The wire has no
/// separate invite change: a member's acceptance of an invite surfaces here as `Joined`, which
/// is the moment their account first enters the audience.
fn is_membership_change(change: migo_protocol::MemberChange) -> bool {
    matches!(
        change,
        migo_protocol::MemberChange::Joined
            | migo_protocol::MemberChange::Left
            | migo_protocol::MemberChange::Kicked
            | migo_protocol::MemberChange::Banned
    )
}

/// Projects decrypted [`Content`] onto the UI's [`Body`] — and the sealed disappearing
/// lifetime, when one is present, onto the deadline the UI's own sweep reads.
///
/// The lifetime travels inside the ciphertext (`MessageSend.expires_in_ms` reaches only the
/// server, which never echoes it), so this projection is the only place a receiver's deadline
/// can come from. The deadline runs on the receiving clock by the same design the web client
/// states: a client must not wait for the server to tell it a message is gone.
fn body_of(content: Content) -> (Body, Option<u32>) {
    match content {
        Content::Text {
            text,
            expires_in_ms,
            ..
        } => (Body::Text(text), expires_in_ms),
        Content::MediaRef {
            media_id,
            mime_type,
            size_bytes,
            width,
            height,
            caption,
            expires_in_ms,
            ..
        } => (
            Body::Media {
                media_id,
                mime_type,
                size_bytes,
                width,
                height,
                caption,
            },
            expires_in_ms,
        ),
        Content::VoiceNoteRef {
            media_id,
            duration_ms,
            waveform,
            expires_in_ms,
            ..
        } => (
            Body::VoiceNote {
                media_id,
                duration_ms,
                waveform: waveform.clone(),
            },
            expires_in_ms,
        ),
        Content::Reaction {
            emoji,
            target_message_id,
            ..
        } => (
            Body::Reaction {
                emoji,
                target: target_message_id,
            },
            None,
        ),
        Content::ControlEvent { .. } => (Body::Unsupported { content_type: 5 }, None),
        Content::Unsupported { content_type } => (Body::Unsupported { content_type }, None),
    }
}

/// The capture pump for a voice-note recording.
///
/// A plain thread, not a task, because the microphone's chunks arrive on a *blocking* std
/// channel — a runtime thread would sit parked on `recv()` anyway, and a plain thread says
/// so honestly. The pump owns the receiver and the draft file, and nothing else; the worker
/// keeps the device handle, and dropping that handle is what stops capture and (via the
/// closed channel) ends this thread. The samples append at whatever rate the host granted,
/// resampled to the note's own rate when the host insisted on another, written to the file
/// as they arrive — never buffered into a second copy of the note — with each tenth of a
/// second folded into one live-waveform bar on the way past. A paused recording drops its
/// chunks: a pause is time the note does not contain, so neither the count nor the file
/// grows through one. The cap's own backstop lives here too — the pump stops appending at
/// the cap even if nobody noticed the tick — and the last thing the pump does is flush,
/// so the file a finalisation reads back is the whole note.
fn spawn_recording_pump(
    frames: std_mpsc::Receiver<Vec<i16>>,
    source_rate: u32,
    shared: Arc<RecordShared>,
    mut out: std::io::BufWriter<std::fs::File>,
) -> std::thread::JoinHandle<()> {
    // The pump's own narrow import: `Write` is the file's business, not the module's — this
    // thread is the only writer the store ever has.
    use std::io::Write as _;
    std::thread::Builder::new()
        .name("migo-voice-record".to_owned())
        .spawn(move || {
            let max_samples =
                media::VOICE_NOTE_MAX_MS * u64::from(media::VOICE_NOTE_SAMPLE_RATE) / 1_000;
            let mut resampler = (source_rate != media::VOICE_NOTE_SAMPLE_RATE)
                .then(|| call_audio::Resampler::new(source_rate, media::VOICE_NOTE_SAMPLE_RATE));
            let mut resampled: Vec<i16> = Vec::new();
            // One waveform bar's window: the loudest sample so far and how many samples the
            // window has taken. A window that a pause or the end interrupts is dropped, not
            // flushed — a bar stands for a tenth of a second someone spoke.
            let mut window_peak: i16 = 0;
            let mut window_len: u64 = 0;
            let mut byte: [u8; 2] = [0; 2];
            // `while let` stops when the channel closes — the microphone was dropped, so the
            // recording is over whatever the flag says, and everything written so far is
            // the note.
            while let Ok(chunk) = frames.recv() {
                if shared.stop.load(Ordering::Relaxed) {
                    break;
                }
                if shared.paused.load(Ordering::Relaxed) {
                    continue;
                }
                if shared.samples.load(Ordering::Relaxed) >= max_samples {
                    break;
                }
                resampled.clear();
                match resampler.as_mut() {
                    Some(resampler) => {
                        resampler.process(&chunk, &mut resampled);
                    }
                    None => resampled.extend_from_slice(&chunk),
                }
                if resampled.is_empty() {
                    continue;
                }
                for sample in &resampled {
                    if sample.unsigned_abs() > window_peak.unsigned_abs() {
                        window_peak = *sample;
                    }
                    window_len += 1;
                    if window_len >= media::WAVEFORM_WINDOW_SAMPLES {
                        if let Ok(mut bars) = shared.amplitudes.lock() {
                            bars.push(media::amplitude_to_bar(window_peak));
                        }
                        window_peak = 0;
                        window_len = 0;
                    }
                    byte.copy_from_slice(&sample.to_le_bytes());
                    if out.write_all(&byte).is_err() {
                        // The disk refused the note's next sample: the note is what was
                        // written, and pressing on would only lie about it.
                        let _ = out.flush();
                        return;
                    }
                }
                shared
                    .samples
                    .fetch_add(resampled.len() as u64, Ordering::Relaxed);
            }
            let _ = out.flush();
        })
        .expect("the recording pump's thread spawns")
}

/// Everything the playback pump thread owns, handed over in one piece the way the recording
/// pump takes its shared half: the speaker's channel and clock, the note's samples at their
/// own rate, the two flags the worker shares with the playing state, and the loop's own ways
/// home — the command channel and the note's id.
struct PlaybackPump {
    /// The speaker's frame channel; one send per chunk of output.
    frames: std_mpsc::Sender<Vec<i16>>,
    /// The rate the speaker actually plays at — the resampler's target.
    sink_rate: u32,
    /// The note's own samples, at the rate they were recorded at.
    samples: Arc<Vec<i16>>,
    /// The rate the samples were recorded at.
    rate: u32,
    /// The stop flag the playing state shares: a stop is an end even mid-chunk.
    stop: Arc<AtomicBool>,
    /// The pace flag (§179): a whole percent, re-read before every chunk.
    speed: Arc<AtomicU32>,
    /// The worker's command channel, for the heard threshold and the ending.
    commands: mpsc::UnboundedSender<Command>,
    /// The note being played — the id its listened mark is filed under.
    media_id: Id,
}

/// The playback pump for a voice note.
///
/// Feeds the speaker a hundred milliseconds of source audio at a time and sleeps just under
/// a chunk of output between sends, so the pump stays ahead of the device without queuing
/// the whole note into a channel that would outlive a stop. The pace is a sleep, not a
/// clock: a desktop voice note is a bubble's button, not a studio monitor, and the speaker's
/// own buffer is the timing authority — the pump only has to not run dry.
///
/// Speed (§179) is a shared flag the loop re-reads every chunk: playing at `s` times speed
/// is resampling the source as though it had been recorded at `s` times its rate, so the
/// same samples become a shorter note and the media is never asked for again. The cursor is
/// untouched by a change, so a mid-playback switch continues from the same position at the
/// new pace rather than restarting the note.
///
/// The listened threshold (§179: played to near the end) is crossed at nine-tenths of the
/// note, and reported once — through the loop's own channel, like the ending below, because
/// the marks belong to the loop.
///
/// When the samples run out the pump reports the ending into the worker's own loop (the
/// same self-addressing a chain tracker's completion takes), because the playing state —
/// and the stopped button that turns back into a play button — belongs to the loop.
fn spawn_playback_pump(pump: PlaybackPump) {
    let PlaybackPump {
        frames,
        sink_rate,
        samples,
        rate,
        stop,
        speed,
        commands,
        media_id,
    } = pump;
    std::thread::Builder::new()
        .name("migo-voice-play".to_owned())
        .spawn(move || {
            let mut resampler: Option<call_audio::Resampler> = None;
            let mut resampled: Vec<i16> = Vec::new();
            // A tenth of a second of source audio per chunk, floored at one sample so a
            // pathological rate cannot produce an empty chunk loop.
            let chunk_len = (rate as usize / 10).max(1);
            let mut cursor = 0usize;
            // The pace the loop last adopted, as a whole percent. Zero is "no pace yet", so
            // the first iteration builds the resampler for whatever the note started at —
            // which is the pace the loop handed in, the saved setting included.
            let mut at_percent = 0u32;
            // Just under a chunk of *output* time: the faster the pace, the shorter the wait,
            // because a faster note drains the device in less time per chunk of source.
            let mut pacing = Duration::from_millis(90);
            // Whether the listened threshold has been crossed and reported. The report goes
            // once, at the moment of crossing — a note stopped at nine-tenths was heard to
            // (near) the end, and a note stopped sooner was not.
            let mut heard = false;
            while cursor < samples.len() {
                if stop.load(Ordering::Relaxed) {
                    return;
                }
                let percent = speed.load(Ordering::Relaxed);
                if percent != at_percent {
                    at_percent = percent;
                    let factor = VoiceSpeed::from_percent(percent).factor();
                    let effective = (f64::from(rate) * f64::from(factor)).round() as u32;
                    resampler = (effective != sink_rate)
                        .then(|| call_audio::Resampler::new(effective, sink_rate));
                    pacing = Duration::from_millis((90.0 / f64::from(factor)).round() as u64);
                }
                let end = (cursor + chunk_len).min(samples.len());
                let chunk = &samples[cursor..end];
                let out: &[i16] = match resampler.as_mut() {
                    Some(resampler) => {
                        resampled.clear();
                        resampler.process(chunk, &mut resampled);
                        &resampled
                    }
                    None => chunk,
                };
                if frames.send(out.to_vec()).is_err() {
                    // The speaker was dropped: playback is over, stopped or not.
                    return;
                }
                cursor = end;
                if !heard && cursor * 10 >= samples.len() * 9 {
                    heard = true;
                    let _ = commands.send(Command::VoiceNoteHeard { media_id });
                }
                std::thread::sleep(pacing);
            }
            // The note finished on its own. Tell the loop, unless a stop already did.
            if !stop.load(Ordering::Relaxed) {
                let _ = commands.send(Command::VoiceNoteEnded { media_id });
            }
        })
        .ok();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A deterministic id: the number rendered as the low bytes of a 128-bit id, so members
    /// can be named in tests the way `idOf(n)` names them across the SDK's tests.
    fn id_of(n: u8) -> Id {
        let mut bytes = [0u8; 16];
        bytes[15] = n;
        Id::from_bytes(bytes)
    }

    /// The heartbeat derivation is the keep-alive contract's client half: half the advertised
    /// interval (so a late-firing timer still beats the two-interval deadline), never below
    /// the floor (so a pathological advertisement cannot turn the desktop into a pinger, and
    /// a zero cannot arm a spin).
    #[test]
    fn the_heartbeat_is_half_the_advertised_interval_floored() {
        assert_eq!(
            heartbeat_interval(30_000),
            Duration::from_secs(15),
            "the production advertisement halves to a 15s beat"
        );
        assert_eq!(
            heartbeat_interval(10_000),
            Duration::from_secs(5),
            "the floor catches an aggressive 10s advertisement"
        );
        assert_eq!(
            heartbeat_interval(0),
            MIN_HEARTBEAT,
            "a zero advertisement arms the floor, not a spin"
        );
    }

    /// A frame for the sequencing tests: one opcode, no flags, no body — the shape the
    /// classifier reads, since flags and opcode are the only parts of a header it consults.
    fn frame_of(opcode: Opcode) -> Frame {
        Frame::new(
            migo_wire::FrameHeader::new(opcode.to_wire(), 1),
            bytes::Bytes::new(),
        )
    }

    /// The resume counter's classification is §150's client half, and an off-by-one in it
    /// turns a clean resume into a refused one: Critical frames are counted, Coalescable ones
    /// are not (the server never numbers a frame it may drop), RECONNECT_HINT is not (the
    /// server writes it straight to the transport at graceful shutdown, never through the
    /// mailbox), an error reply is (the ERROR flag rides a Critical header whatever opcode it
    /// answers), and an opcode this build does not know is not (a newer server's frames follow
    /// their own class rules, not this build's guesses).
    #[test]
    fn the_counter_counts_exactly_what_the_server_numbers() {
        assert!(
            is_sequenced(&frame_of(Opcode::MessageEvent)),
            "a Critical event is numbered"
        );
        assert!(
            is_sequenced(&frame_of(Opcode::Sync)),
            "a Critical reply is numbered"
        );
        assert!(
            !is_sequenced(&frame_of(Opcode::Typing)),
            "a Coalescable event is never numbered"
        );
        assert!(
            !is_sequenced(&frame_of(Opcode::PresenceEvent)),
            "presence is Coalescable and never numbered"
        );
        assert!(
            !is_sequenced(&frame_of(Opcode::ReconnectHint)),
            "the hint is written past the mailbox and never numbered"
        );

        let refused = Frame::new(
            migo_wire::FrameHeader::new(Opcode::MessageSend.to_wire(), 1).error(),
            bytes::Bytes::new(),
        );
        assert!(
            is_sequenced(&refused),
            "an error reply rides a Critical header and is numbered"
        );

        let unknown = Frame::new(migo_wire::FrameHeader::new(99_999, 1), bytes::Bytes::new());
        assert!(
            !is_sequenced(&unknown),
            "an opcode this build cannot name is not guessed at"
        );
    }

    /// The resume ask names the session and the count this process actually read — the two
    /// facts the server's ring needs to answer with exactly the frames the outage cost.
    #[test]
    fn the_resume_ask_names_the_session_and_its_own_count() {
        let session = GatewaySession {
            id: id_of(7),
            sequenced: 42,
        };
        let ask = session.resume_request();
        assert_eq!(ask.session_id, id_of(7), "the ask names its session");
        assert_eq!(ask.last_frame_seq, 42, "the ask asks from what was read");
    }

    /// The watermark is a *prefix's* top, not a maximum: it advances only over contiguous
    /// ground (§152's gapless seqs are what make the next brick knowable), stands still
    /// through redeliveries, holds at a hole, and names the hole's top when one opens — a
    /// fill asked for less would leave the hole's tail unfetched.
    #[test]
    fn the_watermark_advances_only_over_contiguous_ground() {
        let mut account = SeqAccount::default();

        // The floor: the first event a conversation ever routes, whatever its seq — history
        // below it may be gone (the server's own Truncated) or simply unfetched.
        assert_eq!(account.track(5), None);
        assert_eq!(account.watermark, 5);

        assert_eq!(account.track(6), None, "the next brick advances the prefix");
        assert_eq!(account.watermark, 6);

        assert_eq!(account.track(4), None, "a redelivery moves nothing");
        assert_eq!(account.watermark, 6);

        assert_eq!(account.track(9), Some(9), "a hole is named as its own top");
        assert_eq!(account.watermark, 6, "the watermark waits at the hole");
        assert_eq!(account.highest_seen, 9);

        // The fill's bricks close the hole without opening another.
        assert_eq!(account.track(7), None);
        assert_eq!(account.track(8), None);
        assert_eq!(
            account.watermark, 9,
            "the hole closed, the prefix stands on its top"
        );
        assert_eq!(account.track(10), None);
        assert_eq!(account.watermark, 10);
    }

    /// The stall is the loop guard: while the watermark stands where an unproductive fill left
    /// it, the server has already been asked and has already answered, so nothing re-asks —
    /// and the moment the watermark moves (the fill's own last brick, or a hole that closed
    /// another way), the stall lifts and later holes may ask again.
    #[test]
    fn a_stall_lifts_only_when_the_watermark_moves() {
        let mut account = SeqAccount::default();
        account.track(10);
        account.stalled_at = Some(10);

        assert_eq!(account.track(12), Some(12), "the new hole is still named");
        assert_eq!(
            account.stalled_at,
            Some(10),
            "a stall survives events that do not close it"
        );

        assert_eq!(account.track(11), None, "the closing brick is no gap");
        assert_eq!(
            account.stalled_at, None,
            "the watermark moving lifts the stall"
        );
        assert_eq!(account.watermark, 12);
    }

    /// A walk continues only while it moves within its bound: a bounded (gap) walk listens to
    /// its own target rather than the conversation's live edge — `more` is the server's word
    /// for history above the page, true whenever the conversation has any, hole filled or not
    /// — an unbounded walk follows `more`, and no walk outlives a page that moved nothing or a
    /// budget spent.
    #[test]
    fn a_walk_continues_only_while_it_moves_within_its_bound() {
        let gap_walk = CatchUp {
            to_seq: Some(20),
            pages_left: 3,
        };
        assert!(
            gap_walk.continues(true, false, 15),
            "a bounded walk walks to its target, not the live edge"
        );
        assert!(
            !gap_walk.continues(true, true, 20),
            "the hole filled, the walk stops rather than tailing live traffic"
        );
        assert!(
            !gap_walk.continues(false, true, 10),
            "a page that moved nothing is the server's answer, not a re-ask"
        );

        let replay = CatchUp {
            to_seq: None,
            pages_left: 2,
        };
        assert!(
            replay.continues(true, true, 400),
            "an unbounded walk follows the server's own word for more"
        );
        assert!(
            !replay.continues(true, false, 400),
            "no history above: the walk reached the live edge"
        );

        let spent = CatchUp {
            to_seq: None,
            pages_left: 0,
        };
        assert!(
            !spent.continues(true, true, 400),
            "the page budget is the bound a long conversation is kept inside"
        );
    }

    /// A `CONVERSATION_MEMBER_EVENT` for one account and change, in the conversation every
    /// membership test here shares.
    fn member_event(
        user: u8,
        change: migo_protocol::MemberChange,
    ) -> migo_protocol::ConversationMemberEvent {
        migo_protocol::ConversationMemberEvent {
            conversation_id: id_of(0x5e),
            user_id: id_of(user),
            change,
            member_count: 2,
            group_key_epoch: None,
        }
    }

    /// The list row's preview is not the membership: a cache seeded from a list row stays
    /// incomplete until the roster promotes it, and a join applied onto the preview does not
    /// promote it — the ninth member of a nine-member group must be reached by the roster the
    /// first send reads, never by the eight the preview happened to name.
    #[test]
    fn a_list_preview_is_promoted_only_by_the_roster() {
        let mut cache = MemberCache {
            ids: (20..=27).map(id_of).collect(),
            complete: false,
        };

        // A join lands before the roster read. The preview patched with it is still a
        // preview: nothing about one more member makes the set whole.
        cache.apply(&member_event(29, migo_protocol::MemberChange::Joined));
        assert!(!cache.complete, "a patched preview stays incomplete");

        // The roster answers with the whole membership — nine members, none of them the
        // premature join above (it arrived before the truth, and the truth wins wholesale).
        cache.promote((20..=28).map(id_of).collect());
        assert!(cache.complete, "the roster promotes");
        assert_eq!(cache.ids, (20..=28).map(id_of).collect::<Vec<_>>());
        assert!(
            !cache.ids.contains(&id_of(29)),
            "the promotion replaces the preview rather than merging into it"
        );
    }

    /// A member event moves a complete cache without a roster re-read: a join adds, a
    /// departure removes, and a presence fact or an unknown change touches nothing. This is
    /// the live-stream half of the fix — the audience follows the group between roster reads.
    #[test]
    fn member_events_move_a_complete_cache() {
        let mut cache = MemberCache {
            ids: (20..=22).map(id_of).collect(),
            complete: true,
        };

        cache.apply(&member_event(29, migo_protocol::MemberChange::Joined));
        assert_eq!(
            cache.ids,
            (20..=22).chain(29..=29).map(id_of).collect::<Vec<_>>()
        );
        assert!(cache.complete, "a patch does not demote a complete cache");

        // A duplicate join is a no-op: the set holds the member already.
        cache.apply(&member_event(29, migo_protocol::MemberChange::Joined));
        assert_eq!(cache.ids.len(), 4);

        cache.apply(&member_event(20, migo_protocol::MemberChange::Left));
        assert!(!cache.ids.contains(&id_of(20)), "a departure is removed");

        // Presence facts and unknown changes are not membership: the set stands.
        cache.apply(&member_event(29, migo_protocol::MemberChange::Disconnected));
        cache.apply(&member_event(29, migo_protocol::MemberChange::Reconnected));
        cache.apply(&member_event(29, migo_protocol::MemberChange::Unknown));
        assert!(cache.ids.contains(&id_of(29)));

        // A kick and a ban depart the member the same way a leave does.
        cache.apply(&member_event(21, migo_protocol::MemberChange::Kicked));
        cache.apply(&member_event(22, migo_protocol::MemberChange::Banned));
        assert_eq!(cache.ids, vec![id_of(29)]);
    }

    /// Only an audience change is a membership change: section 163's rotation trigger fires
    /// for the four changes that move the group's membership, and pointedly not for the
    /// presence pair — a disconnect keeps the key across the gap, and a rotation spent on a
    /// flicker would protect the chain against nobody.
    #[test]
    fn membership_changes_are_the_audience_changes() {
        use migo_protocol::MemberChange;

        for change in [
            MemberChange::Joined,
            MemberChange::Left,
            MemberChange::Kicked,
            MemberChange::Banned,
        ] {
            assert!(
                is_membership_change(change),
                "{change:?} moves the audience and must rotate"
            );
        }
        for change in [
            MemberChange::Unknown,
            MemberChange::Disconnected,
            MemberChange::Reconnected,
        ] {
            assert!(
                !is_membership_change(change),
                "{change:?} leaves the audience standing"
            );
        }
    }

    /// An empty roster answer is not a promotion: a conversation the roster says has nobody in
    /// it is an answer no audience can be built from, and the preview the cache held — whatever
    /// it named — stays, incomplete, until a fuller answer says otherwise.
    #[test]
    fn an_empty_roster_does_not_promote() {
        let preview: Vec<Id> = (20..=22).map(id_of).collect();
        let mut cache = MemberCache {
            ids: preview.clone(),
            complete: false,
        };

        cache.promote(Vec::new());
        assert!(!cache.complete, "an empty answer promotes nothing");
        assert_eq!(cache.ids, preview, "the preview the cache held stands");
    }

    /// The wallet-sync compare folds every written form of one address to the registry's
    /// canonical string: EIP-55 (what derivation holds), prefixed lowercase (what a paste
    /// carries), and the registry's own no-prefix form must all land on the same bytes, or a
    /// registered wallet reads as missing and the sync re-registers it — resurrecting one the
    /// user archived.
    #[test]
    fn canonical_address_folds_every_written_form_together() {
        const CHECKSUMMED: &str = "0x5aAeb6053F3E94C9b9A09f33669435E7Ef1BeAed";
        const LOWERCASE: &str = "5aaeb6053f3e94c9b9a09f33669435e7ef1beaed";
        assert_eq!(canonical_address(CHECKSUMMED), LOWERCASE);
        assert_eq!(canonical_address(&format!("0x{LOWERCASE}")), LOWERCASE);
        assert_eq!(canonical_address(LOWERCASE), LOWERCASE);
        assert_eq!(canonical_address(&format!("  {CHECKSUMMED} ")), LOWERCASE);
    }

    fn keys_of(root: Option<&migo_account::MigoRoot>, account_id: Id) -> DeviceKeys {
        let mut keys = match root {
            Some(root) => DeviceKeys::founding(root),
            None => DeviceKeys::additional(),
        };
        keys.session = Some(SavedSession {
            server_url: "https://migo.example".to_owned(),
            account_id,
            device_id: Id::from_bytes([2; 16]),
            username: "whoever".to_owned(),
            refresh_token: "single-use".to_owned(),
        });
        keys
    }

    /// The tier-one door opens for the account's own root in the account's own vault — the
    /// founding device returning with its file, ratchets and safety number intact.
    #[test]
    fn the_same_account_and_root_open_the_tier_one_door() {
        let root = migo_account::MigoRoot::generate(&mut OsRandom);
        let account_id = Id::from_bytes([1; 16]);
        let keys = keys_of(Some(&root), account_id);
        let root_bytes: [u8; 32] = root.as_bytes().try_into().expect("the root is 32 bytes");

        assert!(vault_holds_this_account(&keys, account_id, root_bytes));
    }

    /// Another account's vault never opens the door, root or no root: the refusal is the point,
    /// not an inconvenience on the way to overwriting a verified identity.
    #[test]
    fn a_different_account_does_not_open_the_tier_one_door() {
        let root = migo_account::MigoRoot::generate(&mut OsRandom);
        let keys = keys_of(Some(&root), Id::from_bytes([1; 16]));
        let root_bytes: [u8; 32] = root.as_bytes().try_into().expect("the root is 32 bytes");

        assert!(!vault_holds_this_account(
            &keys,
            Id::from_bytes([9; 16]),
            root_bytes
        ));
    }

    /// The fingerprint memory's whole judgement in one test: first sight is not a change, the
    /// same key again is not a change, a different key for a device already known is the one
    /// change worth warning about, and the map keeps the new fingerprint either way — so the
    /// warning fires once for a real change, not once per conversation that notices it.
    #[test]
    fn a_peer_fingerprint_changes_only_when_a_known_device_changes_it() {
        let mut seen: HashMap<Id, [u8; 32]> = HashMap::new();
        let device = Id::from_bytes([3; 16]);

        assert!(!peer_fingerprint_changed(&mut seen, device, [1; 32]));
        assert!(!peer_fingerprint_changed(&mut seen, device, [1; 32]));
        assert!(peer_fingerprint_changed(&mut seen, device, [2; 32]));
        // The new fingerprint is what "last seen" now means.
        assert_eq!(seen.get(&device), Some(&[2; 32]));
        // And the change is not re-reported for the same new key.
        assert!(!peer_fingerprint_changed(&mut seen, device, [2; 32]));

        // A second device of the same peer is first sight, not a change.
        assert!(!peer_fingerprint_changed(
            &mut seen,
            Id::from_bytes([4; 16]),
            [3; 32]
        ));
    }

    /// The same account id with a different root is a file that is not this account's at all —
    /// the account id is the container's own claim, and the root is the thing this device can
    /// actually check.
    #[test]
    fn a_different_root_does_not_open_the_tier_one_door() {
        let root = migo_account::MigoRoot::generate(&mut OsRandom);
        let account_id = Id::from_bytes([1; 16]);
        let keys = keys_of(Some(&root), account_id);
        let other: [u8; 32] = root.as_bytes().try_into().expect("the root is 32 bytes");
        let mut other = other;
        other[0] ^= 0xff;

        assert!(!vault_holds_this_account(&keys, account_id, other));
    }

    /// An additional device's vault holds no root at all, so it cannot answer for the container's
    /// one — the refusal keeps such a device's own verified identity in place.
    #[test]
    fn a_vault_without_a_root_does_not_open_the_tier_one_door() {
        let account_id = Id::from_bytes([1; 16]);
        let keys = keys_of(None, account_id);

        assert!(!vault_holds_this_account(&keys, account_id, [7u8; 32]));
    }

    /// A vault with the root but no saved session has the account's material and no device
    /// record: tier one has no username and device id to name the KNOWN device with, so it is
    /// the refusal, not a guess.
    #[test]
    fn a_vault_without_a_session_does_not_open_the_tier_one_door() {
        let root = migo_account::MigoRoot::generate(&mut OsRandom);
        let mut keys = keys_of(Some(&root), Id::from_bytes([1; 16]));
        keys.session = None;
        let root_bytes: [u8; 32] = root.as_bytes().try_into().expect("the root is 32 bytes");

        assert!(!vault_holds_this_account(
            &keys,
            Id::from_bytes([1; 16]),
            root_bytes
        ));
    }

    /// A frame for the batch tests, distinct by correlation so the envelope's element order
    /// can be asserted. `PING` is the shape a server batches least, which is exactly why it
    /// makes an honest element: nothing about the test's envelope depends on the payload type.
    fn batched_frame(correlation: u32) -> Frame {
        Frame::new(
            migo_wire::FrameHeader::new(Opcode::Ping.to_wire(), correlation),
            bytes::Bytes::new(),
        )
    }

    /// A BATCH envelope arrives as one record and dispatches as its elements, in order — the
    /// funnel `on_record` exists for. The elements carry their own correlations, so a reply
    /// inside an envelope is matched exactly like a solo one.
    #[test]
    fn a_batch_envelope_unpacks_to_its_elements_in_order() {
        let elements = vec![batched_frame(11), batched_frame(12), batched_frame(13)];
        let envelope = migo_wire::encode_batch(&elements).expect("the envelope encodes");
        assert!(
            envelope.header.is_batch(),
            "three elements are worth wrapping"
        );

        let unpacked = migo_wire::decode_batch(&envelope).expect("the envelope unpacks");
        let correlations: Vec<u32> = unpacked
            .iter()
            .map(|frame| frame.header.correlation)
            .collect();
        assert_eq!(
            correlations,
            vec![11, 12, 13],
            "the elements keep their order"
        );
    }

    /// A bare frame passes the same funnel untouched — one dispatch path whether a frame
    /// arrived alone or inside a batch, which is the whole point of `decode_batch` handing a
    /// one-element list back for a plain record.
    #[test]
    fn a_bare_frame_passes_the_funnel_as_itself() {
        let solo = batched_frame(21);
        let unpacked = migo_wire::decode_batch(&solo).expect("a bare frame is not an error");
        assert_eq!(unpacked.len(), 1);
        assert_eq!(unpacked[0].header.correlation, 21);
        assert!(
            !unpacked[0].header.is_batch(),
            "the bare frame is not re-wrapped"
        );
    }
}
