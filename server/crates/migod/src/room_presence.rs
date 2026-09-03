//! Room presence and reconnect lifecycle: the bookkeeping that turns a socket coming up
//! or going down into the room-level events and online counts the domain crates refuse to
//! compute.
//!
//! # Why this lives in the composition root and not in `migo-rooms`
//!
//! Two facts have to meet here that meet nowhere else. `migo-rooms` owns *who is a member*
//! and knows nothing about sockets; the gateway owns *who is connected* and knows nothing
//! about rooms. The online count of a room is the intersection of the two, and brief
//! section 14 forbids computing it by querying presence on every listing — so it is kept as
//! an in-memory tally right where both facts are already in hand: the composition root that
//! wired the gateway to the rooms service. `migo_rooms::view::ONLINE_COUNT_UNSET` names the
//! zero this component overwrites, and `Roomkeeper::timeout_member` is the one door it opens
//! back into the domain.
//!
//! # What a connection edge does (brief sections 183 and 184)
//!
//! Only the edges matter — an account with three sockets is exactly as online as one with a
//! single socket, so nothing happens on a second or third connect, and nothing on the second
//! or third disconnect. The two that count are:
//!
//! * **first socket up (0 → 1):** every room the account is in gains one online member, so a
//!   fresh online count goes out for each; and if any of those rooms had been told the
//!   account went offline, they are told it is back with a `Reconnected`.
//! * **last socket down (1 → 0):** every room is told the member went dark with a
//!   `Disconnected` — but the seat is kept, because a dropped socket or a backgrounded tab is
//!   not a departure. A two-minute grace timer is armed; if it fires with the account still
//!   offline and still a member, [`Roomkeeper::timeout_member`](migo_rooms::Roomkeeper::timeout_member)
//!   makes the departure real with a `Left`.
//!
//! The owner is exempt from the timeout: `timeout_member` refuses to remove them, so a room's
//! creator keeps the room by closing a laptop. They are still shown offline and still owed a
//! `Reconnected` when they return.
//!
//! # Why a generation counter
//!
//! A grace timer armed two minutes ago must not remove an account that reconnected ninety
//! seconds in. Every connect and disconnect bumps a per-account generation; the timer captures
//! the value it was armed with and does nothing if it has moved. That is the whole of the
//! cancellation — no timer handle to track, no lock held across the wait, and a
//! reconnect-then-redisconnect (which re-arms with a newer generation) leaves the stale timer
//! harmless.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use parking_lot::Mutex;

use migo_core::{Id, Timestamp};
use migo_gateway::Gateway;
use migo_protocol::{
    MemberChange, Opcode, RoomMemberEvent, RoomStateEvent, RoomVoteEvent, Topic, TopicKind,
};
use migo_rooms::{view, Broadcast as RoomBroadcast, Fanout as RoomFanout, SharedRooms};
use migo_store::SharedStore;

use crate::dispatch::coalesce_key_of;

/// How long a member stays in their rooms after their last session drops (brief section 184).
///
/// Two minutes, long enough to cover a train tunnel, a phone locking, or a tab going to sleep,
/// and short enough that a genuine departure is not left haunting a roster for an afternoon.
pub(crate) const RECONNECT_GRACE_MS: u64 = 120_000;

/// How many roster rows to read per page when counting who is online.
///
/// The store clamps any request to this ceiling anyway, and it is far above a room's member
/// cap, so in practice one page is the whole roster; the read still pages so that a larger cap
/// in a later build stays correct rather than silently undercounting.
const ROSTER_PAGE: u16 = 200;

/// A late-bound handle to the gateway.
///
/// The dispatcher is built *before* the gateway — the gateway is handed the dispatcher at
/// construction and owns it thereafter — so anything the dispatcher needs in order to publish
/// out of band, with no client request in hand, cannot hold an `Arc<Gateway>` at its own
/// construction. This is the one-slot cell the composition root fills the instant the gateway
/// is open, exactly as it hands the same `Arc<Gateway>` to the mesh transport a few lines
/// later. Until it is filled every publish through it is a no-op, which is the right behaviour
/// for the sliver of startup before the gateway can accept anything at all.
pub struct GatewayHandle {
    gateway: OnceLock<Arc<Gateway>>,
}

