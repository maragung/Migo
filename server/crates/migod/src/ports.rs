//! The port adapters a composition root must supply.
//!
//! Four domain crates deliberately ship a *hole* rather than a default: an interface they need
//! but refuse to decide the implementation of, because the choice is deployment policy, not domain
//! logic. This module fills those holes for a running `migod`.
//!
//! - [`FsStorage`] implements [`migo_media::Storage`]: where media bytes actually live. The media
//!   service signs tickets, verifies sizes, and sweeps tombstones, but it never holds a byte —
//!   brief section 168 forbids the server from being a byte proxy. A real deployment points this
//!   at an object store that issues presigned URLs; this filesystem backend is the development
//!   stand-in, honest about being one.
//! - [`StaffRoster`] implements [`migo_moderation::Roster`]: who, globally, is staff. Moderation
//!   asks "what may this account do" and refuses to answer it from room membership; the answer is
//!   an operational directory the composition root owns. The safe default is that nobody is staff.
//! - [`EconomyRewards`] implements [`migo_games::Rewards`] over [`migo_economy::SharedTreasurer`]:
//!   the seam that lets a finished game credit experience and a win confer a badge. It is the one
//!   place two sibling domains (games and economy) meet, and by layering rule they meet only here,
//!   in the composition root, never by depending on each other.
//! - [`EconomyKickTariff`] implements [`migo_messaging::KickTariff`] over
//!   [`migo_economy::SharedTreasurer`]: the seam that lets an outright kick carry its price — a
//!   Kick Point spent first, a coin when there is none, a refusal when the kicker holds neither.
//!   Messaging and economy are siblings too, and they meet only here as well.
//! - [`StoreCallGate`] implements [`migo_calls::CallGate`]: the membership, block, and
//!   social-graph questions the call service must ask before it lets one account ring
//!   another. Calls cannot read those tables themselves — same layering rule, same answer:
//!   the composition root decides, the domain asks.
//! - [`StoreMessageGate`] implements [`migo_messaging::MessageGate`]: the peer-privacy and
//!   room-moderation questions the messaging service must ask before one send is
//!   sequenced. Messaging cannot read the social graph or the room aggregate — the same
//!   layering rule once more, and the same answer.

use std::collections::HashMap;
use std::io::ErrorKind as IoErrorKind;
use std::path::PathBuf;

use async_trait::async_trait;
use tokio::io::AsyncReadExt;

use migo_calls::CallGate;
use migo_core::{Id, Result, Timestamp};
use migo_economy::{Award, Badge, BadgeGrant, SharedTreasurer, Source};
use migo_games::Rewards;
use migo_media::{Grant, Head, Storage, SNIFF_BYTES};
use migo_messaging::KickTariff;
use migo_moderation::{Powers, Roster};
use migo_protocol::fault;

/// A filesystem-backed [`Storage`]: object bytes as files under one root directory.
///
/// This is the development backend. It reads and writes real files, so a locally running node
/// stores and serves media without an object store, but its "signed" URLs carry no signature —
/// there is no secret to sign with and no S3 to honour one. A production node uses the S3 backend,
/// whose URLs are presigned and short-lived. The two are interchangeable behind [`Storage`]
/// precisely so this distinction stays here and never reaches the media service or a client.
///
/// Keys are resolved under the root with a traversal guard: a key is server-generated and flat,
/// but a key that tried to climb out of the media directory is refused rather than followed.
pub struct FsStorage {
    root: PathBuf,
    public_base: String,
}

impl FsStorage {
    /// Builds a filesystem backend rooted at `root`, minting URLs under `public_base`.
    ///
    /// `public_base` is the externally reachable prefix a client fetches media from — typically
    /// the node's public URL with a media path. A trailing slash is normalised away so joining a
    /// key never doubles it.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>, public_base: impl Into<String>) -> Self {
        let mut public_base = public_base.into();
        while public_base.ends_with('/') {
            public_base.pop();
        }
        Self {
            root: root.into(),
            public_base,
        }
    }

    /// Resolves a storage key to a path under the root, refusing anything that could escape it.
    ///
    /// Storage keys come from [`migo_media::storage_key`] and are flat and safe by construction;
    /// this is defence in depth, so that a future key scheme, or a bug that let a client influence
    /// a key, cannot turn into a write outside the media directory.
    fn resolve(&self, key: &str) -> Result<PathBuf> {
        let unsafe_segment = key
            .split('/')
            .any(|segment| segment.is_empty() || segment == "." || segment == "..");
        if key.is_empty() || key.starts_with('/') || unsafe_segment {
            return Err(fault::storage(format!("unsafe storage key: {key}")));
        }
        Ok(self.root.join(key))
    }

    /// The URL a client uses to reach a key. Dev-only: it is unsigned (see the type docs).
    fn url_for(&self, key: &str) -> String {
        format!("{}/{}", self.public_base, key)
    }
}

