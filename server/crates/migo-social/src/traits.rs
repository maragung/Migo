//! The social contract.
//!
//! One trait, because every method here reads the same table and is governed by the
//! same first question: has either of these two accounts blocked the other. A narrower
//! trait that could list friends but not answer that question would be the half of the
//! operation without the rule, and the half without the rule is the half that gets
//! called from a path nobody reviewed.
//!
//! # Why [`Graph::may_interact`] is on this trait and not inlined elsewhere
//!
//! Because a block has to stop contact everywhere, and the crates that need to honour
//! it must not depend on this one. `docs/01-architecture.md` forbids two layer-3
//! crates from depending on each other, and messaging, notify, and the call path all
//! genuinely need to know whether one account may reach another.
//!
//! So the check is published here as one method and the composition root wires it: the
//! gateway asks social, then asks messaging. Two domain crates never see each other,
//! and the rule that a blocked account cannot get through exists exactly once.
//!
//! The alternative — each crate reading `relationship(a, b, Block)` for itself — is
//! the version where the direction gets checked one way round in four places and both
//! ways round in three, and "blocked users can still reply in threads" ships.
//!
//! # Why [`Graph::standing`] returns a struct and not a relationship list
//!
//! A profile screen needs seven booleans and asks for them once. The list form would
//! be four store reads for the caller to reduce into the same seven, and every client
//! would reduce them slightly differently — which is how one client shows "friends"
//! for a request that is still pending.
//!
//! # What is deliberately not here
//!
//! **No profile read and no profile write.** `who_can_message`, `who_can_add`, and
//! `show_last_seen` are read here to answer a gate, and set through the account
//! service that owns the profile row. A settings writer in this crate would be a
//! second owner of one row, and the first time the two disagreed a privacy setting
//! would be saved and then quietly overwritten.
//!
//! **No presence.** `Interaction::LastSeen` answers *whether* the caller may see a
//! last-seen time; the time itself lives in `migo_presence`, which asks this crate the
//! question and then decides what to send. Returning the timestamp from here would put
//! two crates in charge of one field.
//!
//! **No mutual-friend graph beyond one hop.** [`Graph::suggest`] looks at the caller's
//! friends and their friends and stops. "People you may know" over three hops is a
//! graph traversal on a request path, and the honest place for it is a job that writes
//! its answers down.
//!
//! **No frame.** See the [`notice`](crate::notice) module: brief section 145 marks the
//! social opcodes `STATUS: SPEC`, so a friend request is delivered as a notification
//! and not as a social event.

use async_trait::async_trait;
use migo_core::{Id, Result};

use crate::model::{
    Caller, Edge, Found, FriendOutcome, Interaction, Pending, RespondOutcome, Standing, Suggestion,
};
use crate::notice::Notice;

/// Everything the social graph does.
#[async_trait]
pub trait Graph: Send + Sync {
    /// Asks somebody to be a friend.
    ///
    /// Idempotent on the pair of accounts, which brief section 153 requires: asking
    /// twice returns [`FriendOutcome::AlreadyRequested`] rather than an error, because
    /// the client that asked twice is a client whose first reply was lost, and telling
    /// it the request failed would be a lie about a request that is waiting.
    ///
    /// A request that crosses one going the other way accepts it. Two people who asked
    /// each other before either answered should not both be left staring at an
    /// unanswered request.
    ///
    /// The [`Notice`] is `None` whenever nothing changed, so a caller with nothing to
    /// deliver cannot forget to check.
    async fn request_friend(
        &self,
        caller: &Caller,
        subject_id: Id,
    ) -> Result<(FriendOutcome, Option<Notice>)>;

    /// Accepts or declines a request this account received.
    ///
    /// `accept` rather than two methods, because the two share every check — there
    /// must be a request, it must be addressed to the caller, and neither side may
    /// have blocked the other since — and a shape that let one of them skip a check
    /// would eventually let one of them skip a check.
    ///
    /// Declining is silent. There is no notice for it, because "X declined your friend
    /// request" is a message whose only function is to make a private decision
    /// somebody else's business.
    async fn respond_friend(
        &self,
        caller: &Caller,
        requester_id: Id,
        accept: bool,
    ) -> Result<(RespondOutcome, Option<Notice>)>;

    /// Ends a friendship.
    ///
    /// Both edges, in one operation, and no notice. A friendship stored on one side
    /// only is how "we are not friends but you are still in my list" happens, and a
    /// notification would turn a quiet exit into a confrontation.
    async fn remove_friend(&self, caller: &Caller, subject_id: Id) -> Result<()>;

    /// Follows an account.
    ///
    /// One direction and no consent, which is what makes it different from a
    /// friendship. Refused when either side has blocked the other: a follow is a
    /// subscription to somebody's activity, and a block that did not stop one would be
    /// a block that did not work.
    async fn follow(&self, caller: &Caller, subject_id: Id) -> Result<()>;

