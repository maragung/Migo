//! The rooms service.
//!
//! # The five invariants
//!
//! **Deny wins, and it wins last.** Effective permissions are the role default plus
//! the per-member grant minus the per-member deny, in that order, computed in one
//! place ([`crate::permission::resolve`]). A moderator who takes `CHAT_SEND` away
//! from somebody needs that to hold whatever else the member accumulates, or the
//! moderation tool is advisory.
//!
//! **A ban is visible before a departure is.** Every standing check reads the ban
//! before it reads `left_at`, because a banned member is also a member who left — the
//! store sets both — and checking departure first would report `NOT_A_MEMBER` and hide
//! the ban from the person it was applied to and from the operator reading the logs.
//!
//! **Nothing is sent when nothing changed.** Brief section 156. Joining a room this
//! account is already in, submitting a settings screen without touching it, and
//! setting a role to the one already held all return `None` instead of a frame.
//!
//! **The owner is not a rank.** No sanction, no role change, and no permission
//! override can touch the owner, by anybody, at any level. There is nothing above
//! `Owner` for [`crate::permission::outranks`] to compare against, so the only way to
//! allow it would be a special case — and a special case here is how a room gets taken
//! from the person who made it.
//!
//! **A refusal names what happened.** `NOT_A_MEMBER`, `BANNED`, `MUTED`, and
//! `PERMISSION_DENIED` are four codes rather than one, because a client that cannot
//! tell "you were banned an hour ago" from "you cannot pin messages" cannot say
//! anything useful to the person holding the phone.
//!
//! # Why `authorize` is not rate limited
//!
//! Every other method here charges the limiter; that one deliberately does not. It is
//! called by another domain on the path of an operation that has already been charged
//! — a send into a room charges `MessageSend`, then asks this crate whether the account
//! may send — and charging again would bill one user action twice and make a room's
//! send budget depend on how many permission checks the implementation happens to do.
//!
//! # Why four costs are named here and not in the IDL
//!
//! ADR-0006 puts the price of an operation on its opcode, and five of these methods
//! have no opcode: brief section 145 gives rooms only `RoomJoin`, `RoomLeave`,
//! `RoomList`, and the two events. Creating, updating, moderating, and reading a room
//! are REST surface. Adding opcodes for them so they could carry a price would put
//! five frames in the packet registry that no socket will ever send, so the costs are
//! constants in this file, next to the code that spends them.
//!
//! # What this crate does not do
//!
//! It does not deliver frames, count who is online, assign sequence numbers, or
//! enforce slow mode. [`crate::traits`] records why for each one.

use std::sync::Arc;

use async_trait::async_trait;
use migo_core::metrics::Registry;
use migo_core::{Id, OsRandom, Random, Result, Timestamp};
use migo_protocol::{
    codes, fault, Opcode, RoomJoinRequest, RoomJoinResponse, RoomKind, RoomLeaveRequest,
    RoomListRequest, RoomListResponse, RoomMemberEvent, RoomRole, RoomSummary,
};
use migo_ratelimit::{BucketKey, RateLimiter, SharedRateLimiter};
use migo_store::model::{join_policy, NewRoom, Patch, Room, RoomMember};
use migo_store::traits::{MessagingStore, RoomStore};
use migo_store::{SharedStore, Store};
use parking_lot::Mutex;

use crate::fanout::Fanout;
use crate::metrics::{
    AuthorizeOutcome, ChangeOutcome, CreateOutcome, JoinOutcome, Meters, SanctionKind,
};
use crate::model::{
    slug_is_valid, Authorized, Caller, NewRoomRequest, RoomsConfig, Sanction, Settings,
    TopicChange, DEFAULT_LIST_LIMIT, MAX_LIST_LIMIT, MAX_MUTE_MS, MAX_NAME_LEN, MAX_QUERY_LEN,
    MAX_REASON_LEN, MAX_ROSTER_PAGE, MAX_SLOW_MODE_SECONDS, MAX_TOPIC_LEN, MIN_ROOM_CAPACITY,
    PERMANENT_BAN_MS,
};
use crate::permission;
use crate::traits::Roomkeeper;
use crate::view;

/// A shared, fully erased rooms service.
pub type SharedRooms = Arc<dyn Roomkeeper>;

/// What creating a room costs.
///
/// Ten times a join. A room is a permanent object with a namespace entry, a
/// conversation, and a slug nobody else can ever take, and the cheap version of this
/// number is a slug-squatting script.
const CREATE_COST: u32 = 50;

/// What one moderation action costs.
///
/// Cheap enough that clearing up a raid is not itself rate limited, and not free,
/// because a compromised moderator session should run out of budget before it gets
/// through the roster.
const MODERATION_COST: u32 = 10;

/// What one settings change costs.
const SETTINGS_COST: u32 = 10;

/// What one room read costs.
const READ_COST: u32 = 5;

/// Rooms examined before a search stops looking.
///
/// A search here is a scan of the browse ordering, because `rooms` has no text index —
/// `docs/04-data-model.md` does not create one, and inventing a `LIKE '%term%'` over a
/// growing table would be a sequential scan dressed as a feature. The bound is what
/// keeps the cost of a miss constant: two hundred rows, the store's own page ceiling,
/// and then an honest short answer.
const MAX_SEARCH_SCAN: u16 = 200;

/// The sequence reported for a conversation that has no messages yet.
const NO_MESSAGES_YET: u64 = 0;

/// Rooms over a store and a rate limiter.
///
/// No cache parameter, unlike presence and messaging. Nothing here is cached: a
/// membership row decides whether somebody may speak, and a stale copy of it is a
/// member who was banned two minutes ago and is still talking. Brief section 173 says
/// losing the cache must lose nothing that matters, and the way to honour that for a
/// permission is not to put it there.
pub struct Rooms<S: ?Sized = dyn Store, L: ?Sized = dyn RateLimiter> {
    store: Arc<S>,
    limiter: Arc<L>,
    /// The randomness source, behind a lock because [`Random`] is `Send` and not
    /// `Sync`.
    ///
    /// Used for two ids per created room and nothing else. The lock is never held
    /// across an `await`, which is what keeps a mutex off a scheduler's critical
    /// path.
    random: Mutex<Box<dyn Random>>,
    config: RoomsConfig,
    meters: Meters,
}