#[async_trait]
impl Storage for FsStorage {
    async fn sign_upload(&self, key: &str, byte_size: u64, expires_at: Timestamp) -> Result<Grant> {
        // A filesystem needs the parent directory to exist before a write to the key can land; an
        // object store needs no such step. `byte_size` is unused here because this backend cannot
        // bind a signature to a content length — a production backend does.
        let _ = byte_size;
        let path = self.resolve(key)?;
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|error| fault::storage(error.to_string()))?;
        }
        Ok(Grant::new(self.url_for(key), expires_at))
    }

    async fn sign_download(&self, key: &str, expires_at: Timestamp) -> Result<Grant> {
        Ok(Grant::new(self.url_for(key), expires_at))
    }

    async fn head(&self, key: &str, head_len: usize) -> Result<Option<Head>> {
        let path = self.resolve(key)?;
        let mut file = match tokio::fs::File::open(&path).await {
            Ok(file) => file,
            Err(error) if error.kind() == IoErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(fault::storage(error.to_string())),
        };
        let byte_size = file
            .metadata()
            .await
            .map_err(|error| fault::storage(error.to_string()))?
            .len();
        let want = head_len.min(SNIFF_BYTES);
        let mut head = [0u8; SNIFF_BYTES];
        let mut filled = 0;
        while filled < want {
            match file
                .read(&mut head[filled..want])
                .await
                .map_err(|error| fault::storage(error.to_string()))?
            {
                0 => break,
                read => filled += read,
            }
        }
        Ok(Some(Head {
            byte_size,
            head,
            head_len: filled,
        }))
    }

    async fn uploaded_bytes(&self, key: &str) -> Result<Option<u64>> {
        let path = self.resolve(key)?;
        match tokio::fs::metadata(&path).await {
            Ok(metadata) => Ok(Some(metadata.len())),
            Err(error) if error.kind() == IoErrorKind::NotFound => Ok(None),
            Err(error) => Err(fault::storage(error.to_string())),
        }
    }

    async fn remove(&self, key: &str) -> Result<()> {
        let path = self.resolve(key)?;
        match tokio::fs::remove_file(&path).await {
            Ok(()) => Ok(()),
            // The trait requires removing a key that holds nothing to succeed: both callers (the
            // sweeper and the scan pipeline) can legitimately run twice.
            Err(error) if error.kind() == IoErrorKind::NotFound => Ok(()),
            Err(error) => Err(fault::storage(error.to_string())),
        }
    }
}

/// The global staff directory: which accounts hold moderator [`Powers`], and how much.
///
/// Moderation calls [`Roster::powers`] on every operator request and treats the absence of a grant
/// as [`Powers::NONE`] — an ordinary account — never as an error. This implementation answers from
/// an in-memory map the composition root builds. The development default is [`empty`](Self::empty):
/// no account is staff, so every operator action is refused, which is the correct posture for a
/// node with no configured staff.
pub struct StaffRoster {
    staff: HashMap<Id, Powers>,
}

impl StaffRoster {
    /// Builds a roster from an explicit account-to-powers map.
    #[must_use]
    pub fn new(staff: HashMap<Id, Powers>) -> Self {
        Self { staff }
    }

    /// A roster in which nobody is staff. Every account resolves to [`Powers::NONE`].
    #[must_use]
    pub fn empty() -> Self {
        Self {
            staff: HashMap::new(),
        }
    }
}

#[async_trait]
impl Roster for StaffRoster {
    async fn powers(&self, account_id: Id) -> Result<Powers> {
        Ok(self.staff.get(&account_id).copied().unwrap_or(Powers::NONE))
    }
}