    /// Stops following. Idempotent.
    async fn unfollow(&self, caller: &Caller, subject_id: Id) -> Result<()>;

    /// Blocks an account.
    ///
    /// Does four things at once, because a block that did three of them is a bug
    /// waiting for somebody to notice: it writes the block edge, drops any friendship
    /// in both directions, drops any pending request in either direction, and drops
    /// the follow edges in both directions.
    ///
    /// Not doing the last one is the classic version of this bug — the block stops new
    /// contact while the blocked account keeps receiving everything the blocker posts.
    async fn block(&self, caller: &Caller, subject_id: Id) -> Result<()>;

    /// Lifts a block. Idempotent.
    ///
    /// Restores nothing. The friendship and the follows the block removed are gone,
    /// and rebuilding them would be this crate deciding that two people who fell out
    /// and made up want the same graph they had before.
    async fn unblock(&self, caller: &Caller, subject_id: Id) -> Result<()>;

    /// Marks or unmarks a favourite.
    ///
    /// A favourite is private: it changes how the caller's own client sorts a list and
    /// tells the other account nothing. So there is no notice, and no gate beyond the
    /// block check — favouriting somebody is not contact.
    async fn set_favorite(&self, caller: &Caller, subject_id: Id, favorite: bool) -> Result<()>;

    /// Settled friendships, newest first.
    ///
    /// Pending requests are excluded. They are in [`Graph::pending`], because a
    /// pending request that appeared in a friend list would let anybody read a
    /// `Friends`-only field by asking to be a friend and never being answered.
    async fn friends(&self, caller: &Caller, limit: Option<u16>) -> Result<Vec<Edge>>;

    /// Requests waiting, in both directions.
    ///
    /// One call for both, because the screen that shows them shows them together and
    /// two calls would be two round trips for one list.
    async fn pending(&self, caller: &Caller, limit: Option<u16>) -> Result<Pending>;

    /// Accounts this one follows, newest first.
    async fn following(&self, caller: &Caller, limit: Option<u16>) -> Result<Vec<Edge>>;

    /// Accounts that follow this one, newest first.
    async fn followers(&self, caller: &Caller, limit: Option<u16>) -> Result<Vec<Edge>>;

    /// Accounts this one has blocked.
    ///
    /// Only the caller's own list. There is no way to ask who has blocked *you*, and
    /// there never will be: brief section 180 arranges the error codes so a caller
    /// cannot tell a block from a privacy setting, and an endpoint that listed the
    /// blockers would answer the question the codes were designed not to answer.
    async fn blocked(&self, caller: &Caller, limit: Option<u16>) -> Result<Vec<Edge>>;

    /// Accounts this one has marked as favourites.
    async fn favorites(&self, caller: &Caller, limit: Option<u16>) -> Result<Vec<Edge>>;

    /// What one account is to another, from the caller's side.
    ///
    /// Never reports the subject's block of the caller. See [`Standing`].
    async fn standing(&self, caller: &Caller, subject_id: Id) -> Result<Standing>;

    /// Whether the caller may do this to the subject.
    ///
    /// The cross-domain entry point, and the reason this trait exists in the shape it
    /// does. `Ok(())` means go ahead; the error carries a code the caller can return
    /// verbatim.
    ///
    /// Both refusals — a block and a privacy setting — return the same code, which is
    /// brief section 180: a caller that could tell them apart could use a friend
    /// request as a probe for whether somebody blocked them. The two are still
    /// distinguishable in the metrics, where an aggregate count names nobody.
    ///
    /// Not rate limited. It is called on the path of an operation that has already
    /// been charged — a message send charges `MessageSend`, then asks this crate
    /// whether the recipient accepts messages — and charging again would bill one user
    /// action twice and make a send budget depend on how many gates the implementation
    /// happens to consult.
    async fn may_interact(
        &self,
        caller: &Caller,
        subject_id: Id,
        interaction: Interaction,
    ) -> Result<()>;

    /// Accounts the caller might know, by shared friends.
    ///
    /// Bounded at both hops, and it excludes existing friends, pending requests, and
    /// anybody either side has blocked. A suggestion list that offered up the account
    /// somebody blocked last week would be the product undoing a decision the user
    /// made deliberately.
    async fn suggest(&self, caller: &Caller, limit: Option<u16>) -> Result<Vec<Suggestion>>;

    /// Finds accounts by username or display-name prefix.
    ///
    /// Only accounts that opted into being searchable, which the store enforces in the
    /// query rather than after ranking, and never an account either side has blocked.
    async fn search(&self, caller: &Caller, query: &str, limit: Option<u16>) -> Result<Vec<Found>>;
}
