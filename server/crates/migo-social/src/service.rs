//! The social service.
//!
//! # The four invariants
//!
//! **A block is symmetric, and it is checked first.** Every path here asks whether
//! either account has blocked the other before it does anything else, because a block
//! that only stops contact in the direction it was written is a block that does not
//! work. The store answers the symmetric question in one call for exactly this reason.
//!
//! **A pending request is not a friendship.** `accepted_at` decides, everywhere. A
//! request that read as a friendship would let anybody see a `Friends`-only field by
//! asking to be a friend and never being answered — which is a privacy bypass with a
//! one-line exploit.
//!
//! **A refusal never says who refused.** Brief section 180 requires that a caller
//! cannot tell "this account blocked me" from "this account's settings exclude you".
//! Both answer `PRIVACY_RESTRICTED`. The caller's *own* block answers
//! `BLOCKED_BY_USER`, because telling somebody what they themselves did leaks nothing
//! and saves them a support ticket.
//!
//! **A gate that cannot finish refuses.** The mutual-friend answer is bounded at
//! [`MAX_MUTUAL_SCAN`] a side. Past that the scan is incomplete, and an incomplete
//! scan is treated as "no mutual friend" — a refusal. A privacy gate that fails open
//! when the data gets large is a privacy gate that stops working exactly for the
//! accounts that have the most to lose.
//!
//! # Why `may_interact` is not rate limited
//!
//! Every other method here charges the limiter; that one deliberately does not. It is
//! called by another domain on the path of an operation that has already been charged
//! — a send charges `MessageSend`, then asks this crate whether the recipient accepts
//! messages — and charging again would bill one user action twice and make a send
//! budget depend on how many gates the implementation happens to consult.
//!
//! # Where these prices come from
//!
//! Brief section 145 already priced four of them, in the block marked `STATUS: SPEC`:
//! `FRIEND_REQUEST` 10, `FRIEND_RESPOND` 5, `BLOCK_SET` 5, `RELATIONSHIP_LIST` 3. The
//! opcodes are not in the packet registry yet, so they cannot be charged through
//! `charge_opcode`, but the numbers the brief chose are the numbers used here — copied
//! rather than invented, so that generating the frames later is a change of mechanism
//! and not a reprice.
//!
//! Two operations the brief never gave an opcode — search and suggestion — are priced
//! in this file, higher than a listing, because each one is a scan rather than a keyed
//! read.
//!
//! # What this crate does not do
//!
//! It does not write a profile, read a presence, deliver a frame, or walk the graph
//! past one hop. [`crate::traits`] records why for each one.

use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;
use migo_core::metrics::Registry;
use migo_core::{Error, Id, Result};
use migo_protocol::{codes, fault, Opcode, RelationshipKind};
use migo_ratelimit::{BucketKey, RateLimiter, SharedRateLimiter};
use migo_store::model::{Profile, Relationship, Visibility};
use migo_store::traits::{AccountStore, SocialStore};
use migo_store::{SharedStore, Store};

use crate::metrics::{EdgeKind, GateOutcome, Meters, RequestOutcome, ResponseOutcome};
use crate::model::{
    query_is_usable, strictest, Caller, Edge, Found, FriendOutcome, Interaction, Pending,
    ProfileCard, RespondOutcome, SocialConfig, Standing, Suggestion, DEFAULT_PAGE, MAX_FAVORITES,
    MAX_MUTUAL_SCAN, MAX_PAGE, MAX_PROFILE_BATCH,
};
use crate::notice::Notice;
use crate::traits::Graph;

/// A shared, fully erased social service.
pub type SharedSocial = Arc<dyn Graph>;

/// What a friend request costs. Brief section 145, opcode 113.
const REQUEST_COST: u32 = 10;

/// What answering a friend request costs. Brief section 145, opcode 114.
const RESPOND_COST: u32 = 5;

/// What creating or removing any other edge costs. Brief section 145, opcode 116.
///
/// One price for follow, unfollow, block, unblock, favourite, and un-friend. They are
/// one row each, written or deleted, and pricing them apart would be five numbers
/// expressing one fact.
const EDGE_COST: u32 = 5;

/// What one listing costs. Brief section 145, opcode 117.
const LIST_COST: u32 = 3;

/// What one account search costs.
///
/// No opcode in brief section 145 to take this from. Twice a listing, because a search
/// is a prefix scan across every searchable account rather than an indexed read of one
/// account's edges, and because search is the endpoint a scraper reaches for first.
const SEARCH_COST: u32 = 10;

/// What one suggestion round costs.
///
/// Four times a listing. A suggestion is up to `1 + MAX_MUTUAL_SCAN` reads of the
/// friend table, which is the most expensive thing this crate will do on a request
/// path, and the price should say so.
const SUGGEST_COST: u32 = 20;

/// Friends whose own friend lists a suggestion round will read.
///
/// Twenty-five, not two hundred. The scan is quadratic in this number — twenty-five
/// friends of two hundred friends each is five thousand edges read to fill one screen
/// — and a suggestion list is a nice-to-have that must never be the reason a request
/// path got slow. The friends chosen are the most recent, which is also the most
/// useful sample: the people somebody met last are the people whose circles they are
/// still meeting.
const SUGGEST_SEED_FRIENDS: u16 = 25;

/// Which side of a pair wrote the block.
///
/// Three states rather than a boolean, because the answer decides which of two error
/// codes a caller sees, and brief section 180 makes that distinction load-bearing: one
/// of the two codes may be shown and the other must never be.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BlockState {
    /// Neither has blocked the other.
    Clear,
    /// The caller blocked the subject. Safe to disclose: it is their own act.
    ByCaller,
    /// The subject blocked the caller. Never disclosed as such.
    BySubject,
}