/// Bridges game outcomes into the economy: the [`Rewards`] port backed by the [`Treasurer`].
///
/// Games and economy are sibling domains and, by the layering rule, must not depend on each other.
/// They meet only here. A finished game credits experience through
/// [`award_experience`](Rewards::award_experience), which becomes an economy [`Award`] tagged
/// [`Source::Game`]; a win becomes a [`Badge::GameChampion`] grant. Both carry the game id so the
/// economy can make the credit idempotent and a replayed award adds nothing twice.
///
/// [`Treasurer`]: migo_economy::Treasurer
pub struct EconomyRewards {
    treasurer: SharedTreasurer,
}

impl EconomyRewards {
    /// Wraps a treasurer as the games reward sink.
    #[must_use]
    pub fn new(treasurer: SharedTreasurer) -> Self {
        Self { treasurer }
    }
}

#[async_trait]
impl Rewards for EconomyRewards {
    async fn award_experience(
        &self,
        account_id: Id,
        amount: i64,
        game_id: Id,
        at: Timestamp,
    ) -> Result<()> {
        self.treasurer
            .award(Award {
                account_id,
                source: Source::Game,
                amount,
                ref_id: Some(game_id),
                idempotency_key: Some(format!("game-xp-{game_id}")),
                at,
            })
            .await?;
        Ok(())
    }

    async fn mark_winner(&self, account_id: Id, game_id: Id, at: Timestamp) -> Result<()> {
        self.treasurer
            .award_badge(BadgeGrant {
                account_id,
                badge: Badge::GameChampion,
                ref_id: Some(game_id),
                at,
            })
            .await?;
        Ok(())
    }
}

/// Implements [`migo_messaging::KickTariff`] over [`migo_economy::SharedTreasurer`]:
/// the seam that lets a founder's outright kick carry its price. It is the one place
/// messaging and economy meet, and by the layering rule they meet only here, in the
/// composition root — the same answer the games seam above gives, never a dependency
/// between the sibling crates.
///
/// The adapter is thin on purpose: the treasurer's own
/// [`charge_kick`](migo_economy::Treasurer::charge_kick) owns the pricing order (a
/// Kick Point spent before a coin is asked, a refusal when the kicker holds neither)
/// and the idempotency that keeps a retried kick from paying twice. All this adapter
/// adds is the `Result` that says "the price was settled"; the `KickCharge` detail the
/// treasurer returns is the ledger's business, not the kick's.
pub struct EconomyKickTariff {
    treasurer: SharedTreasurer,
}

impl EconomyKickTariff {
    /// Wraps a treasurer as the messaging kick tariff.
    #[must_use]
    pub fn new(treasurer: SharedTreasurer) -> Self {
        Self { treasurer }
    }
}

#[async_trait]
impl KickTariff for EconomyKickTariff {
    async fn charge_kick(
        &self,
        kicker: Id,
        conversation_id: Id,
        target_id: Id,
        at: Timestamp,
    ) -> Result<()> {
        self.treasurer
            .charge_kick(kicker, conversation_id, target_id, at)
            .await
            .map(|_| ())
    }
}

// --- the media data plane ---------------------------------------------------------
//
// The API's byte routes (PUT/GET under the public media path) need exactly two
// operations on the filesystem backend — write the bytes where the key says, read them
// back — and the media service's own `Storage` port deliberately has neither: grants and
// heads are ticketing concerns, not transport. Implementing `MediaFiles` here keeps the
// traversal rule in one place, next to the resolver it mirrors.

#[async_trait]
impl migo_api::MediaFiles for FsStorage {
    async fn write(&self, key: &str, bytes: bytes::Bytes) -> migo_core::Result<()> {
        use tokio::io::AsyncWriteExt as _;

        let path = self.resolve(key)?;
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|error| fault::storage(error.to_string()))?;
        }
        let mut file = tokio::fs::File::create(&path)
            .await
            .map_err(|error| fault::storage(error.to_string()))?;
        file.write_all(&bytes)
            .await
            .map_err(|error| fault::storage(error.to_string()))?;
        file.flush()
            .await
            .map_err(|error| fault::storage(error.to_string()))?;
        Ok(())
    }

    async fn read(&self, key: &str) -> migo_core::Result<bytes::Bytes> {
        let path = self.resolve(key)?;
        let bytes = tokio::fs::read(&path).await.map_err(|error| {
            if error.kind() == IoErrorKind::NotFound {
                fault::not_found("media object")
            } else {
                fault::storage(error.to_string())
            }
        })?;
        Ok(bytes::Bytes::from(bytes))
    }
}