impl GatewayHandle {
    /// An empty handle, to be filled once the gateway is open.
    #[must_use]
    pub fn new() -> Self {
        Self {
            gateway: OnceLock::new(),
        }
    }

    /// Binds the gateway. The first call wins and later calls are ignored, because a process
    /// has exactly one gateway and rebinding it would be a bug rather than a feature.
    pub fn set(&self, gateway: Arc<Gateway>) {
        let _ = self.gateway.set(gateway);
    }

    /// The gateway, once bound; `None` during the startup window before it is.
    pub(crate) fn get(&self) -> Option<&Arc<Gateway>> {
        self.gateway.get()
    }
}

impl Default for GatewayHandle {
    fn default() -> Self {
        Self::new()
    }
}

/// The out-of-band publish surface the room-presence component needs.
///
/// A trait rather than the gateway itself, so the component's logic — which rooms hear a
/// disconnect, when a reconnect is owed, when a timeout empties a seat — can be exercised by
/// handing it a recorder and reading back what it would have sent, with no socket and no
/// runtime full of transport. Production wires [`GatewayPublisher`], which forwards to the same
/// hub the request path and the mesh publish through.
pub(crate) trait RoomPublisher: Send + Sync {
    /// A membership lifecycle change on a room's topic.
    ///
    /// Published like the request path's member events — not coalesced by anything that would
    /// let a `Disconnected` and a later `Reconnected` collapse into one, because a subscriber
    /// that saw only the survivor would have the wrong picture of who is in the room.
    fn publish_member(&self, room_id: Id, event: &RoomMemberEvent, now: Timestamp);

    /// An online-count change on a room's topic, coalesced by room (section 154) so a backed-up
    /// consumer keeps only the latest count rather than a queue of stale ones.
    fn publish_state(&self, room_id: Id, event: &RoomStateEvent, now: Timestamp);

    /// A kick-vote tally on a room's topic, coalesced by room the same way the state events
    /// are: only the latest tally matters, and a queue of stale ones would have a client
    /// count the same voice twice.
    ///
    /// Nothing on the presence path produces one — a vote is a request-path fanout — but the
    /// forwarding helper takes a whole [`RoomFanout`](migo_rooms::Fanout) and the match must
    /// be total, so the surface carries it rather than the helper quietly dropping it.
    fn publish_vote(&self, room_id: Id, event: &RoomVoteEvent, now: Timestamp);
}

/// The production [`RoomPublisher`]: the gateway, once it exists.
pub(crate) struct GatewayPublisher {
    gateway: Arc<GatewayHandle>,
}

impl GatewayPublisher {
    /// Wraps the late-bound gateway handle.
    pub(crate) fn new(gateway: Arc<GatewayHandle>) -> Self {
        Self { gateway }
    }

    /// The topic every room event fans out to.
    fn room_topic(room_id: Id) -> Topic {
        Topic {
            kind: TopicKind::Room,
            id: room_id,
        }
    }
}

impl RoomPublisher for GatewayPublisher {
    fn publish_member(&self, room_id: Id, event: &RoomMemberEvent, now: Timestamp) {
        if let Some(gateway) = self.gateway.get() {
            gateway.broadcast_to_topic(
                &Self::room_topic(room_id),
                Opcode::RoomMemberEvent,
                event,
                now,
            );
        }
    }

    fn publish_state(&self, room_id: Id, event: &RoomStateEvent, now: Timestamp) {
        if let Some(gateway) = self.gateway.get() {
            gateway.broadcast_to_topic_coalesced(
                &Self::room_topic(room_id),
                Opcode::RoomStateEvent,
                event,
                coalesce_key_of(&room_id),
                now,
            );
        }
    }

    fn publish_vote(&self, room_id: Id, event: &RoomVoteEvent, now: Timestamp) {
        if let Some(gateway) = self.gateway.get() {
            gateway.broadcast_to_topic_coalesced(
                &Self::room_topic(room_id),
                Opcode::RoomVoteEvent,
                event,
                coalesce_key_of(&room_id),
                now,
            );
        }
    }
}

/// One account's connection state, as room presence sees it.
#[derive(Default)]
struct AccountPresence {
    /// Live sessions right now. Only the 0 ↔ non-zero edges of this number matter to a room.
    sessions: u32,
    /// Bumped on every connect and disconnect. A grace timer captures the value at the moment
    /// it was armed and refuses to act if it has changed since — which is how a reconnect (or a
    /// reconnect and another disconnect) cancels a stale timer without a lock held across the
    /// wait.
    generation: u64,
    /// Rooms told this account went offline and not yet told it came back or left. What a
    /// reconnect consults to decide which rooms are owed a `Reconnected`, and what stops a
    /// second disconnect from announcing one twice.
    announced_offline: HashSet<Id>,
}