/// The social graph over a store and a rate limiter.
///
/// No cache parameter, unlike presence and messaging. A stale block is a blocked
/// account getting through, and brief section 173 requires that losing the cache lose
/// nothing that matters — the way to honour that for a block is not to put it there.
///
/// No `Random`, unlike rooms. This crate mints no ids: every row it writes is keyed by
/// the pair of accounts that already exist, which is also what makes the whole surface
/// idempotent without a generated key to deduplicate on.
pub struct Social<S: ?Sized = dyn Store, L: ?Sized = dyn RateLimiter> {
    store: Arc<S>,
    limiter: Arc<L>,
    config: SocialConfig,
    meters: Meters,
}

/// Builds the social service the composition root hands around.
#[must_use]
pub fn open(
    store: SharedStore,
    limiter: SharedRateLimiter,
    registry: &Registry,
    config: SocialConfig,
) -> SharedSocial {
    Arc::new(Social::new(store, limiter, registry, config))
}

impl<S, L> Social<S, L>
where
    S: AccountStore + SocialStore + ?Sized,
    L: RateLimiter + ?Sized,
{
    /// Assembles the service and registers every series at zero.
    pub fn new(store: Arc<S>, limiter: Arc<L>, registry: &Registry, config: SocialConfig) -> Self {
        Self {
            store,
            limiter,
            config,
            meters: Meters::new(registry),
        }
    }

    // --- shared plumbing -------------------------------------------------------

    /// Charges an operation that has no opcode to be priced from.
    ///
    /// One bucket, the account's, because there is no endpoint identity to open a
    /// second one under. See the module docs for where the numbers come from.
    async fn charge(&self, caller: &Caller, cost: u32) -> Result<()> {
        let keys = [BucketKey::account_write(caller.account_id)];
        self.limiter
            .charge(&keys, cost, caller.tier, caller.now)
            .await?
            .into_result()
    }

    /// Charges an operation that does have an opcode.
    ///
    /// Only [`Graph::standing`] qualifies: it is what a profile screen asks for, so it
    /// is charged as `PROFILE_FETCH` and repriced by an edit to the IDL rather than by
    /// an edit to this file.
    async fn charge_opcode(&self, caller: &Caller, opcode: Opcode) -> Result<()> {
        let keys = [
            BucketKey::endpoint_write_of_account(caller.account_id, opcode),
            BucketKey::account_write(caller.account_id),
        ];
        self.limiter
            .charge_opcode(&keys, opcode, caller.tier, caller.now)
            .await?
            .into_result()
    }

    /// Refuses a caller that is not fully identified.
    ///
    /// The gateway never produces one. It is checked anyway because a nil account id
    /// would be an edge every unauthenticated request shares.
    fn require_identity(caller: &Caller) -> Result<()> {
        if caller.account_id.is_nil() || caller.device_id.is_nil() {
            return Err(fault::unauthenticated(
                "the social graph needs an identified account and device",
            ));
        }
        Ok(())
    }

    /// Refuses a subject that is missing or is the caller.
    ///
    /// Self-edges are refused here rather than at the store, which also refuses them,
    /// so that the metric records an invalid request instead of a storage fault and so
    /// that the message names the field a client got wrong.
    fn require_other(caller: &Caller, subject_id: Id) -> Result<()> {
        if subject_id.is_nil() {
            return Err(fault::field_required("subject_id"));
        }
        if subject_id == caller.account_id {
            return Err(fault::validation(
                "subject_id",
                "an account cannot relate to itself",
            ));
        }
        Ok(())
    }

    /// A page size the store will accept.
    fn page(limit: Option<u16>) -> u16 {
        limit.unwrap_or(DEFAULT_PAGE).clamp(1, MAX_PAGE)
    }

    /// Which side of the pair wrote a block, if either did.
    ///
    /// Two keyed reads, short-circuited, rather than the store's symmetric helper. The
    /// helper answers "is there a block" in one call and is used where that is the
    /// whole question — filtering a search, filtering a suggestion — but a refusal
    /// needs to know *whose* block it was to pick the right code.
    ///
    /// The caller's own block is looked up first, so the disclosable answer wins when
    /// both sides have blocked each other.
    async fn block_state(&self, caller_id: Id, subject_id: Id) -> Result<BlockState> {
        if self
            .store
            .relationship(caller_id, subject_id, RelationshipKind::Block)
            .await?
            .is_some()
        {
            return Ok(BlockState::ByCaller);
        }
        if self
            .store
            .relationship(subject_id, caller_id, RelationshipKind::Block)
            .await?
            .is_some()
        {
            return Ok(BlockState::BySubject);
        }
        Ok(BlockState::Clear)
    }

    /// The refusal a block produces, by side.
    ///
    /// `None` when there is nothing to refuse.
    fn block_refusal(state: BlockState) -> Option<Error> {
        match state {
            BlockState::Clear => None,
            // Disclosed: the caller did this, and a client that can say "you blocked
            // this person" saves them working out why the button does nothing.
            BlockState::ByCaller => Some(fault::error(
                codes::BLOCKED_BY_USER,
                "the caller has blocked the subject",
            )),
            // Never disclosed as a block. Brief section 180: this must be
            // indistinguishable from a privacy setting, so it is the same code a
            // privacy setting produces and the internal text is the only difference.
            BlockState::BySubject => Some(Self::restricted("the subject has blocked the caller")),
        }
    }

    /// Which label a block belongs under, whichever side wrote it.
    ///
    /// Both sides are a block to the operator, and only the caller is kept from telling
    /// them apart. That distinction cannot be recovered from the error, because the
    /// error a subject's block produces is deliberately the same one a privacy setting
    /// produces -- so it is taken from the state instead, at every site that has the
    /// state in hand.
    fn block_outcome(state: BlockState) -> GateOutcome {
        match state {
            BlockState::Clear => GateOutcome::Allowed,
            BlockState::ByCaller | BlockState::BySubject => GateOutcome::Blocked,
        }
    }

    /// The one refusal a privacy setting produces.
    ///
    /// A constructor rather than an inline `fault::error` at nine call sites, so that
    /// the code and the fact that no detail is ever made public are decided once.
    fn restricted(internal: &str) -> Error {
        fault::error(codes::PRIVACY_RESTRICTED, internal)
    }

    /// Whether an accepted friendship exists, from the caller's side.
    ///
    /// `accepted_at` is the test, not the presence of the row. See the invariants.
    async fn are_friends(&self, caller_id: Id, subject_id: Id) -> Result<bool> {
        Ok(self
            .store
            .relationship(caller_id, subject_id, RelationshipKind::Friend)
            .await?
            .is_some_and(|edge| edge.accepted_at.is_some()))
    }

    /// Whether two accounts share at least one accepted friend.
    ///
    /// Bounded at [`MAX_MUTUAL_SCAN`] a side, and it needs no separate branch for the
    /// incomplete case: an unfinished scan and a genuinely empty intersection both
    /// produce `false`, which is a refusal. Failing closed here is deliberate — see
    /// the invariants.
    ///
    /// Returns the number of edges read as well, so the suggestion metric can report
    /// what a round cost.
    async fn shares_a_friend(&self, left: Id, right: Id) -> Result<(bool, usize)> {
        let mine = self.accepted_friends(left, MAX_MUTUAL_SCAN).await?;
        let theirs = self.accepted_friends(right, MAX_MUTUAL_SCAN).await?;
        let scanned = mine.len() + theirs.len();
        let seen: HashSet<Id> = mine.into_iter().collect();
        Ok((theirs.iter().any(|id| seen.contains(id)), scanned))
    }

    /// The other end of every accepted friendship, up to `limit`.
    async fn accepted_friends(&self, account_id: Id, limit: u16) -> Result<Vec<Id>> {
        Ok(self
            .store
            .relationships(account_id, RelationshipKind::Friend, limit)
            .await?
            .into_iter()
            .filter(|edge| edge.accepted_at.is_some())
            .map(|edge| edge.other_id)
            .collect())
    }

    /// The subject's profile, or `NOT_FOUND`.
    ///
    /// Ahead of the gate rather than behind it, because a gate cannot read a setting
    /// that is not there. Reached only after the block check, so an account that
    /// blocked the caller is never distinguished from one that does not exist.
    async fn load_profile(&self, subject_id: Id) -> Result<Profile> {
        self.store
            .profile(subject_id)
            .await?
            .ok_or_else(|| fault::not_found("account"))
    }

    /// The visibility that governs one interaction.
    fn policy_for(&self, interaction: Interaction, profile: &Profile) -> Visibility {
        match interaction {
            Interaction::Message => profile.who_can_message,
            // No `who_can_call` column exists, so the deployment default is combined
            // with the message policy. See `SocialConfig::call_default`.
            Interaction::Call => strictest(self.config.call_default, profile.who_can_message),
            Interaction::FriendRequest => profile.who_can_add,
            Interaction::LastSeen => profile.show_last_seen,
        }
    }

    /// Applies one visibility setting.
    ///
    /// `Friends` means two different things and both are correct:
    ///
    /// For a message, a call, or a last-seen time it means an accepted friendship.
    ///
    /// For a **friend request** it means friends-of-friends, because the literal
    /// reading is a contradiction — nobody could ever become a friend of an account
    /// that only accepts requests from friends, so the setting would be a synonym for
    /// `Nobody` and one of the three values would be dead. Friends-of-friends is also
    /// what the setting means everywhere else it appears in the industry, so a user
    /// who ticks it gets what they expected.
    async fn satisfies(
        &self,
        caller: &Caller,
        subject_id: Id,
        interaction: Interaction,
        policy: Visibility,
    ) -> Result<bool> {
        match policy {
            Visibility::Everyone => Ok(true),
            Visibility::Nobody => Ok(false),
            Visibility::Friends => match interaction {
                Interaction::FriendRequest => {
                    let (shared, _) = self.shares_a_friend(caller.account_id, subject_id).await?;
                    Ok(shared)
                }
                _ => self.are_friends(caller.account_id, subject_id).await,
            },
        }
    }

    /// The whole gate, without the metric.
    ///
    /// Split out so that [`Graph::may_interact`] can record an outcome and the mutating
    /// methods can record a *different* outcome for the same refusal — a blocked friend
    /// request is one gate decision and one request decision, and collapsing them would
    /// lose the ability to tell a refused request from a refused call.
    async fn gate(
        &self,
        caller: &Caller,
        subject_id: Id,
        interaction: Interaction,
    ) -> Result<GateOutcome> {
        // Interacting with yourself is allowed, except for a friend request. A note to
        // self is a real feature and your own last-seen time is not a secret from you;
        // asking to be your own friend is a client bug, and answering it with a
        // friendship would put a self-edge in a table whose primary key forbids one.
        if subject_id == caller.account_id {
            return match interaction {
                Interaction::FriendRequest => Err(fault::validation(
                    "subject_id",
                    "an account cannot befriend itself",
                )),
                _ => Ok(GateOutcome::Allowed),
            };
        }
        let state = self.block_state(caller.account_id, subject_id).await?;
        // The subject's block leaves here as an *outcome* and not as an error. It has
        // to reach the caller as the same refusal a privacy setting produces, and an
        // error carries only its code, so an error would arrive at the counter
        // indistinguishable from a privacy setting and the operator would lose the one
        // reading the caller is not allowed to have. `require_gate` turns this into
        // that refusal.
        if state == BlockState::BySubject {
            return Ok(GateOutcome::Blocked);
        }
        if let Some(refusal) = Self::block_refusal(state) {
            return Err(refusal);
        }
        let profile = match self.store.profile(subject_id).await? {
            Some(profile) => profile,
            None => return Ok(GateOutcome::Unknown),
        };
        let policy = self.policy_for(interaction, &profile);
        if self
            .satisfies(caller, subject_id, interaction, policy)
            .await?
        {
            Ok(GateOutcome::Allowed)
        } else {
            Err(Self::restricted("a visibility setting excludes the caller"))
        }
    }

    /// The gate, as another domain sees it: allowed, or an error to hand back.
    async fn require_gate(
        &self,
        caller: &Caller,
        subject_id: Id,
        interaction: Interaction,
    ) -> Result<()> {
        match self.gate(caller, subject_id, interaction).await {
            Ok(GateOutcome::Allowed) => {
                self.meters.gate(GateOutcome::Allowed);
                Ok(())
            }
            Ok(GateOutcome::Unknown) => {
                self.meters.gate(GateOutcome::Unknown);
                Err(fault::not_found("account"))
            }
            // Both refusals are already the right code; only the label differs.
            Ok(other) => {
                self.meters.gate(other);
                Err(Self::restricted("a visibility setting excludes the caller"))
            }
            Err(error) => {
                self.meters.gate(Self::outcome_of(&error));
                Err(error)
            }
        }
    }

    /// Which label a refusal belongs under.
    ///
    /// Only ever reached by a refusal whose code says everything there is to say. A
    /// block never arrives here -- the caller's own block would be readable from its
    /// code, but the subject's would not, so both are labelled from the state by
    /// [`Self::block_outcome`] before the error is built.
    fn outcome_of(error: &Error) -> GateOutcome {
        match error.code() {
            codes::BLOCKED_BY_USER => GateOutcome::Blocked,
            codes::PRIVACY_RESTRICTED => GateOutcome::Restricted,
            codes::NOT_FOUND => GateOutcome::Unknown,
            _ => GateOutcome::Unknown,
        }
    }

    /// How a refused friend request is labelled on the request counter.
    fn request_outcome_of(error: &Error) -> RequestOutcome {
        match error.code() {
            codes::BLOCKED_BY_USER => RequestOutcome::Blocked,
            codes::PRIVACY_RESTRICTED => RequestOutcome::Restricted,
            codes::RATE_LIMITED => RequestOutcome::RateLimited,
            _ => RequestOutcome::Invalid,
        }
    }

    /// Refuses an edge that would take the account past a ceiling.
    ///
    /// Counted rather than paged, which is why `SocialStore::count_relationships`
    /// exists: a limit measured from a two-hundred-row page is a two-hundred-row
    /// limit whatever the configuration says.
    async fn require_room_for(
        &self,
        account_id: Id,
        kind: RelationshipKind,
        ceiling: usize,
        what: &str,
    ) -> Result<()> {
        let held = self.store.count_relationships(account_id, kind).await?;
        if held as usize >= ceiling {
            return Err(fault::error(
                codes::QUOTA_EXCEEDED,
                format!("the {what} ceiling is reached"),
            ));
        }
        Ok(())
    }

    /// Writes one edge, dated now.
    async fn put_edge(
        &self,
        caller: &Caller,
        subject_id: Id,
        kind: RelationshipKind,
    ) -> Result<()> {
        self.store
            .put_relationship(Relationship {
                account_id: caller.account_id,
                other_id: subject_id,
                kind,
                created_at: caller.now,
                // Only a friendship has an accepted state, and a friendship is never
                // written directly: it is written by `accept_friend`, in both
                // directions, in one operation.
                accepted_at: None,
            })
            .await?;
        Ok(())
    }

    /// Removes one edge in both directions.
    ///
    /// Removing something absent is not an error at the store, which is what makes
    /// every un- operation in this crate idempotent without a read first.
    async fn drop_both(&self, left: Id, right: Id, kind: RelationshipKind) -> Result<()> {
        self.store.remove_relationship(left, right, kind).await?;
        self.store.remove_relationship(right, left, kind).await?;
        Ok(())
    }

    /// Removes the two rows that make up a pending request, whichever way it points.
    async fn drop_pending(&self, left: Id, right: Id) -> Result<()> {
        self.drop_both(left, right, RelationshipKind::PendingIncoming)
            .await?;
        self.drop_both(left, right, RelationshipKind::PendingOutgoing)
            .await
    }

    /// A listing of one kind the caller owns.
    async fn owned(
        &self,
        caller: &Caller,
        kind: RelationshipKind,
        limit: Option<u16>,
    ) -> Result<Vec<Edge>> {
        Self::require_identity(caller)?;
        self.charge(caller, LIST_COST).await?;
        Ok(self
            .store
            .relationships(caller.account_id, kind, Self::page(limit))
            .await?
            .iter()
            .map(Edge::of)
            .collect())
    }
}