// --- the calls service's questions -------------------------------------------------
//
// `migo-calls` refuses to read the membership and block tables itself: by the
// layering rule it cannot depend on the crates that own them, and a call is
// the one request whose every rule is about *somebody else*. It asks through
// the `CallGate` port instead, and this adapter answers from the store and
// the social graph.

/// Answers the call service's gate questions from the process's own store and
/// social graph.
///
/// Membership and blocks come from the store. The third question — whether
/// the callee's own policy admits the caller — belongs to the social graph,
/// which `migo-calls` cannot reach by the same layering rule that keeps it
/// off the store, so the graph is held here and asked through
/// [`Graph::may_interact`](migo_social::Graph::may_interact) with the caller
/// the call service already proved.
///
/// Every question fails closed, in the direction that refuses contact: a
/// store that cannot answer a membership question is a store that has not
/// said "member", one that cannot answer a block question is a store that has
/// not said "not blocked", and a graph that cannot answer the policy question
/// is a graph that has not said "allowed". An outage then costs a call
/// invitation, never a ring through a policy.
pub struct StoreCallGate {
    store: migo_store::SharedStore,
    social: migo_social::SharedSocial,
}

impl StoreCallGate {
    /// Wraps the store and the social graph the composition root already
    /// opened.
    #[must_use]
    pub fn new(store: migo_store::SharedStore, social: migo_social::SharedSocial) -> Self {
        Self { store, social }
    }
}

#[async_trait]
impl CallGate for StoreCallGate {
    async fn may_invite(&self, conversation_id: Id, caller_id: Id) -> bool {
        self.store
            .is_member(conversation_id, caller_id)
            .await
            .unwrap_or(false)
    }

    async fn blocked_either_way(&self, a: Id, b: Id) -> bool {
        self.store.is_blocked_either_way(a, b).await.unwrap_or(true)
    }

    async fn can_call(&self, caller: &migo_calls::Caller, callee_id: Id) -> bool {
        // The social graph asks its questions of its own `Caller`, built here
        // from the one the call service already proved: the same account, the
        // same device, the same tier, and the same sampled `now`, so the
        // graph answers for exactly this request rather than a reconstruction
        // of it. `request_id` is the correlation a social opcode would carry;
        // the gate has no frame of its own to correlate.
        let who =
            migo_social::Caller::new(caller.account_id, caller.device_id, caller.tier, caller.now);
        self.social
            .may_interact(&who, callee_id, migo_social::Interaction::Call)
            .await
            .is_ok()
    }
}

// --- the messaging service's questions ----------------------------------------------
//
// `migo-messaging` refuses to read the social graph or the room aggregate: the first
// owns "whose messages does this person accept", the second owns the moderation
// ladder a room's conversation is sent into. It asks through the `MessageGate` port
// instead, and this adapter answers from the graph and the room service the
// composition root already opened.

/// Answers the messaging service's gate questions from the process's own social
/// graph and room aggregate.
///
/// The privacy question delegates to [`Graph::may_interact`](migo_social::Graph::may_interact)
/// with [`Interaction::Message`](migo_social::Interaction::Message), which is the same gate
/// every other kind of contact already passes — a direct message must not be the one path that
/// skips it. The room question delegates to
/// [`Roomkeeper::authorize`](migo_rooms::Roomkeeper::authorize) with the `CHAT_SEND` bit,
/// which walks the room's own ladder (membership, ban, mute, permission) and
/// returns its interval.
///
/// Both questions fail closed, exactly as the port's contract requires: a graph
/// or room that cannot answer is one that has not said "allowed", and the send
/// is refused rather than sequenced past the refusal.
pub struct StoreMessageGate {
    store: migo_store::SharedStore,
    social: migo_social::SharedSocial,
    rooms: migo_rooms::SharedRooms,
}