/// The per-account presence tally and everything it needs to act on an edge.
struct Shared {
    /// The tally. A `parking_lot::Mutex` because every critical section here is a handful of
    /// map operations and never spans an `.await` — the async work (store reads, publishes)
    /// always happens with the guard already dropped.
    state: Mutex<HashMap<Id, AccountPresence>>,
    /// Read to learn which rooms an account is in and how large they are.
    store: SharedStore,
    /// The one door back into the domain: `timeout_member` when a grace expires.
    rooms: SharedRooms,
    /// Where room events go.
    publisher: Arc<dyn RoomPublisher>,
}

/// Tracks which accounts are connected and turns the edges into room events.
///
/// Holds an `Arc<Shared>` so a grace timer can be spawned with its own owning reference and
/// outlive the call that armed it.
pub(crate) struct RoomPresence {
    shared: Arc<Shared>,
}

impl RoomPresence {
    /// Wires the component to the store it reads, the rooms service it times members out
    /// through, and the surface it publishes on.
    pub(crate) fn new(
        store: SharedStore,
        rooms: SharedRooms,
        publisher: Arc<dyn RoomPublisher>,
    ) -> Self {
        Self {
            shared: Arc::new(Shared {
                state: Mutex::new(HashMap::new()),
                store,
                rooms,
                publisher,
            }),
        }
    }

    /// A session for `account_id` came up.
    pub(crate) async fn on_session_started(&self, account_id: Id, now: Timestamp) {
        self.shared.on_session_started(account_id, now).await;
    }

    /// A session for `account_id` went down.
    pub(crate) async fn on_session_ended(&self, account_id: Id, now: Timestamp) {
        Arc::clone(&self.shared)
            .on_session_ended(account_id, now)
            .await;
    }

    /// How many of a room's members hold a live session right now.
    pub(crate) async fn online_count(&self, room_id: Id) -> u32 {
        self.shared.online_count(room_id).await
    }
}

impl Shared {
    /// First-socket-up bookkeeping, and the reconnect events that follow.
    async fn on_session_started(&self, account_id: Id, now: Timestamp) {
        let owed = {
            let mut state = self.state.lock();
            let entry = state.entry(account_id).or_default();
            entry.sessions += 1;
            entry.generation += 1;
            if entry.sessions == 1 {
                // 0 → 1: the account is reachable again. Take the set of rooms we told it went
                // offline; each is owed a "back", but only if it is still a room they are in.
                std::mem::take(&mut entry.announced_offline)
            } else {
                // A second or third socket. The account was already online, so no room's tally
                // moved and there is nothing to say — brief section 156.
                return;
            }
        };

        let rooms = match self.store.rooms_for_account(account_id).await {
            Ok(rooms) => rooms,
            // A failed read is not a reason to invent events. The tally is already updated; the
            // next successful edge or listing will carry the truth.
            Err(_) => return,
        };
        for room in &rooms {
            let count = self.online_count(room.room_id).await;
            self.publisher
                .publish_state(room.room_id, &state_delta(room.room_id, count), now);
            if owed.contains(&room.room_id) {
                let event = member_event(
                    room.room_id,
                    account_id,
                    true,
                    view::count(room.member_count),
                    MemberChange::Reconnected,
                );
                self.publisher.publish_member(room.room_id, &event, now);
            }
        }
    }