#[async_trait]
impl<S, L> Graph for Social<S, L>
where
    S: AccountStore + SocialStore + ?Sized + Send + Sync,
    L: RateLimiter + ?Sized + Send + Sync,
{
    async fn request_friend(
        &self,
        caller: &Caller,
        subject_id: Id,
    ) -> Result<(FriendOutcome, Option<Notice>)> {
        Self::require_identity(caller)?;
        if let Err(error) = Self::require_other(caller, subject_id) {
            self.meters.request(RequestOutcome::Invalid);
            return Err(error);
        }
        if let Err(error) = self.charge(caller, REQUEST_COST).await {
            self.meters.request(RequestOutcome::RateLimited);
            return Err(error);
        }

        // Already friends: answered before the gate, because a settled friendship
        // outranks a setting either side changed afterwards. Somebody who narrows
        // `who_can_add` should not have their existing friends told they were refused.
        if self.are_friends(caller.account_id, subject_id).await? {
            self.meters.request(RequestOutcome::Redundant);
            return Ok((FriendOutcome::AlreadyFriends, None));
        }

        if let Err(error) = self
            .require_gate(caller, subject_id, Interaction::FriendRequest)
            .await
        {
            self.meters.request(Self::request_outcome_of(&error));
            return Err(error);
        }

        // A request already waiting from this account. Brief section 153 keys
        // idempotency on the pair, so this is an outcome and not an error: the client
        // that asked twice never saw the first answer.
        if self
            .store
            .relationship(
                caller.account_id,
                subject_id,
                RelationshipKind::PendingOutgoing,
            )
            .await?
            .is_some()
        {
            self.meters.request(RequestOutcome::Duplicate);
            return Ok((FriendOutcome::AlreadyRequested, None));
        }

        // A request already waiting from the other side. Accept it rather than stack a
        // second one: two people who asked each other before either answered should not
        // both be left staring at an unanswered request.
        if self
            .store
            .relationship(
                caller.account_id,
                subject_id,
                RelationshipKind::PendingIncoming,
            )
            .await?
            .is_some()
        {
            self.store
                .accept_friend(caller.account_id, subject_id, caller.now)
                .await?;
            self.meters.request(RequestOutcome::Reciprocated);
            self.meters.added(EdgeKind::Friend);
            return Ok((
                FriendOutcome::Accepted,
                Some(Notice::friend_accepted(
                    subject_id,
                    caller.account_id,
                    caller.now,
                )),
            ));
        }

        // Both ceilings, because a friendship costs a row on both sides and letting
        // one account fill somebody else's list would make the limit a suggestion.
        if let Err(error) = self
            .require_room_for(
                caller.account_id,
                RelationshipKind::Friend,
                self.config.max_friends,
                "friend",
            )
            .await
        {
            self.meters.request(RequestOutcome::Full);
            return Err(error);
        }
        if let Err(error) = self
            .require_room_for(
                subject_id,
                RelationshipKind::Friend,
                self.config.max_friends,
                "friend",
            )
            .await
        {
            self.meters.request(RequestOutcome::Full);
            return Err(error);
        }

        // Two rows, one per side, so that "who asked me" is an indexed read of the
        // asker's own rows rather than a scan of everybody's outgoing requests.
        self.put_edge(caller, subject_id, RelationshipKind::PendingOutgoing)
            .await?;
        self.store
            .put_relationship(Relationship {
                account_id: subject_id,
                other_id: caller.account_id,
                kind: RelationshipKind::PendingIncoming,
                created_at: caller.now,
                accepted_at: None,
            })
            .await?;
        self.meters.request(RequestOutcome::Sent);
        Ok((
            FriendOutcome::Requested,
            Some(Notice::friend_request(
                subject_id,
                caller.account_id,
                caller.now,
            )),
        ))
    }

    async fn respond_friend(
        &self,
        caller: &Caller,
        requester_id: Id,
        accept: bool,
    ) -> Result<(RespondOutcome, Option<Notice>)> {
        Self::require_identity(caller)?;
        Self::require_other(caller, requester_id)?;
        self.charge(caller, RESPOND_COST).await?;

        // The caller's own incoming row is the authority on whether there is anything
        // to answer. Checking the requester's outgoing row instead would let anybody
        // accept a request that was addressed to somebody else.
        if self
            .store
            .relationship(
                caller.account_id,
                requester_id,
                RelationshipKind::PendingIncoming,
            )
            .await?
            .is_none()
        {
            self.meters.response(ResponseOutcome::Missing);
            return Err(fault::not_found("friend request"));
        }

        if !accept {
            self.drop_pending(caller.account_id, requester_id).await?;
            self.meters.response(ResponseOutcome::Declined);
            // No notice. "X declined your friend request" is a message whose only
            // function is to make a private decision somebody else's business.
            return Ok((RespondOutcome::Declined, None));
        }

        // Re-checked at acceptance, not only at request time. A block written while
        // the request sat in the queue has to hold, and the request could have been
        // waiting for a month.
        let state = self.block_state(caller.account_id, requester_id).await?;
        if let Some(refusal) = Self::block_refusal(state) {
            // The stale request goes too. Leaving it would show the blocked account's
            // name in a pending list forever with no way to clear it.
            self.drop_pending(caller.account_id, requester_id).await?;
            self.meters.response(ResponseOutcome::Missing);
            self.meters.gate(Self::block_outcome(state));
            return Err(refusal);
        }

        self.store
            .accept_friend(caller.account_id, requester_id, caller.now)
            .await?;
        self.meters.response(ResponseOutcome::Accepted);
        self.meters.added(EdgeKind::Friend);
        Ok((
            RespondOutcome::Accepted,
            Some(Notice::friend_accepted(
                requester_id,
                caller.account_id,
                caller.now,
            )),
        ))
    }

    async fn remove_friend(&self, caller: &Caller, subject_id: Id) -> Result<()> {
        Self::require_identity(caller)?;
        Self::require_other(caller, subject_id)?;
        self.charge(caller, EDGE_COST).await?;
        // Both directions, and the pending rows too: un-friending somebody who had
        // also just asked to be a friend again should not leave the request behind.
        self.drop_both(caller.account_id, subject_id, RelationshipKind::Friend)
            .await?;
        self.drop_pending(caller.account_id, subject_id).await?;
        self.meters.removed(EdgeKind::Friend);
        Ok(())
    }

    async fn follow(&self, caller: &Caller, subject_id: Id) -> Result<()> {
        Self::require_identity(caller)?;
        Self::require_other(caller, subject_id)?;
        self.charge(caller, EDGE_COST).await?;
        let state = self.block_state(caller.account_id, subject_id).await?;
        if let Some(refusal) = Self::block_refusal(state) {
            self.meters.gate(Self::block_outcome(state));
            return Err(refusal);
        }
        // The subject must exist. A follow of a deleted account is a row pointing at
        // nothing that shows up in the follower's list forever.
        self.load_profile(subject_id).await?;
        self.require_room_for(
            caller.account_id,
            RelationshipKind::Follow,
            self.config.max_following,
            "following",
        )
        .await?;
        self.put_edge(caller, subject_id, RelationshipKind::Follow)
            .await?;
        self.meters.added(EdgeKind::Follow);
        Ok(())
    }

    async fn unfollow(&self, caller: &Caller, subject_id: Id) -> Result<()> {
        Self::require_identity(caller)?;
        Self::require_other(caller, subject_id)?;
        self.charge(caller, EDGE_COST).await?;
        self.store
            .remove_relationship(caller.account_id, subject_id, RelationshipKind::Follow)
            .await?;
        self.meters.removed(EdgeKind::Follow);
        Ok(())
    }

    async fn block(&self, caller: &Caller, subject_id: Id) -> Result<()> {
        Self::require_identity(caller)?;
        Self::require_other(caller, subject_id)?;
        self.charge(caller, EDGE_COST).await?;
        self.require_room_for(
            caller.account_id,
            RelationshipKind::Block,
            self.config.max_blocks,
            "block",
        )
        .await?;

        // What the block is about to undo, read before it is undone. Four keyed reads on
        // an operation a person performs by hand, so that the removal counters keep
        // meaning "an edge that existed is gone" rather than "a block happened": most
        // blocks are of strangers, and counting a friendship removal for every one of
        // those would leave the friend series unable to answer the single question it
        // exists for.
        let was_friend = self
            .store
            .relationship(caller.account_id, subject_id, RelationshipKind::Friend)
            .await?
            .is_some();
        let was_following = self
            .store
            .relationship(caller.account_id, subject_id, RelationshipKind::Follow)
            .await?
            .is_some()
            || self
                .store
                .relationship(subject_id, caller.account_id, RelationshipKind::Follow)
                .await?
                .is_some();
        let was_favorite = self
            .store
            .relationship(caller.account_id, subject_id, RelationshipKind::Favorite)
            .await?
            .is_some();

        // The block row first. If any of what follows fails, the state left behind is
        // "blocked, and some edges not yet cleared" — which still stops contact. The
        // other order leaves "edges cleared, not blocked", which stops nothing and
        // silently destroyed a friendship.
        self.put_edge(caller, subject_id, RelationshipKind::Block)
            .await?;

        // Everything a block has to undo, in both directions. Leaving the follow edges
        // in place is the classic version of this bug: new contact is stopped while
        // the blocked account keeps receiving everything the blocker posts.
        self.drop_both(caller.account_id, subject_id, RelationshipKind::Friend)
            .await?;
        self.drop_pending(caller.account_id, subject_id).await?;
        self.drop_both(caller.account_id, subject_id, RelationshipKind::Follow)
            .await?;
        // And the favourite, which would otherwise keep the blocked account pinned to
        // the top of the blocker's own list.
        self.store
            .remove_relationship(caller.account_id, subject_id, RelationshipKind::Favorite)
            .await?;

        self.meters.added(EdgeKind::Block);
        // The edges a block took with it are counted as removals, because that is what
        // they are. An operator watching `added` against `removed` for one kind is
        // watching how many of that edge exist, and a removal path that reported nothing
        // would make the two series drift apart by exactly the number of blocks.
        if was_friend {
            self.meters.removed(EdgeKind::Friend);
        }
        if was_following {
            self.meters.removed(EdgeKind::Follow);
        }
        if was_favorite {
            self.meters.removed(EdgeKind::Favorite);
        }
        Ok(())
    }

    async fn unblock(&self, caller: &Caller, subject_id: Id) -> Result<()> {
        Self::require_identity(caller)?;
        Self::require_other(caller, subject_id)?;
        self.charge(caller, EDGE_COST).await?;
        // Only the caller's own block. Removing the other direction would let somebody
        // clear the block that was written against them.
        self.store
            .remove_relationship(caller.account_id, subject_id, RelationshipKind::Block)
            .await?;
        self.meters.removed(EdgeKind::Block);
        Ok(())
    }

    async fn mute(&self, caller: &Caller, subject_id: Id, on: bool) -> Result<()> {
        Self::require_identity(caller)?;
        Self::require_other(caller, subject_id)?;
        self.charge(caller, EDGE_COST).await?;
        if !on {
            self.store
                .remove_relationship(caller.account_id, subject_id, RelationshipKind::Mute)
                .await?;
            self.meters.removed(EdgeKind::Mute);
            return Ok(());
        }
        // And deliberately nothing else. A block tears down the friendship and the
        // follows because it is a wall; a mute is a volume control, and the version
        // that quietly deleted a friendship because its owner wanted one loud
        // account quieter would be this crate making a decision the caller never
        // made. The edges stay, and both parties keep them.
        self.require_room_for(
            caller.account_id,
            RelationshipKind::Mute,
            self.config.max_mutes,
            "mute",
        )
        .await?;
        self.put_edge(caller, subject_id, RelationshipKind::Mute)
            .await?;
        self.meters.added(EdgeKind::Mute);
        Ok(())
    }

    async fn muted(&self, caller: &Caller, limit: Option<u16>) -> Result<Vec<Edge>> {
        self.owned(caller, RelationshipKind::Mute, limit).await
    }

    async fn set_favorite(&self, caller: &Caller, subject_id: Id, favorite: bool) -> Result<()> {
        Self::require_identity(caller)?;
        Self::require_other(caller, subject_id)?;
        self.charge(caller, EDGE_COST).await?;
        if !favorite {
            self.store
                .remove_relationship(caller.account_id, subject_id, RelationshipKind::Favorite)
                .await?;
            self.meters.removed(EdgeKind::Favorite);
            return Ok(());
        }
        // A favourite is private and tells the other account nothing, so the only gate
        // is the caller's own block: keeping somebody you blocked at the top of your
        // list is a contradiction the product should not store.
        if self
            .store
            .relationship(caller.account_id, subject_id, RelationshipKind::Block)
            .await?
            .is_some()
        {
            return Err(fault::error(
                codes::BLOCKED_BY_USER,
                "the caller has blocked the subject",
            ));
        }
        self.require_room_for(
            caller.account_id,
            RelationshipKind::Favorite,
            MAX_FAVORITES,
            "favourite",
        )
        .await?;
        self.put_edge(caller, subject_id, RelationshipKind::Favorite)
            .await?;
        self.meters.added(EdgeKind::Favorite);
        Ok(())
    }

    async fn friends(&self, caller: &Caller, limit: Option<u16>) -> Result<Vec<Edge>> {
        Ok(self
            .owned(caller, RelationshipKind::Friend, limit)
            .await?
            .into_iter()
            // Pending rows are stored under their own kinds, so this filter is belt and
            // braces — but a `Friend` row with no `accepted_at` is exactly what a
            // partially applied acceptance would leave, and it must not read as a
            // friendship.
            .filter(|edge| edge.accepted)
            .collect())
    }

    async fn pending(&self, caller: &Caller, limit: Option<u16>) -> Result<Pending> {
        Self::require_identity(caller)?;
        self.charge(caller, LIST_COST).await?;
        let page = Self::page(limit);
        // Both from the caller's own rows, which is what the two kinds are for: no
        // reverse scan of everybody's outgoing requests to answer "who asked me".
        let incoming = self
            .store
            .relationships(caller.account_id, RelationshipKind::PendingIncoming, page)
            .await?;
        let outgoing = self
            .store
            .relationships(caller.account_id, RelationshipKind::PendingOutgoing, page)
            .await?;
        Ok(Pending {
            incoming: incoming.iter().map(Edge::of).collect(),
            outgoing: outgoing.iter().map(Edge::of).collect(),
        })
    }

    async fn following(&self, caller: &Caller, limit: Option<u16>) -> Result<Vec<Edge>> {
        self.owned(caller, RelationshipKind::Follow, limit).await
    }

    async fn followers(&self, caller: &Caller, limit: Option<u16>) -> Result<Vec<Edge>> {
        Self::require_identity(caller)?;
        self.charge(caller, LIST_COST).await?;
        // The one listing read from the other side of the edge, and the reason the
        // store keeps a reverse index: without it this is a scan of every follow in
        // the system to answer "who follows me".
        Ok(self
            .store
            .inbound_relationships(
                caller.account_id,
                RelationshipKind::Follow,
                Self::page(limit),
            )
            .await?
            .iter()
            // `other_id` on an inbound row is the caller, so the projection has to name
            // the owner instead. Reusing `Edge::of` here would return the caller's own
            // id N times.
            .map(|row| Edge {
                other_id: row.account_id,
                kind: row.kind,
                since: row.created_at,
                accepted: true,
            })
            .collect())
    }

    async fn blocked(&self, caller: &Caller, limit: Option<u16>) -> Result<Vec<Edge>> {
        self.owned(caller, RelationshipKind::Block, limit).await
    }

    async fn favorites(&self, caller: &Caller, limit: Option<u16>) -> Result<Vec<Edge>> {
        self.owned(caller, RelationshipKind::Favorite, limit).await
    }

    async fn standing(&self, caller: &Caller, subject_id: Id) -> Result<Standing> {
        Self::require_identity(caller)?;
        Self::require_other(caller, subject_id)?;
        // Priced from the opcode, because this is what a profile screen asks for.
        self.charge_opcode(caller, Opcode::ProfileFetch).await?;
        let friend = self
            .store
            .relationship(caller.account_id, subject_id, RelationshipKind::Friend)
            .await?;
        Ok(Standing {
            friends: friend.as_ref().is_some_and(|e| e.accepted_at.is_some()),
            requested: self
                .store
                .relationship(
                    caller.account_id,
                    subject_id,
                    RelationshipKind::PendingOutgoing,
                )
                .await?
                .is_some(),
            awaiting_response: self
                .store
                .relationship(
                    caller.account_id,
                    subject_id,
                    RelationshipKind::PendingIncoming,
                )
                .await?
                .is_some(),
            following: self
                .store
                .relationship(caller.account_id, subject_id, RelationshipKind::Follow)
                .await?
                .is_some(),
            // Read from the other side's row, which is a fact about the subject that
            // the subject chose to make public by following. Nothing here reports the
            // subject's *block*: see `Standing`.
            followed_by: self
                .store
                .relationship(subject_id, caller.account_id, RelationshipKind::Follow)
                .await?
                .is_some(),
            favorite: self
                .store
                .relationship(caller.account_id, subject_id, RelationshipKind::Favorite)
                .await?
                .is_some(),
            blocked: self
                .store
                .relationship(caller.account_id, subject_id, RelationshipKind::Block)
                .await?
                .is_some(),
        })
    }

    async fn may_interact(
        &self,
        caller: &Caller,
        subject_id: Id,
        interaction: Interaction,
    ) -> Result<()> {
        // Not charged. See the module docs.
        Self::require_identity(caller)?;
        if subject_id.is_nil() {
            return Err(fault::field_required("subject_id"));
        }
        self.require_gate(caller, subject_id, interaction).await
    }

    async fn suggest(&self, caller: &Caller, limit: Option<u16>) -> Result<Vec<Suggestion>> {
        Self::require_identity(caller)?;
        self.charge(caller, SUGGEST_COST).await?;
        let want = Self::page(limit) as usize;

        let mine = self
            .accepted_friends(caller.account_id, MAX_MUTUAL_SCAN)
            .await?;
        let mut scanned = mine.len();

        // Everybody already accounted for. Suggesting an existing friend is noise;
        // suggesting somebody with a request in flight is the product asking twice.
        let mut known: HashSet<Id> = mine.iter().copied().collect();
        known.insert(caller.account_id);
        for kind in [
            RelationshipKind::PendingOutgoing,
            RelationshipKind::PendingIncoming,
            RelationshipKind::Block,
        ] {
            let rows = self
                .store
                .relationships(caller.account_id, kind, MAX_MUTUAL_SCAN)
                .await?;
            scanned += rows.len();
            known.extend(rows.into_iter().map(|row| row.other_id));
        }

        // One hop, over the most recent friends only. See `SUGGEST_SEED_FRIENDS`.
        let mut tally: Vec<(Id, u32)> = Vec::new();
        for friend in mine.into_iter().take(SUGGEST_SEED_FRIENDS as usize) {
            let theirs = self.accepted_friends(friend, MAX_MUTUAL_SCAN).await?;
            scanned += theirs.len();
            for candidate in theirs {
                if known.contains(&candidate) {
                    continue;
                }
                match tally.iter_mut().find(|(id, _)| *id == candidate) {
                    Some((_, count)) => *count += 1,
                    None => tally.push((candidate, 1)),
                }
            }
        }

        // Most shared friends first, then by id so the order is stable across calls —
        // a suggestion list that reshuffles on every refresh looks broken.
        tally.sort_by(|left, right| right.1.cmp(&left.1).then(left.0.cmp(&right.0)));
        tally.truncate(want);

        // The blocks written by the other side are checked last, because that is one
        // read per surviving candidate and the list is already short. An account that
        // blocked the caller must not be suggested to them, and `known` cannot see
        // that: it only holds the caller's own blocks.
        let mut out = Vec::with_capacity(tally.len());
        for (account_id, mutual_friends) in tally {
            if self
                .store
                .is_blocked_either_way(caller.account_id, account_id)
                .await?
            {
                continue;
            }
            out.push(Suggestion {
                account_id,
                mutual_friends,
            });
        }
        self.meters.suggestions(out.len(), scanned);
        Ok(out)
    }

    async fn search(&self, caller: &Caller, query: &str, limit: Option<u16>) -> Result<Vec<Found>> {
        Self::require_identity(caller)?;
        if !query_is_usable(query) {
            return Err(fault::validation(
                "query",
                "a search term must be non-empty and short",
            ));
        }
        self.charge(caller, SEARCH_COST).await?;
        // The store applies `searchable` and `status = active` in the query rather than
        // after ranking, so an account that opted out never reaches this code.
        let rows = self
            .store
            .search_accounts(query.trim(), Self::page(limit))
            .await?;
        let mut out = Vec::with_capacity(rows.len());
        for (account, profile) in rows {
            if account.account_id == caller.account_id {
                continue;
            }
            // Symmetric, and applied here rather than in the store: a blocked account
            // must not be findable, and the account that did the blocking should not
            // find the person they blocked either.
            if self
                .store
                .is_blocked_either_way(caller.account_id, account.account_id)
                .await?
            {
                continue;
            }
            out.push(Found {
                account_id: account.account_id,
                username: account.username,
                display_name: profile.display_name,
                avatar_media_id: profile.avatar_media_id,
            });
        }
        self.meters.search(out.len());
        Ok(out)
    }

    async fn profiles(&self, caller: &Caller, account_ids: &[Id]) -> Result<Vec<ProfileCard>> {
        Self::require_identity(caller)?;
        if account_ids.is_empty() {
            return Err(fault::field_required("user_ids"));
        }
        if account_ids.len() > MAX_PROFILE_BATCH {
            return Err(fault::validation(
                "user_ids",
                "too many accounts in one profile fetch",
            ));
        }

        // Deduplicated before the charge, not after, so that the bound above and the
        // flat price both apply to work that will actually be done. A client that sends
        // the same id sixty-four times gets one profile and pays for one batch; without
        // this it would get one profile and cost sixty-four times the reads.
        let mut wanted: Vec<Id> = Vec::with_capacity(account_ids.len());
        for &account_id in account_ids {
            if account_id.is_nil() {
                return Err(fault::field_required("user_ids"));
            }
            if !wanted.contains(&account_id) {
                wanted.push(account_id);
            }
        }

        self.charge_opcode(caller, Opcode::ProfileFetch).await?;

        let asked = wanted.len();
        let mut out = Vec::with_capacity(asked);
        for account_id in wanted {
            // Symmetric, and first. A profile is the one read where getting this wrong
            // is invisible in testing: the store answers, the response is well formed,
            // and the only thing wrong with it is that it went to somebody who was
            // blocked. The caller's own id skips the check rather than being special
            // cased later, because nobody can block themselves and a nil-result branch
            // for the self case would be a branch that has to stay correct forever.
            if account_id != caller.account_id
                && self
                    .store
                    .is_blocked_either_way(caller.account_id, account_id)
                    .await?
            {
                continue;
            }
            // Missing rows are omitted, not reported. See `Graph::profiles`: a caller
            // that could tell "no such account" from "blocked you" has been told which
            // one it was. Note that both reads have to land -- an account with no
            // profile row is mid-registration, and a card with a defaulted display name
            // is worse than no card.
            let Some(profile) = self.store.profile(account_id).await? else {
                continue;
            };
            let Some(account) = self.store.account_by_id(account_id).await? else {
                continue;
            };
            out.push(ProfileCard {
                account_id,
                username: account.username,
                display_name: profile.display_name,
                bio: profile.bio,
                avatar_media_id: profile.avatar_media_id,
                country: account.country,
                locale: account.locale,
            });
        }
        self.meters.profiles(asked, out.len());
        Ok(out)
    }
}