/// Builds the rooms service the composition root hands around.
///
/// Takes a [`RoomsConfig`] because one field in it cannot be defaulted: the home
/// region is stamped onto every room at creation and decides which node sequences it
/// forever (brief section 54). A default of `"local"` in production would create
/// rooms claiming a region no process in the deployment sequences.
#[must_use]
pub fn open(
    store: SharedStore,
    limiter: SharedRateLimiter,
    registry: &Registry,
    config: RoomsConfig,
) -> SharedRooms {
    Arc::new(Rooms::new(
        store,
        limiter,
        registry,
        config,
        Box::new(OsRandom) as Box<dyn Random>,
    ))
}

impl<S, L> Rooms<S, L>
where
    S: RoomStore + MessagingStore + ?Sized,
    L: RateLimiter + ?Sized,
{
    /// Assembles the service and registers every series at zero.
    ///
    /// `random` is injected rather than fixed to [`OsRandom`] so a simulation can
    /// replay a run byte for byte (ADR-0009).
    pub fn new(
        store: Arc<S>,
        limiter: Arc<L>,
        registry: &Registry,
        config: RoomsConfig,
        random: Box<dyn Random>,
    ) -> Self {
        Self {
            store,
            limiter,
            random: Mutex::new(random),
            config,
            meters: Meters::new(registry),
        }
    }

    // --- shared plumbing -------------------------------------------------------

    /// Charges an opcode-priced operation against the caller's own surfaces.
    ///
    /// Two buckets, tightest first, and neither of them the room.
    ///
    /// A room-scoped bucket is right for messages — it protects a room's members from
    /// one loud participant — and wrong here. A shared bucket on the join path is a
    /// denial of service with a two-line script: join and leave a popular room until
    /// its bucket is empty, and nobody else can get in. The account is the surface
    /// that should run out.
    async fn charge(&self, caller: &Caller, opcode: Opcode) -> Result<()> {
        let keys = [
            BucketKey::endpoint_of_account(caller.account_id, opcode),
            BucketKey::account(caller.account_id),
        ];
        // `charge_opcode` and not `charge`: ADR-0006 puts the price on the opcode, so
        // naming the opcode makes it impossible to charge the wrong amount, and a
        // reprice in the IDL takes effect here without an edit.
        self.limiter
            .charge_opcode(&keys, opcode, caller.tier, caller.now)
            .await?
            .into_result()
    }

    /// Charges an operation that has no opcode to be priced from.
    ///
    /// One bucket, the account's, because there is no endpoint identity to open a
    /// second one under. See the module docs for why these prices are constants.
    async fn charge_flat(&self, caller: &Caller, cost: u32) -> Result<()> {
        let keys = [BucketKey::account(caller.account_id)];
        self.limiter
            .charge(&keys, cost, caller.tier, caller.now)
            .await?
            .into_result()
    }

    /// Refuses a caller that is not fully identified.
    ///
    /// The gateway never produces one. It is checked anyway because a nil account id
    /// would be a membership row shared by every unauthenticated request, and a nil
    /// device id would make the fanout exclusion match somebody else's socket.
    fn require_identity(caller: &Caller) -> Result<()> {
        if caller.account_id.is_nil() || caller.device_id.is_nil() {
            return Err(fault::unauthenticated(
                "rooms need an identified account and device",
            ));
        }
        Ok(())
    }

    /// A fresh id stamped with `now`.
    fn new_id(&self, now: Timestamp) -> Id {
        let mut random = self.random.lock();
        Id::generate_at(now, &mut **random)
    }

    /// The room, or `NOT_FOUND`.
    ///
    /// An archived room still resolves here. Brief section 85 archives rather than
    /// deletes precisely so that links and history keep working, and the methods that
    /// must refuse an archived room refuse it by name.
    async fn load_room(&self, room_id: Id) -> Result<Room> {
        self.store
            .room(room_id)
            .await?
            .ok_or_else(|| fault::not_found("room"))
    }

    /// The one permission decision in this crate.
    ///
    /// Order is the contract, and each step is ahead of the next for a reason:
    ///
    /// 1. **No row at all** is `NOT_A_MEMBER`.
    /// 2. **A live ban** is `BANNED`, checked before departure because the store sets
    ///    `left_at` when it bans and the ban is the more specific truth.
    /// 3. **A row that left** is `NOT_A_MEMBER`.
    /// 4. **A live mute** is `MUTED`, but only when the action being attempted is one
    ///    a mute withholds — a muted member may still read, list, and join a call.
    ///    Ahead of the permission check so that a muted moderator is told they are
    ///    muted rather than that they lack a bit they plainly hold.
    /// 5. **A missing bit** is `PERMISSION_DENIED`.
    ///
    /// `needed = 0` is the membership-only form: [`permission::allows`] with an empty
    /// mask is always true, and no mask intersects the silenced set, so the call
    /// refuses a stranger and a banned account and nothing else.
    async fn require(&self, caller: &Caller, room: &Room, needed: u64) -> Result<Authorized> {
        let Some(member) = self
            .store
            .room_member(room.room_id, caller.account_id)
            .await?
        else {
            self.meters.authorize(AuthorizeOutcome::NotAMember);
            return Err(Self::not_a_member());
        };
        if member.is_banned(caller.now) {
            self.meters.authorize(AuthorizeOutcome::Banned);
            return Err(Self::banned());
        }
        if !member.is_active() {
            self.meters.authorize(AuthorizeOutcome::NotAMember);
            return Err(Self::not_a_member());
        }
        let permissions = permission::resolve(
            member.role,
            member.permissions_grant,
            member.permissions_deny,
        );
        if member.is_muted(caller.now) && needed & permission::SILENCED_BY_MUTE != 0 {
            self.meters.authorize(AuthorizeOutcome::Muted);
            return Err(Self::muted());
        }
        if !permission::allows(permissions, needed) {
            self.meters.authorize(AuthorizeOutcome::Denied);
            return Err(fault::permission_denied("the room permission is not held"));
        }
        self.meters.authorize(AuthorizeOutcome::Granted);
        Ok(Authorized {
            room_id: room.room_id,
            conversation_id: room.conversation_id,
            kind: room.kind,
            role: member.role,
            permissions,
            // A member who can moderate is not slow-moded. Reported as zero rather
            // than enforced-with-an-exception, so the crate that applies the interval
            // needs one rule and not two: the moderator's instruction to calm down is
            // the message most likely to be needed twice inside the window.
            slow_mode_seconds: if permission::allows(permissions, permission::ROOM_MODERATE) {
                0
            } else {
                room.slow_mode_seconds
            },
        })
    }

    /// The preamble every action against another member shares.
    ///
    /// Four checks in one place, because a moderation path that skipped one of them
    /// would be the path somebody found.
    ///
    /// The subject is **not** required to be active. Banning an account that just left
    /// is the ordinary case — somebody leaves ahead of the consequence — and a version
    /// that demanded a live membership would make leaving a way to avoid a ban.
    async fn require_over(
        &self,
        caller: &Caller,
        room: &Room,
        subject_id: Id,
        needed: u64,
    ) -> Result<(Authorized, RoomMember)> {
        if subject_id.is_nil() {
            return Err(fault::validation("subject_id", "an account id is required"));
        }
        if subject_id == caller.account_id {
            return Err(fault::conflict(
                "a moderation action cannot be aimed at its own actor",
            ));
        }
        let actor = self.require(caller, room, needed).await?;
        let subject = self
            .store
            .room_member(room.room_id, subject_id)
            .await?
            .ok_or_else(|| fault::not_found("room member"))?;
        // Both, and not just the role: a row whose role drifted from the room's owner
        // column would otherwise be a way through, and the two disagreeing is exactly
        // the state a half-finished transfer would leave behind.
        if subject.role == RoomRole::Owner || subject_id == room.owner_id {
            return Err(fault::permission_denied(
                "the room owner cannot be acted on",
            ));
        }
        if !permission::outranks(actor.role, subject.role) {
            return Err(fault::permission_denied(
                "the target's role is not below the actor's",
            ));
        }
        Ok((actor, subject))
    }

    /// This account's role in the room, for a summary, or `None`.
    ///
    /// `None` for a stranger, for somebody who left, and for a banned account. A
    /// listing that showed a banned member their old role would be telling them the
    /// ban had not happened.
    async fn role_in(&self, room: &Room, account_id: Id) -> Result<Option<RoomRole>> {
        let Some(member) = self.store.room_member(room.room_id, account_id).await? else {
            return Ok(None);
        };
        Ok(Some(member.role).filter(|_| member.is_active()))
    }

    /// The member count as it stands now, re-read after a write that moved it.
    ///
    /// A second read rather than arithmetic on the value from before. The count is
    /// derived and the store owns it; adding one here would produce a number that is
    /// right until two people join at once, which is the traffic a popular room is
    /// made of.
    async fn current_count(&self, room_id: Id) -> Result<u32> {
        Ok(self
            .store
            .room(room_id)
            .await?
            .map_or(0, |room| view::count(room.member_count)))
    }

    // --- refusals --------------------------------------------------------------
    //
    // Constructors rather than inline calls, so the four codes this crate can produce
    // are visible in one place and so no message ever grows a detail it should not
    // carry. None of them includes the moderator-written reason: brief section 174
    // keeps free text out of logs, and an error's internal message is a log line.

    /// Not in the room, or never was.
    fn not_a_member() -> migo_core::Error {
        fault::error(
            codes::NOT_A_MEMBER,
            "the account is not a member of the room",
        )
    }

    /// Banned, with the reason left out on purpose.
    ///
    /// The reason belongs in the response body a REST handler builds for the banned
    /// account, read from the membership row at that point. Putting it here would put
    /// text an annoyed moderator typed into every log line and every trace that
    /// touches this error.
    fn banned() -> migo_core::Error {
        fault::error(codes::BANNED, "the account is banned from the room")
    }

    /// Muted, for an action a mute withholds.
    fn muted() -> migo_core::Error {
        fault::error(codes::MUTED, "the account is muted in the room")
    }

    /// At capacity.
    fn room_full() -> migo_core::Error {
        fault::error(codes::ROOM_FULL, "the room is at capacity")
    }

    /// Archived, so it takes no more members and no more settings.
    fn room_archived() -> migo_core::Error {
        fault::error(codes::ROOM_ARCHIVED, "the room is archived")
    }

    // --- validation ------------------------------------------------------------

    /// Everything that can be refused about a new room without touching the store.
    ///
    /// Ahead of the rate limiter in every caller, because a malformed request is a
    /// client bug and charging for it would let one broken build exhaust its user's
    /// allowance and take their working devices down with it.
    fn validate_new(request: &NewRoomRequest, config: &RoomsConfig) -> Result<()> {
        if !slug_is_valid(&request.slug) {
            return Err(fault::validation(
                "slug",
                "3 to 32 characters of lowercase letters, digits, and single interior hyphens",
            ));
        }
        let name = request.name.trim();
        if name.is_empty() {
            return Err(fault::field_required("name"));
        }
        // `chars().count()` and not `len()`: the limit is what a person typed, and a
        // byte limit would give an Indonesian or Arabic name half the room of an
        // English one for no reason a user could discover.
        if name.chars().count() > MAX_NAME_LEN {
            return Err(fault::field_too_long("name", MAX_NAME_LEN));
        }
        if let Some(topic) = &request.topic {
            if topic.chars().count() > MAX_TOPIC_LEN {
                return Err(fault::field_too_long("topic", MAX_TOPIC_LEN));
            }
        }
        if request.kind == RoomKind::Unknown {
            return Err(fault::validation(
                "kind",
                "a room is public or managed; this build has no third kind",
            ));
        }
        if let Some(max) = request.max_members {
            if max < MIN_ROOM_CAPACITY {
                return Err(fault::validation(
                    "max_members",
                    "a room holds at least two people",
                ));
            }
            if max > config.max_members_ceiling {
                return Err(fault::validation(
                    "max_members",
                    "above this deployment's ceiling",
                ));
            }
        }
        Ok(())
    }

    /// Everything that can be refused about a settings change.
    fn validate_settings(settings: &Settings) -> Result<()> {
        if let Some(name) = &settings.name {
            let name = name.trim();
            if name.is_empty() {
                return Err(fault::field_required("name"));
            }
            if name.chars().count() > MAX_NAME_LEN {
                return Err(fault::field_too_long("name", MAX_NAME_LEN));
            }
        }
        if let TopicChange::Set(topic) = &settings.topic {
            if topic.chars().count() > MAX_TOPIC_LEN {
                return Err(fault::field_too_long("topic", MAX_TOPIC_LEN));
            }
        }
        if let Some(seconds) = settings.slow_mode_seconds {
            if !(0..=MAX_SLOW_MODE_SECONDS).contains(&seconds) {
                return Err(fault::validation(
                    "slow_mode_seconds",
                    "between 0 and 3600; longer than an hour is a read-only room, not slow mode",
                ));
            }
        }
        if let Some(policy) = settings.join_policy {
            if !matches!(
                policy,
                join_policy::OPEN | join_policy::APPROVAL | join_policy::INVITE
            ) {
                return Err(fault::validation(
                    "join_policy",
                    "open, approval, or invite only",
                ));
            }
        }
        Ok(())
    }

    /// Everything that can be refused about a sanction.
    fn validate_sanction(sanction: &Sanction) -> Result<()> {
        let reason = match sanction {
            Sanction::Mute {
                duration_ms,
                reason,
            } => {
                if *duration_ms <= 0 || *duration_ms > MAX_MUTE_MS {
                    return Err(fault::validation(
                        "duration_ms",
                        "a mute lasts between one millisecond and thirty days; longer is a ban",
                    ));
                }
                reason
            }
            Sanction::Ban {
                duration_ms,
                reason,
            } => {
                if duration_ms.is_some_and(|ms| ms <= 0) {
                    return Err(fault::validation(
                        "duration_ms",
                        "a ban lasts at least one millisecond; omit it for a permanent one",
                    ));
                }
                reason
            }
            Sanction::Unmute | Sanction::Kick | Sanction::Unban => &None,
        };
        if let Some(reason) = reason {
            if reason.chars().count() > MAX_REASON_LEN {
                return Err(fault::field_too_long("reason", MAX_REASON_LEN));
            }
        }
        Ok(())
    }
}