impl StoreMessageGate {
    /// Wraps the store, social graph, and room service the composition root
    /// already opened.
    #[must_use]
    pub fn new(
        store: migo_store::SharedStore,
        social: migo_social::SharedSocial,
        rooms: migo_rooms::SharedRooms,
    ) -> Self {
        Self {
            store,
            social,
            rooms,
        }
    }
}

#[async_trait]
impl migo_messaging::MessageGate for StoreMessageGate {
    async fn can_message(&self, caller: &migo_messaging::Caller, peer_id: Id) -> bool {
        // The graph's own `Caller`, rebuilt from the one messaging proved, for
        // the same reason `StoreCallGate::can_call` rebuilds it: the graph
        // answers for exactly this request. The block case never reaches here
        // — the send path has already refused it with its own code — so
        // everything this `false` withholds is privacy, and `is_ok()` folds
        // the graph's own refusal codes into the one answer the port allows.
        let who =
            migo_social::Caller::new(caller.account_id, caller.device_id, caller.tier, caller.now);
        self.social
            .may_interact(&who, peer_id, migo_social::Interaction::Message)
            .await
            .is_ok()
    }

    async fn room_speak(
        &self,
        conversation_id: Id,
        caller: &migo_messaging::Caller,
    ) -> migo_messaging::RoomSpeak {
        use migo_protocol::codes;

        // The conversation a room owns resolves to the room first: the ladder
        // is asked about a room, and a conversation no room claims is one no
        // member may speak into — a state no client can produce, and one the
        // port's fail-closed contract answers before the ladder is reached.
        let Ok(Some(room)) = self.store.room_by_conversation(conversation_id).await else {
            return migo_messaging::RoomSpeak::NotMember;
        };
        // The room aggregate's own ladder, asked with the bit a message send
        // needs. Its refusal codes are already the port's vocabulary.
        let who =
            migo_rooms::Caller::new(caller.account_id, caller.device_id, caller.tier, caller.now);
        match self
            .rooms
            .authorize(&who, room.room_id, migo_rooms::permission::CHAT_SEND)
            .await
        {
            Ok(authorized) => migo_messaging::RoomSpeak::Allowed {
                slow_mode_seconds: authorized.slow_mode_seconds.max(0) as u32,
            },
            Err(error) => match error.code() {
                codes::MUTED => migo_messaging::RoomSpeak::Muted,
                codes::NOT_A_MEMBER | codes::BANNED => migo_messaging::RoomSpeak::NotMember,
                // `PERMISSION_DENIED` and anything the ladder could not answer:
                // a refusal the port cannot name is still a refusal, and speech
                // is the side that fails closed.
                _ => migo_messaging::RoomSpeak::Denied,
            },
        }
    }
}

// --- the economy's notifications --------------------------------------------------
//
// `migo-economy` tells the world what happened through its `Announcer` port; the
// composition root decides what telling means here. This adapter stores a notification
// row for every announcement — the inbox half. The realtime half (a `NOTIFICATION_EVENT`
// broadcast) cannot live here: the port fires inside the service, mid-transaction, with
// no connection context to publish from, so the dispatcher publishes it for the paths a
// user can watch (gifts) and everything else waits in the inbox until the client asks.

/// An [`Announcer`](migo_economy::Announcer) that stores every announcement as a
/// notification row.
pub struct NotifyingAnnouncer {
    notifier: migo_notify::SharedNotifier,
}

impl NotifyingAnnouncer {
    /// Builds the adapter over the process's notifier.
    #[must_use]
    pub fn new(notifier: migo_notify::SharedNotifier) -> Self {
        Self { notifier }
    }
}

#[async_trait]
impl migo_economy::Announcer for NotifyingAnnouncer {
    async fn announce(&self, announcement: migo_economy::Announcement) -> Result<()> {
        let event = migo_notify::Event {
            account_id: announcement.account_id,
            kind: announcement.kind,
            actor_id: announcement.actor_id,
            room_id: None,
            subject_id: announcement.subject_id,
            at: announcement.at,
        };
        // The notifier treats a delivery failure as Ok (logged, counted); an Err here
        // is the store being broken, and the economy's contract says that is logged and
        // swallowed — the gift is recorded, the balance is right, and the row is what a
        // missing buzz costs.
        if let Err(error) = self.notifier.notify(event).await {
            tracing::warn!(code = error.code(), "economy notification dropped");
        }
        Ok(())
    }
}