    /// Last-socket-down bookkeeping, the `Disconnected` events, and the grace timer.
    async fn on_session_ended(self: Arc<Self>, account_id: Id, now: Timestamp) {
        let generation = {
            let mut state = self.state.lock();
            let Some(entry) = state.get_mut(&account_id) else {
                // An end with no matching start. The gateway fires start before end, so this is
                // not expected; if it happens there is nothing to decrement.
                return;
            };
            entry.sessions = entry.sessions.saturating_sub(1);
            entry.generation += 1;
            if entry.sessions > 0 {
                // Still holding another socket: the account is online, no room's tally moved.
                return;
            }
            entry.generation
        };

        let rooms = match self.store.rooms_for_account(account_id).await {
            Ok(rooms) => rooms,
            Err(_) => return,
        };

        // Re-check under the lock after the async read. If a session came up while we read the
        // roster, this disconnect is void: we have announced nothing yet, so abandoning here
        // leaves the two views consistent, with the racing connect owning the state.
        let announce = {
            let mut state = self.state.lock();
            let still_offline = matches!(
                state.get(&account_id),
                Some(entry) if entry.sessions == 0 && entry.generation == generation
            );
            if !still_offline {
                false
            } else if rooms.is_empty() {
                // Offline and in no room: nothing to keep the entry for.
                state.remove(&account_id);
                false
            } else {
                if let Some(entry) = state.get_mut(&account_id) {
                    for room in &rooms {
                        entry.announced_offline.insert(room.room_id);
                    }
                }
                true
            }
        };
        if !announce {
            return;
        }

        for room in &rooms {
            // Tell the room the member went dark, but keep the seat: membership is persistent
            // (section 183) and a disconnect is not a departure until the grace expires.
            let event = member_event(
                room.room_id,
                account_id,
                false,
                view::count(room.member_count),
                MemberChange::Disconnected,
            );
            self.publisher.publish_member(room.room_id, &event, now);
            let count = self.online_count(room.room_id).await;
            self.publisher
                .publish_state(room.room_id, &state_delta(room.room_id, count), now);
        }

        // Arm the grace. `generation` freezes this disconnect: a reconnect bumps it, and the
        // timer that wakes two minutes from now finds the mismatch and does nothing.
        let expire_at = now.saturating_add_millis(RECONNECT_GRACE_MS as i64);
        let this = Arc::clone(&self);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(RECONNECT_GRACE_MS)).await;
            this.expire(account_id, generation, expire_at).await;
        });
    }

    /// The grace timer firing: remove the member from each room they still owe, unless they
    /// came back or the room is theirs.
    async fn expire(&self, account_id: Id, generation: u64, now: Timestamp) {
        // A reconnect since this timer was armed has bumped the generation; if so, this timer
        // speaks for a connection that no longer exists and must do nothing. Deterministic room
        // order so a test reads the same sequence every run.
        let mut pending: Vec<Id> = {
            let state = self.state.lock();
            match state.get(&account_id) {
                Some(entry) if entry.sessions == 0 && entry.generation == generation => {
                    entry.announced_offline.iter().copied().collect()
                }
                _ => return,
            }
        };
        pending.sort_unstable();

        for room_id in pending {
            // Re-check before each removal: a reconnect landing mid-sweep bumps the generation
            // and the next iteration stops, so nobody who just came back is removed between two
            // of these store writes.
            {
                let state = self.state.lock();
                let still = matches!(
                    state.get(&account_id),
                    Some(entry) if entry.sessions == 0 && entry.generation == generation
                );
                if !still {
                    break;
                }
            }
            match self.rooms.timeout_member(room_id, account_id, now).await {
                Ok(Some(fanout)) => {
                    // Removed. Tell the room, and stop tracking that we owed it anything.
                    self.publish_fanout(fanout, now);
                    let mut state = self.state.lock();
                    if let Some(entry) = state.get_mut(&account_id) {
                        entry.announced_offline.remove(&room_id);
                    }
                }
                // Owner-exempt, or already gone. Leave the owner's owed-room entry in place so a
                // later reconnect still tells the room they are back; an already-gone room is
                // filtered out at that reconnect because they are no longer a member of it.
                Ok(None) => {}
                // A failed removal is left for the next edge to reconcile rather than retried in
                // a tight loop against a store that is already unhappy.
                Err(_) => {}
            }
        }

        // If the account is offline and owes no room a follow-up, forget it. Under the same lock
        // as the check so a reconnect racing this cannot have its fresh entry dropped.
        let mut state = self.state.lock();
        let drop_it = matches!(
            state.get(&account_id),
            Some(entry) if entry.sessions == 0 && entry.announced_offline.is_empty()
        );
        if drop_it {
            state.remove(&account_id);
        }
    }

    /// How many of a room's members hold a live session right now.
    ///
    /// Derived on demand — the roster intersected with the session tally — rather than kept as
    /// a running per-room number, because a running number drifts: a join, a kick, or a role
    /// change while online all move it, and those paths are not connection edges this component
    /// observes. Rooms are small and the tally is in memory, so the read is bounded and touches
    /// no presence query (section 14).
    async fn online_count(&self, room_id: Id) -> u32 {
        let mut online: u32 = 0;
        let mut after: Option<Id> = None;
        loop {
            let members = match self.store.room_members(room_id, ROSTER_PAGE, after).await {
                Ok(members) => members,
                // A failed read leaves the count as far as we counted rather than failing a
                // listing over a decoration.
                Err(_) => break,
            };
            if members.is_empty() {
                break;
            }
            {
                let state = self.state.lock();
                for member in &members {
                    if state
                        .get(&member.account_id)
                        .is_some_and(|entry| entry.sessions > 0)
                    {
                        online = online.saturating_add(1);
                    }
                }
            }
            if members.len() < ROSTER_PAGE as usize {
                break;
            }
            after = members.last().map(|member| member.account_id);
        }
        online
    }

    /// Forwards a domain fanout to the publish surface.
    fn publish_fanout(&self, fanout: RoomFanout, now: Timestamp) {
        match &fanout.event {
            RoomBroadcast::Member(event) => {
                self.publisher.publish_member(fanout.room_id, event, now);
            }
            RoomBroadcast::State(event) => {
                self.publisher.publish_state(fanout.room_id, event, now);
            }
            RoomBroadcast::Vote(event) => {
                self.publisher.publish_vote(fanout.room_id, event, now);
            }
        }
    }
}