/// Which series a sanction is counted under.
const fn sanction_kind(sanction: &Sanction) -> SanctionKind {
    match sanction {
        Sanction::Mute { .. } => SanctionKind::Mute,
        Sanction::Unmute => SanctionKind::Unmute,
        Sanction::Kick => SanctionKind::Kick,
        Sanction::Ban { .. } => SanctionKind::Ban,
        Sanction::Unban => SanctionKind::Unban,
    }
}

/// A membership event about `subject`, for the room's topic.
fn member_event(
    room_id: Id,
    subject: Id,
    joined: bool,
    role: Option<RoomRole>,
    member_count: Option<u32>,
) -> RoomMemberEvent {
    RoomMemberEvent {
        room_id,
        user_id: subject,
        joined,
        role,
        member_count,
    }
}

#[async_trait]
impl<S, L> Roomkeeper for Rooms<S, L>
where
    S: RoomStore + MessagingStore + ?Sized + Send + Sync,
    L: RateLimiter + ?Sized + Send + Sync,
{
    async fn create(&self, caller: &Caller, request: NewRoomRequest) -> Result<RoomSummary> {
        Self::require_identity(caller)?;
        if let Err(err) = Self::validate_new(&request, &self.config) {
            self.meters.create(CreateOutcome::Invalid);
            return Err(err);
        }
        if let Err(err) = self.charge_flat(caller, CREATE_COST).await {
            self.meters.create(CreateOutcome::RateLimited);
            return Err(err);
        }
        // Advisory, not authoritative. The store owns slug uniqueness and will refuse
        // a collision on its own; this read exists so the ordinary case — the name is
        // taken — comes back as `ALREADY_EXISTS` naming the field, instead of as the
        // bare conflict a unique-index violation produces.
        if self.store.room_by_slug(&request.slug).await?.is_some() {
            self.meters.create(CreateOutcome::Taken);
            return Err(fault::already_exists("room slug"));
        }
        let new = NewRoom {
            room_id: self.new_id(caller.now),
            conversation_id: self.new_id(caller.now),
            slug: request.slug,
            name: request.name.trim().to_string(),
            // An all-whitespace topic becomes no topic. The alternative stores a
            // string that renders as a topic-shaped gap in every client.
            topic: request
                .topic
                .map(|topic| topic.trim().to_string())
                .filter(|topic| !topic.is_empty()),
            kind: request.kind,
            owner_id: caller.account_id,
            home_region: self.config.home_region.clone(),
            max_members: request
                .max_members
                .unwrap_or(self.config.default_max_members),
            encryption: view::encryption_for(request.kind),
            created_at: caller.now,
        };
        let room = match self.store.create_room(new).await {
            Ok(room) => room,
            Err(err) => {
                // The read above cannot close the window: two creations of one slug in
                // the same millisecond both pass it and the index refuses the second.
                let taken = matches!(err.code(), codes::ALREADY_EXISTS | codes::CONFLICT);
                self.meters.create(if taken {
                    CreateOutcome::Taken
                } else {
                    CreateOutcome::Invalid
                });
                return Err(err);
            }
        };
        self.meters.create(CreateOutcome::Accepted);
        Ok(view::summary(&room, Some(RoomRole::Owner)))
    }

    async fn join(
        &self,
        caller: &Caller,
        request: RoomJoinRequest,
    ) -> Result<(RoomJoinResponse, Option<Fanout>)> {
        Self::require_identity(caller)?;
        if request.room_id.is_nil() {
            self.meters.join(JoinOutcome::NotFound);
            return Err(fault::validation("room_id", "a room id is required"));
        }
        // Refused rather than ignored. An invite code this build cannot check is a
        // code the client believes was verified, and admitting the holder on the
        // strength of a string nobody read is worse than telling them the feature is
        // off.
        if request.invite_code.is_some() {
            self.meters.join(JoinOutcome::NotAdmitted);
            return Err(fault::feature_disabled("room invite codes"));
        }
        if let Err(err) = self.charge(caller, Opcode::RoomJoin).await {
            self.meters.join(JoinOutcome::RateLimited);
            return Err(err);
        }
        let room = match self.load_room(request.room_id).await {
            Ok(room) => room,
            Err(err) => {
                self.meters.join(JoinOutcome::NotFound);
                return Err(err);
            }
        };
        if room.archived_at.is_some() {
            self.meters.join(JoinOutcome::Archived);
            return Err(Self::room_archived());
        }
        let existing = self
            .store
            .room_member(room.room_id, caller.account_id)
            .await?;
        // Before anything else about the room: a ban survives leaving and rejoining,
        // and the store's `join_room` does not check it.
        if existing
            .as_ref()
            .is_some_and(|member| member.is_banned(caller.now))
        {
            self.meters.join(JoinOutcome::Banned);
            return Err(Self::banned());
        }
        let already = existing.as_ref().is_some_and(RoomMember::is_active);
        if !already {
            match room.join_policy {
                join_policy::OPEN => {}
                // Brief section 20 asks for an approval queue and there is no table
                // for one. Refusing is the only honest answer: admitting somebody a
                // policy meant to hold back is the failure that shipping the table
                // later cannot undo.
                join_policy::APPROVAL => {
                    self.meters.join(JoinOutcome::NotAdmitted);
                    return Err(fault::feature_disabled("room join approval"));
                }
                // Invitation-only without invitations means nobody new gets in. That
                // is the policy working, so it is `PERMISSION_DENIED` and not a
                // missing feature.
                join_policy::INVITE => {
                    self.meters.join(JoinOutcome::NotAdmitted);
                    return Err(fault::permission_denied("the room is invitation only"));
                }
                _ => {
                    self.meters.join(JoinOutcome::NotAdmitted);
                    return Err(fault::feature_disabled("the room's join policy"));
                }
            }
            // A pre-check, because the store enforces capacity inside the same
            // critical section as the insert and can only report `CONFLICT` from
            // there. This is what turns the common case into `ROOM_FULL`, which the
            // client can render; the store's refusal remains the backstop for the two
            // joins that race for the last seat.
            if room.member_count >= room.max_members {
                self.meters.join(JoinOutcome::Full);
                return Err(Self::room_full());
            }
        }
        // The existing row when there is one, so a rejoin keeps the role it was given
        // and the sanctions it accumulated. A fresh row would silently demote a
        // Moderator who stepped out for an hour and clear the mute they were under.
        let row = match existing.clone() {
            Some(member) => member,
            None => RoomMember {
                room_id: room.room_id,
                account_id: caller.account_id,
                role: RoomRole::Member,
                permissions_grant: 0,
                permissions_deny: 0,
                joined_at: caller.now,
                left_at: None,
                muted_until: None,
                banned_until: None,
                ban_reason: None,
                // Nobody invited them; the room was open. The column records who did
                // when an invitation exists to record.
                invited_by: None,
            },
        };
        let stored = self.store.join_room(row).await?;
        // Re-read so the summary carries the count the join produced rather than the
        // one from before it.
        let room = self.load_room(room.room_id).await?;
        let last_seq = self.store.conversation(room.conversation_id).await?.map_or(
            NO_MESSAGES_YET,
            |conversation| {
                // `max(0)` before the cast: a negative sequence is impossible and an
                // `as u64` of one would hand the client eighteen quintillion.
                conversation.last_seq.max(0) as u64
            },
        );
        let response = RoomJoinResponse {
            room: view::summary(&room, Some(stored.role)),
            conversation_id: room.conversation_id,
            encryption: view::encryption_for(room.kind),
            last_seq,
        };
        self.meters.join(if already {
            JoinOutcome::Already
        } else if existing.is_some() {
            JoinOutcome::Rejoined
        } else {
            JoinOutcome::Accepted
        });
        // Brief section 156: a client that asked to join a room it is already in gets
        // its answer and the room hears nothing.
        let fanout = (!already).then(|| {
            Fanout::member(
                room.room_id,
                caller.device_id,
                member_event(
                    room.room_id,
                    caller.account_id,
                    true,
                    Some(stored.role),
                    Some(view::count(room.member_count)),
                ),
            )
        });
        Ok((response, fanout))
    }

    async fn leave(&self, caller: &Caller, request: RoomLeaveRequest) -> Result<Option<Fanout>> {
        Self::require_identity(caller)?;
        if request.room_id.is_nil() {
            return Err(fault::validation("room_id", "a room id is required"));
        }
        self.charge(caller, Opcode::RoomLeave).await?;
        let room = self.load_room(request.room_id).await?;
        let Some(member) = self
            .store
            .room_member(room.room_id, caller.account_id)
            .await?
        else {
            self.meters.leave(ChangeOutcome::Unchanged);
            return Ok(None);
        };
        if !member.is_active() {
            self.meters.leave(ChangeOutcome::Unchanged);
            return Ok(None);
        }
        // The owner cannot walk out. A room whose owner is gone has nobody who can
        // transfer it, archive it, or appoint a Manager, and the alternative — promote
        // somebody automatically — hands a community to whoever happens to be next in
        // the roster.
        if room.owner_id == caller.account_id {
            self.meters.leave(ChangeOutcome::Denied);
            return Err(fault::conflict(
                "the owner must transfer the room or archive it before leaving",
            ));
        }
        self.store
            .leave_room(room.room_id, caller.account_id, caller.now)
            .await?;
        self.meters.leave(ChangeOutcome::Applied);
        Ok(Some(Fanout::member(
            room.room_id,
            caller.device_id,
            member_event(
                room.room_id,
                caller.account_id,
                false,
                None,
                Some(self.current_count(room.room_id).await?),
            ),
        )))
    }

    async fn list(&self, caller: &Caller, request: RoomListRequest) -> Result<RoomListResponse> {
        Self::require_identity(caller)?;
        // Refused, not ignored. `rooms` has no column for any of the three, so a
        // client that asked for Indonesian-language rooms and silently received all
        // rooms would render a filtered heading over unfiltered content — and would
        // keep doing it, because nothing about the response says the filter was
        // dropped.
        for (field, value) in [
            ("category", &request.category),
            ("language", &request.language),
            ("country", &request.country),
        ] {
            if value.is_some() {
                return Err(fault::feature_disabled(&format!("room listing by {field}")));
            }
        }
        // A cursor over an ordering that moves — member count — silently skips rows
        // and repeats others as rooms shuffle past the cursor between pages. Paging a
        // ranked browse needs a snapshot or a stable tiebreak carried in the token,
        // and neither exists yet, so `next_cursor` is always absent and this refuses
        // to pretend otherwise.
        if request.cursor.is_some() {
            return Err(fault::feature_disabled("room listing cursors"));
        }
        if request
            .query
            .as_ref()
            .is_some_and(|query| query.chars().count() > MAX_QUERY_LEN)
        {
            return Err(fault::field_too_long("query", MAX_QUERY_LEN));
        }
        self.charge(caller, Opcode::RoomList).await?;
        let limit = if request.limit == 0 {
            DEFAULT_LIST_LIMIT
        } else {
            request.limit.min(MAX_LIST_LIMIT)
        };
        let query = request
            .query
            .as_deref()
            .map(str::trim)
            .filter(|query| !query.is_empty())
            .map(str::to_lowercase);
        // A search pays for a scan; a plain browse asks for exactly what it will
        // return.
        let scan = if query.is_some() {
            MAX_SEARCH_SCAN
        } else {
            // `as u16` on a value the clamp above holds under 51.
            limit as u16
        };
        // `None` and not `Some(Public)`: brief section 21 lists Managed rooms in
        // discovery and moderates the joining, so hiding them here would make a
        // Managed room unfindable by the people it is for.
        let scanned = self.store.browse_rooms(None, scan).await?;
        let rooms: Vec<RoomSummary> = scanned
            .iter()
            .filter(|room| match query.as_deref() {
                None => true,
                // The slug is already lowercase by construction; the name is not.
                Some(query) => {
                    room.name.to_lowercase().contains(query) || room.slug.contains(query)
                }
            })
            .take(limit as usize)
            // `None` for the role, and not the caller's: this row was read for a
            // browse and nobody looked up a membership for it. A listing that
            // guessed would claim a role the caller does not have.
            .map(|room| view::summary(room, None))
            .collect();
        self.meters.listing(rooms.len(), scanned.len());
        Ok(RoomListResponse {
            rooms,
            next_cursor: None,
        })
    }

    async fn summary(&self, caller: &Caller, room_id: Id) -> Result<RoomSummary> {
        Self::require_identity(caller)?;
        self.charge_flat(caller, READ_COST).await?;
        let room = self.load_room(room_id).await?;
        let role = self.role_in(&room, caller.account_id).await?;
        Ok(view::summary(&room, role))
    }

    async fn resolve(&self, caller: &Caller, reference: &str) -> Result<RoomSummary> {
        Self::require_identity(caller)?;
        self.charge_flat(caller, READ_COST).await?;
        let reference = reference.trim();
        // The id form first, because a slug can never be one: `slug_is_valid` refuses
        // any string that parses as an id, so the two namespaces cannot overlap and
        // the order cannot be exploited.
        let room = if let Ok(room_id) = Id::parse(reference) {
            self.store.room(room_id).await?
        } else if slug_is_valid(reference) {
            self.store.room_by_slug(reference).await?
        } else {
            // Not a validation error. A deep link that does not name a room is
            // indistinguishable, from the outside, from one that names a room this
            // caller may not see, and answering "malformed" for one and "not found"
            // for the other turns the endpoint into a slug oracle.
            None
        };
        let room = room.ok_or_else(|| fault::not_found("room"))?;
        let role = self.role_in(&room, caller.account_id).await?;
        Ok(view::summary(&room, role))
    }

    async fn mine(&self, caller: &Caller) -> Result<Vec<RoomSummary>> {
        Self::require_identity(caller)?;
        self.charge_flat(caller, READ_COST).await?;
        let rooms = self.store.rooms_for_account(caller.account_id).await?;
        // One membership read per room. The store's contract offers a room list and a
        // membership read, not a joined projection, and the alternative — reporting
        // `my_role: None` on the screen that exists to show what you are — would make
        // the field useless exactly where it matters. Bounded by how many rooms one
        // account is in, which is a number a person chose.
        let mut summaries = Vec::with_capacity(rooms.len());
        for room in &rooms {
            let role = self.role_in(room, caller.account_id).await?;
            summaries.push(view::summary(room, role));
        }
        Ok(summaries)
    }

    async fn roster(
        &self,
        caller: &Caller,
        room_id: Id,
        limit: u16,
        after: Option<Id>,
    ) -> Result<Vec<RoomMember>> {
        Self::require_identity(caller)?;
        self.charge_flat(caller, READ_COST).await?;
        let room = self.load_room(room_id).await?;
        // Membership and nothing more. A roster is the membership list of a
        // community: handing it to anyone holding a room id would make every public
        // room a directory of the people in it, and there is no permission bit for
        // "may see who is here" because every member may.
        self.require(caller, &room, 0).await?;
        self.store
            .room_members(room_id, limit.clamp(1, MAX_ROSTER_PAGE), after)
            .await
    }

    async fn update(
        &self,
        caller: &Caller,
        room_id: Id,
        settings: Settings,
    ) -> Result<(RoomSummary, Option<Fanout>)> {
        Self::require_identity(caller)?;
        if let Err(err) = Self::validate_settings(&settings) {
            self.meters.settings(ChangeOutcome::Invalid);
            return Err(err);
        }
        self.charge_flat(caller, SETTINGS_COST).await?;
        let room = self.load_room(room_id).await?;
        if room.archived_at.is_some() {
            self.meters.settings(ChangeOutcome::Denied);
            return Err(Self::room_archived());
        }
        // The join policy decides who gets in, which is membership management rather
        // than decoration, so it costs the higher bit. An Administrator can rename a
        // room; turning it invitation-only is a Manager's decision.
        let needed = if settings.join_policy.is_some() {
            permission::ROOM_EDIT | permission::ROOM_MANAGE
        } else {
            permission::ROOM_EDIT
        };
        let actor = match self.require(caller, &room, needed).await {
            Ok(actor) => actor,
            Err(err) => {
                self.meters.settings(ChangeOutcome::Denied);
                return Err(err);
            }
        };
        // Each field is dropped when it already holds the value asked for, so a
        // settings screen that submits every input does not stamp `updated_at`, does
        // not broadcast, and does not appear in an audit trail as a change.
        let name = settings
            .name
            .map(|name| name.trim().to_string())
            .filter(|name| *name != room.name);
        let topic = match settings.topic {
            TopicChange::Keep => Patch::Keep,
            TopicChange::Set(topic) => {
                let topic = topic.trim().to_string();
                // An all-whitespace topic is a removal, matching what creation does
                // with one.
                if topic.is_empty() {
                    if room.topic.is_none() {
                        Patch::Keep
                    } else {
                        Patch::Clear
                    }
                } else if room.topic.as_deref() == Some(topic.as_str()) {
                    Patch::Keep
                } else {
                    Patch::Set(topic)
                }
            }
            TopicChange::Clear if room.topic.is_none() => Patch::Keep,
            TopicChange::Clear => Patch::Clear,
        };
        let slow_mode = settings
            .slow_mode_seconds
            .filter(|seconds| *seconds != room.slow_mode_seconds);
        let join_policy = settings
            .join_policy
            .filter(|policy| *policy != room.join_policy);
        if name.is_none() && topic.is_keep() && slow_mode.is_none() && join_policy.is_none() {
            self.meters.settings(ChangeOutcome::Unchanged);
            return Ok((view::summary(&room, Some(actor.role)), None));
        }
        let updated = self
            .store
            .update_room(
                room_id,
                name,
                topic.clone(),
                slow_mode,
                join_policy,
                caller.now,
            )
            .await?;
        let mut event = view::delta(room_id);
        match &topic {
            Patch::Set(topic) => event.topic = Some(topic.clone()),
            // An empty string, because `None` on this field already means "unchanged".
            // `crate::view` records why the wire cannot say it any other way.
            Patch::Clear => event.topic = Some(String::new()),
            Patch::Keep => {}
        }
        if slow_mode.is_some() {
            // `Some(0)` when it was turned off: `None` would mean "unchanged" and
            // leave every client showing an interval that is no longer in force.
            event.slow_mode_ms = Some(view::slow_mode_ms(&updated).unwrap_or(0));
        }
        // A rename and a policy change produce no frame. `RoomStateEvent` carries a
        // count, a topic, and an interval, and nothing else — so a renamed room is
        // learned from the next summary rather than announced. Adding a field for it
        // is a schema change, and a domain crate is not where the packet registry
        // gets edited.
        self.meters.settings(ChangeOutcome::Applied);
        let fanout =
            (!view::is_empty(&event)).then(|| Fanout::state(room_id, caller.device_id, event));
        Ok((view::summary(&updated, Some(actor.role)), fanout))
    }

    async fn archive(&self, caller: &Caller, room_id: Id) -> Result<()> {
        Self::require_identity(caller)?;
        self.charge_flat(caller, SETTINGS_COST).await?;
        let room = self.load_room(room_id).await?;
        // The owner only. `ROOM_MANAGE` is deliberately not enough: archiving ends the
        // room for everybody in it, and it is the one settings action a Manager
        // appointed this morning should not be able to take alone.
        if room.owner_id != caller.account_id {
            return Err(fault::permission_denied(
                "only the owner may archive a room",
            ));
        }
        if room.archived_at.is_some() {
            // Idempotent. The second press of a button whose first press succeeded is
            // not an error worth showing anybody.
            return Ok(());
        }
        self.store.archive_room(room_id, caller.now).await?;
        self.meters.archive();
        Ok(())
    }

    async fn set_role(
        &self,
        caller: &Caller,
        room_id: Id,
        subject_id: Id,
        role: RoomRole,
    ) -> Result<Option<Fanout>> {
        Self::require_identity(caller)?;
        if role == RoomRole::Unknown {
            self.meters.role(ChangeOutcome::Invalid);
            return Err(fault::validation("role", "not a role this build knows"));
        }
        // Ownership is not a role you can be given. `transfer_ownership` demotes the
        // outgoing owner in the same write; granting `Owner` here would produce a room
        // with two of them and an `owner_id` column that names one.
        if role == RoomRole::Owner {
            self.meters.role(ChangeOutcome::Invalid);
            return Err(fault::conflict(
                "ownership moves by transfer, not by a role change",
            ));
        }
        self.charge_flat(caller, MODERATION_COST).await?;
        let room = self.load_room(room_id).await?;
        let (actor, subject) = match self
            .require_over(caller, &room, subject_id, permission::ROOM_MANAGE)
            .await
        {
            Ok(pair) => pair,
            Err(err) => {
                self.meters.role(ChangeOutcome::Denied);
                return Err(err);
            }
        };
        // The granted role, as well as the current one. Without this a Moderator could
        // mint an Administrator and be outranked by their own appointee a second later.
        if !permission::outranks(actor.role, role) {
            self.meters.role(ChangeOutcome::Denied);
            return Err(fault::permission_denied(
                "the granted role is not below the actor's",
            ));
        }
        if !subject.is_active() {
            self.meters.role(ChangeOutcome::Denied);
            return Err(Self::not_a_member());
        }
        if subject.role == role {
            self.meters.role(ChangeOutcome::Unchanged);
            return Ok(None);
        }
        self.store
            .set_room_role(room_id, subject_id, role, caller.now)
            .await?;
        self.meters.role(ChangeOutcome::Applied);
        // `joined: true`, because the field says whether the member is in the room and
        // they are. `RoomMemberEvent` has no verb, so a client tells a promotion from
        // an arrival by whether the member was already in its roster — which is also
        // why the count is absent: nothing about it moved.
        Ok(Some(Fanout::member(
            room_id,
            caller.device_id,
            member_event(room_id, subject_id, true, Some(role), None),
        )))
    }

    async fn set_permissions(
        &self,
        caller: &Caller,
        room_id: Id,
        subject_id: Id,
        grant: u64,
        deny: u64,
    ) -> Result<()> {
        Self::require_identity(caller)?;
        // A bit this build does not define is refused rather than stored. Keeping it
        // would make the difference invisible until the bit acquired a meaning in a
        // later release and started granting something nobody asked for.
        if permission::unknown_bits(grant | deny) != 0 {
            self.meters.overrides(ChangeOutcome::Invalid);
            return Err(fault::validation(
                "permissions",
                "the mask names bits this build does not define",
            ));
        }
        if grant & deny != 0 {
            self.meters.overrides(ChangeOutcome::Invalid);
            return Err(fault::validation(
                "permissions",
                "a bit cannot be granted and denied at once",
            ));
        }
        self.charge_flat(caller, MODERATION_COST).await?;
        let room = self.load_room(room_id).await?;
        let (actor, subject) = match self
            .require_over(caller, &room, subject_id, permission::ROOM_MANAGE)
            .await
        {
            Ok(pair) => pair,
            Err(err) => {
                self.meters.overrides(ChangeOutcome::Denied);
                return Err(err);
            }
        };
        // Without this the override mask is a privilege-escalation primitive: grant
        // yourself `ROOM_MANAGE`, then grant yourself the rest.
        //
        // Only the grant is checked. A deny cannot escalate anything — the worst it
        // does is take a permission away, which `ROOM_MANAGE` already allows by
        // demotion — so requiring the actor to hold a bit before withholding it would
        // buy nothing and would block an Administrator who had been denied
        // `CHAT_PIN` themselves from moderating pins.
        if !permission::allows(actor.permissions, grant) {
            self.meters.overrides(ChangeOutcome::Denied);
            return Err(fault::permission_denied(
                "a permission cannot be granted by an actor who does not hold it",
            ));
        }
        if subject.permissions_grant == grant && subject.permissions_deny == deny {
            self.meters.overrides(ChangeOutcome::Unchanged);
            return Ok(());
        }
        self.store
            .set_room_permissions(room_id, subject_id, grant, deny, caller.now)
            .await?;
        self.meters.overrides(ChangeOutcome::Applied);
        // No fanout. `RoomMemberEvent` carries a role and not a permission set, so
        // there is no frame that could describe this, and inventing one would put a
        // moderation detail about one member on a topic the whole room subscribes to.
        Ok(())
    }

    async fn sanction(
        &self,
        caller: &Caller,
        room_id: Id,
        subject_id: Id,
        sanction: Sanction,
    ) -> Result<Option<Fanout>> {
        Self::require_identity(caller)?;
        Self::validate_sanction(&sanction)?;
        self.charge_flat(caller, MODERATION_COST).await?;
        let room = self.load_room(room_id).await?;
        let (_actor, subject) = self
            .require_over(caller, &room, subject_id, sanction.permission())
            .await?;
        self.meters.sanction(sanction_kind(&sanction));
        // Every arm passes the *other* sanction's expiry back unchanged, because
        // `set_room_sanction` writes both columns and a caller that sent `None` for
        // the one it was not changing would lift it. Muting somebody must not be a
        // way to clear their ban.
        match sanction {
            Sanction::Mute {
                duration_ms,
                reason,
            } => {
                let until =
                    Timestamp::from_millis(caller.now.as_millis().saturating_add(duration_ms));
                self.store
                    .set_room_sanction(
                        room_id,
                        subject_id,
                        Some(until),
                        subject.banned_until,
                        // One `reason` column serves both sanctions, so the most
                        // recent action's text is what the member is shown. An
                        // honest limitation of the schema rather than a choice:
                        // `docs/04-data-model.md` has one column, and writing a
                        // mute reason into a second one it does not have is not
                        // something this crate can do.
                        reason,
                        caller.now,
                    )
                    .await?;
                // No frame. A mute is between a moderator and one member, and
                // announcing it to the room turns a correction into a punishment.
                Ok(None)
            }
            Sanction::Unmute => {
                self.store
                    .set_room_sanction(
                        room_id,
                        subject_id,
                        None,
                        subject.banned_until,
                        subject.ban_reason.clone(),
                        caller.now,
                    )
                    .await?;
                Ok(None)
            }
            Sanction::Kick => {
                if !subject.is_active() {
                    // Already gone. Nothing to write and nothing to announce.
                    return Ok(None);
                }
                self.store
                    .leave_room(room_id, subject_id, caller.now)
                    .await?;
                Ok(Some(Fanout::member(
                    room_id,
                    caller.device_id,
                    member_event(
                        room_id,
                        subject_id,
                        false,
                        None,
                        Some(self.current_count(room_id).await?),
                    ),
                )))
            }
            Sanction::Ban {
                duration_ms,
                reason,
            } => {
                let until = Timestamp::from_millis(duration_ms.map_or(PERMANENT_BAN_MS, |ms| {
                    // Clamped, so a client that sends a very large duration gets a
                    // permanent ban rather than a timestamp that wrapped.
                    caller
                        .now
                        .as_millis()
                        .saturating_add(ms)
                        .min(PERMANENT_BAN_MS)
                }));
                self.store
                    .set_room_sanction(
                        room_id,
                        subject_id,
                        subject.muted_until,
                        Some(until),
                        reason,
                        caller.now,
                    )
                    .await?;
                // The room hears that somebody left, not why. The reason is for the
                // banned account and the moderation log; broadcasting it would put
                // free text an annoyed moderator typed onto every subscriber's screen.
                Ok(Some(Fanout::member(
                    room_id,
                    subject_id,
                    member_event(
                        room_id,
                        subject_id,
                        false,
                        None,
                        Some(self.current_count(room_id).await?),
                    ),
                )))
            }
            Sanction::Unban => {
                self.store
                    .set_room_sanction(
                        room_id,
                        subject_id,
                        subject.muted_until,
                        None,
                        // Cleared with the ban. A reason that outlived the sanction it
                        // explains would surface on the next mute as an explanation of
                        // something else.
                        None,
                        caller.now,
                    )
                    .await?;
                // Lifting a ban does not put anybody back in the room; they rejoin.
                Ok(None)
            }
        }
    }

    async fn transfer_ownership(
        &self,
        caller: &Caller,
        room_id: Id,
        to: Id,
    ) -> Result<Option<Fanout>> {
        Self::require_identity(caller)?;
        if to.is_nil() {
            return Err(fault::validation("to", "an account id is required"));
        }
        // Brief section 85. Ahead of everything else, including the rate limiter,
        // because the point of the requirement is that a stolen session cannot reach
        // the operation at all — and a refusal that had already spent budget would let
        // an attacker drain the real owner's allowance while being turned away.
        if !caller.reauthenticated {
            return Err(fault::error(
                codes::REAUTHENTICATION_REQUIRED,
                "an ownership transfer needs a recently proved factor",
            ));
        }
        self.charge_flat(caller, MODERATION_COST).await?;
        let room = self.load_room(room_id).await?;
        // Not a permission bit. There is no `ROOM_TRANSFER` in brief section 48 and
        // there should not be: the room is the owner's to give away, and a bit for it
        // would be a bit somebody could be granted.
        if room.owner_id != caller.account_id {
            return Err(fault::permission_denied(
                "only the owner may transfer a room",
            ));
        }
        if to == caller.account_id {
            // Already the owner. The store treats this as a no-op too; returning early
            // keeps the counter honest about how many transfers happened.
            return Ok(None);
        }
        self.store
            .transfer_room_ownership(room_id, caller.account_id, to, caller.now)
            .await?;
        self.meters.transfer();
        // One event, about the incoming owner. The outgoing owner's demotion to
        // Manager is deliberately not broadcast: two frames would arrive in an order
        // the gateway does not promise, and a client that saw the demotion first would
        // render a room with no owner.
        Ok(Some(Fanout::member(
            room_id,
            caller.device_id,
            member_event(room_id, to, true, Some(RoomRole::Owner), None),
        )))
    }

    async fn authorize(&self, caller: &Caller, room_id: Id, needed: u64) -> Result<Authorized> {
        Self::require_identity(caller)?;
        // No charge. See the module docs: this is called from inside an operation that
        // has already paid, and billing it again would make one user action cost twice.
        let room = self.load_room(room_id).await?;
        self.require(caller, &room, needed).await
    }
}