/// A room membership lifecycle event: this account, this change, the room's total member count.
///
/// `member_count` is the room's total and not the online count: a disconnect and a reconnect do
/// not change who is a member, so the number a `RoomMemberEvent` carries is unchanged across
/// both. The online count travels on its own [`state_delta`].
fn member_event(
    room_id: Id,
    account_id: Id,
    joined: bool,
    member_count: u32,
    change: MemberChange,
) -> RoomMemberEvent {
    RoomMemberEvent {
        room_id,
        user_id: account_id,
        joined,
        role: None,
        member_count: Some(member_count),
        change: Some(change),
    }
}

/// A room state event carrying only a new online count.
///
/// Every other field absent — a delta (section 156): this frame says the online count moved and
/// says nothing else, so it cannot be misread as clearing a topic or resetting a cap.
fn state_delta(room_id: Id, online_count: u32) -> RoomStateEvent {
    RoomStateEvent {
        room_id,
        online_count: Some(online_count),
        member_count: None,
        topic: None,
        slow_mode_ms: None,
        max_members: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use migo_cache::MemoryCache;
    use migo_core::config::Config;
    use migo_core::metrics::Registry;
    use migo_protocol::{RoomJoinRequest, RoomKind};
    use migo_ratelimit::{CacheRateLimiter, Policies, TrustTier};
    use migo_rooms::{Caller as RoomCaller, NewRoomRequest};
    use migo_store::MemoryStore;

    /// Alice owns every fixture room.
    const ALICE: u128 = 1;
    /// Bob is the ordinary member things happen to.
    const BOB: u128 = 2;
    /// A device per account; the value is never inspected, only distinct.
    const DEVICE: u128 = 100;

    /// When a fixture is built.
    const NOW: i64 = 1_700_000_000_000;

    /// An id from a small number, so a failure message names the fixture.
    fn id(value: u128) -> Id {
        Id::from(value)
    }

    /// A timestamp from milliseconds.
    fn ts(millis: i64) -> Timestamp {
        Timestamp::from_millis(millis)
    }

    /// A rooms caller for an account acting from its one device.
    fn caller(account: u128, millis: i64) -> RoomCaller {
        RoomCaller::new(
            id(account),
            id(DEVICE + account),
            TrustTier::Established,
            ts(millis),
        )
    }

    /// Records what a `RoomPublisher` would have sent, for the test to read back.
    #[derive(Default)]
    struct Recorder {
        members: Mutex<Vec<RoomMemberEvent>>,
        states: Mutex<Vec<(Id, RoomStateEvent)>>,
        votes: Mutex<Vec<(Id, RoomVoteEvent)>>,
    }

    impl Recorder {
        /// Every member event with this change, in the order they were published.
        fn members_with(&self, change: MemberChange) -> Vec<RoomMemberEvent> {
            self.members
                .lock()
                .iter()
                .filter(|event| event.change == Some(change))
                .cloned()
                .collect()
        }

        /// The latest online count published for a room, if any.
        fn latest_online(&self, room_id: Id) -> Option<u32> {
            self.states
                .lock()
                .iter()
                .rev()
                .find(|(id, _)| *id == room_id)
                .and_then(|(_, event)| event.online_count)
        }
    }

    impl RoomPublisher for Recorder {
        fn publish_member(&self, _room_id: Id, event: &RoomMemberEvent, _now: Timestamp) {
            self.members.lock().push(event.clone());
        }

        fn publish_state(&self, room_id: Id, event: &RoomStateEvent, _now: Timestamp) {
            self.states.lock().push((room_id, event.clone()));
        }

        fn publish_vote(&self, room_id: Id, event: &RoomVoteEvent, _now: Timestamp) {
            self.votes.lock().push((room_id, event.clone()));
        }
    }

    /// Everything a test needs: a real rooms service over an in-memory store, plus the
    /// component under test wired to a recorder.
    struct Fixture {
        presence: RoomPresence,
        rooms: SharedRooms,
        store: SharedStore,
        recorder: Arc<Recorder>,
    }

    impl Fixture {
        fn new() -> Self {
            let settings = Config::default();
            let store: SharedStore = Arc::new(MemoryStore::new());
            let cache = Arc::new(MemoryCache::new());
            let registry = Registry::new();
            let policies = Policies::from_config(&settings.rate_limit)
                .expect("the default policies are valid");
            let limiter = Arc::new(CacheRateLimiter::new(cache, policies, &registry));
            let rooms = migo_rooms::open(
                Arc::clone(&store),
                limiter,
                &registry,
                migo_rooms::RoomsConfig::default(),
            );
            let recorder = Arc::new(Recorder::default());
            let presence = RoomPresence::new(
                Arc::clone(&store),
                Arc::clone(&rooms),
                Arc::clone(&recorder) as Arc<dyn RoomPublisher>,
            );
            Self {
                presence,
                rooms,
                store,
                recorder,
            }
        }

        /// Alice's public room with Bob as an ordinary member. Returns its id.
        async fn room_with_bob(&self) -> Id {
            let room = self
                .rooms
                .create(
                    &caller(ALICE, NOW),
                    NewRoomRequest {
                        slug: "lobby".to_string(),
                        name: "Lobby".to_string(),
                        topic: None,
                        kind: RoomKind::Public,
                        max_members: None,
                    },
                )
                .await
                .expect("a well-formed room is created")
                .room_id;
            self.rooms
                .join(
                    &caller(BOB, NOW + 1),
                    RoomJoinRequest {
                        room_id: room,
                        invite_code: None,
                    },
                )
                .await
                .expect("an open room admits a member");
            room
        }

        /// Whether an account is still an active member of a room.
        async fn is_member(&self, room: Id, account: u128) -> bool {
            self.store
                .room_member(room, id(account))
                .await
                .expect("the store answers")
                .is_some_and(|member| member.is_active())
        }
    }

    /// Lets a spawned grace timer run to completion under paused time. The main task is always
    /// runnable while yielding, so the runtime never auto-advances during this — time only moves
    /// where a test says `advance`.
    async fn settle() {
        for _ in 0..16 {
            tokio::task::yield_now().await;
        }
    }

    #[tokio::test]
    async fn a_last_session_drop_tells_each_room_disconnected_and_keeps_the_seat() {
        let fixture = Fixture::new();
        let room = fixture.room_with_bob().await;

        fixture
            .presence
            .on_session_started(id(BOB), ts(NOW + 10))
            .await;
        fixture
            .presence
            .on_session_ended(id(BOB), ts(NOW + 20))
            .await;

        let disconnects = fixture.recorder.members_with(MemberChange::Disconnected);
        assert_eq!(
            disconnects.len(),
            1,
            "the one room Bob is in hears one disconnect"
        );
        assert_eq!(disconnects[0].user_id, id(BOB));
        assert!(!disconnects[0].joined, "a disconnect is not a join");
        assert!(
            fixture.is_member(room, BOB).await,
            "a disconnect keeps the seat; only the grace expiry removes it"
        );
    }

    #[tokio::test]
    async fn a_reconnect_within_grace_restores_the_member_and_leaves_the_seat() {
        let fixture = Fixture::new();
        let room = fixture.room_with_bob().await;

        fixture
            .presence
            .on_session_started(id(BOB), ts(NOW + 10))
            .await;
        fixture
            .presence
            .on_session_ended(id(BOB), ts(NOW + 20))
            .await;
        // Back before the grace elapses.
        fixture
            .presence
            .on_session_started(id(BOB), ts(NOW + 30))
            .await;

        let reconnects = fixture.recorder.members_with(MemberChange::Reconnected);
        assert_eq!(
            reconnects.len(),
            1,
            "the room Bob left and returned to hears one reconnect"
        );
        assert_eq!(reconnects[0].user_id, id(BOB));
        assert!(reconnects[0].joined, "a reconnect reads as present");
        assert!(
            fixture.is_member(room, BOB).await,
            "the seat was never given up"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_grace_expiry_removes_the_member_and_drops_the_count() {
        let fixture = Fixture::new();
        let room = fixture.room_with_bob().await;

        fixture
            .presence
            .on_session_started(id(BOB), ts(NOW + 10))
            .await;
        fixture
            .presence
            .on_session_ended(id(BOB), ts(NOW + 20))
            .await;
        settle().await; // let the timer register its sleep
        tokio::time::advance(Duration::from_millis(RECONNECT_GRACE_MS + 1)).await;
        settle().await; // let it fire and remove

        let lefts = fixture.recorder.members_with(MemberChange::Left);
        assert_eq!(
            lefts.len(),
            1,
            "the expiry makes the departure real with a Left"
        );
        assert_eq!(lefts[0].user_id, id(BOB));
        assert!(
            !fixture.is_member(room, BOB).await,
            "the grace expired with Bob still offline, so the seat is emptied"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn the_owner_is_never_removed_by_a_grace_expiry() {
        let fixture = Fixture::new();
        let room = fixture.room_with_bob().await;

        // Alice, the owner, drops her last session and never comes back.
        fixture
            .presence
            .on_session_started(id(ALICE), ts(NOW + 10))
            .await;
        fixture
            .presence
            .on_session_ended(id(ALICE), ts(NOW + 20))
            .await;
        settle().await;
        tokio::time::advance(Duration::from_millis(RECONNECT_GRACE_MS + 1)).await;
        settle().await;

        assert!(
            fixture
                .recorder
                .members_with(MemberChange::Disconnected)
                .iter()
                .any(|e| e.user_id == id(ALICE)),
            "the owner is still shown as offline"
        );
        assert!(
            fixture.recorder.members_with(MemberChange::Left).is_empty(),
            "but the owner keeps the room across a timeout"
        );
        assert!(
            fixture.is_member(room, ALICE).await,
            "the owner stays seated: a room cannot be left without an owner"
        );
    }

    #[tokio::test]
    async fn only_the_last_socket_moves_a_room() {
        let fixture = Fixture::new();
        fixture.room_with_bob().await;

        // Two sockets up, then only one down: the account is still online.
        fixture
            .presence
            .on_session_started(id(BOB), ts(NOW + 10))
            .await;
        fixture
            .presence
            .on_session_started(id(BOB), ts(NOW + 11))
            .await;
        fixture
            .presence
            .on_session_ended(id(BOB), ts(NOW + 20))
            .await;

        assert!(
            fixture
                .recorder
                .members_with(MemberChange::Disconnected)
                .is_empty(),
            "a room hears nothing until the last socket drops"
        );
    }

    #[tokio::test]
    async fn online_count_is_the_roster_intersected_with_live_sessions() {
        let fixture = Fixture::new();
        let room = fixture.room_with_bob().await;

        assert_eq!(
            fixture.presence.online_count(room).await,
            0,
            "nobody connected yet"
        );

        fixture
            .presence
            .on_session_started(id(ALICE), ts(NOW + 10))
            .await;
        assert_eq!(
            fixture.presence.online_count(room).await,
            1,
            "the owner is online"
        );
        assert_eq!(
            fixture.recorder.latest_online(room),
            Some(1),
            "the room was told its count moved to one"
        );

        fixture
            .presence
            .on_session_started(id(BOB), ts(NOW + 11))
            .await;
        assert_eq!(
            fixture.presence.online_count(room).await,
            2,
            "both members online"
        );
        assert_eq!(fixture.recorder.latest_online(room), Some(2));
    }
}
