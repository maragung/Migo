//! The account-to-node routing map section 170's honest tail admitted was
//! missing, built the way the two tiers before it were: a question and an
//! answer over the mesh.
//!
//! An account row carries no home label a directory could read — unlike a room
//! or a conversation, whose row names the node that owns its fan-out — so the
//! routing question "which node holds this account's rows?" can only be asked
//! as a broadcast (`FED_ACCOUNT_QUERY`), and answered by the one node that
//! holds the rows (`FED_ACCOUNT_ROWS`). A peer that holds nothing stays
//! silent, which is the honest half of the design: silence is not an error,
//! it is one more way of saying "not mine", and the asker's bounded wait turns
//! it back into the same fail-closed refusal the gate already gives.
//!
//! The pull is on demand and only from a gate that fail-closed on a missing
//! row. Nothing here replicates eagerly — a node does not ship its accounts to
//! peers just because they are linked, because that would make the mesh a
//! copy of every node's whole user table and the pull-on-demand shape is what
//! keeps both of section 170's topologies honest. The trigger is the privacy
//! gate reading no local profile for the recipient (`StoreMessageGate`) and
//! the membership read of a subscribed conversation coming back empty
//! (`authorize_topic`), which are exactly the two places the stand-ins used to
//! hand-copy rows into: the account, profile, and the social edges between the
//! two accounts cross with the answer, as does a conversation row with its
//! members. The answer carries the rows verbatim, nothing derived — the
//! passphrase hash included, because the mesh is an authenticated link between
//! allow-listed fleet peers, the same trust boundary that already carries
//! sealed envelopes, and a replica account without its hash would not be the
//! row it claims to be.
//!
//! What a node accepts is narrower than what it asks for: rows are applied
//! only while an ask this process made is still waiting, and only when the row
//! is not already held — a peer cannot overwrite local truth, only feed a gate
//! that had nothing to read. And the replica is a snapshot of pull time, not a
//! subscription: a setting the owner changes on the home node after the pull
//! does not propagate until the next ask, which is the staleness this tier
//! owes its readers and no more.
//!
//! # The third tier: call rows
//!
//! A 1:1 call row breaks the assumption the two tiers above are built on. An
//! account or a conversation has one node that writes it; a call has two, one
//! for each party's device, and which node that is falls out of where the two
//! people happened to connect rather than out of anything the row says. So the
//! call tier is the only one that is a pull *and* a push:
//!
//! - The pull is the same question and answer (`FED_CALL_QUERY` and
//!   `FED_CALL_ROWS`, opcodes 248 and 249): the callee's node, handed
//!   `CALL_ANSWER` for a row the inviting node minted, asks the fleet and
//!   adopts the answer. That is the half section 165 recorded as owed — the
//!   completion path of a call whose two devices sit on two nodes.
//! - The push is the half a read-only replica would not survive. Once the
//!   callee's node has answered, it holds `callee_device` and `Connecting`,
//!   and the caller's node — which minted the row and is the node that will
//!   route the caller's SDP — holds neither. So every node that changes a call
//!   row mirrors the new row to every allowed peer, and a peer applies it only
//!   when it already holds that row.
//!
//! What a node accepts therefore reads as one sentence: **a peer may fill a
//! row this node asked for, and may update a row this node already holds, but
//! may never plant a row this node never had and never asked about.** The
//! update is ordered by the state machine itself — the wire numbering is
//! already the order the states advance in, so a row only ever moves forward —
//! which is what keeps a `Connecting` mirror from landing on top of an `Ended`
//! one and resurrecting a call both parties already watched die.
//!
//! The tier is inert on a node whose relay was built without a call store
//! (see [`ReplicationRelay::with_calls`]): no store, no rows to ask for, no
//! question worth putting on the wire. That is the honest state of a process
//! that does not serve calls at all, and it is what the tests that build a
//! relay without one exercise.

use std::collections::HashSet;
use std::time::Duration;

use migo_calls::{Call, CallState, EndReason, SharedCallStore};
use migo_core::{Id, Result, Timestamp};
use migo_federation::model::FederatedEvent;
use migo_federation::SharedMesh;
use migo_protocol::{
    FedAccountEdge, FedAccountQuery, FedAccountRows, FedCallQuery, FedCallRows,
    FedConversationQuery, FedConversationRows, Opcode, RelationshipKind,
};
use migo_store::model::{AccountStatus, Conversation, Gender, NewAccount, Profile, Relationship};
use migo_store::SharedStore;

use crate::room_relay::encode_envelope;

/// How many allow-list rows one ask reads. The same page clamp the two tiers
/// before this one scan with, so all three cost one page of the allow-list
/// each and a fleet larger than the page is paged the same way by every tier.
const PEER_SCAN_LIMIT: u16 = 256;

/// How many times the asker re-checks its own store while waiting for an
/// answer. The wait is bounded because the ask must never wedge a client
/// request: an owner that is partitioned away or refuses costs exactly
/// `ASK_ATTEMPTS × ASK_WAIT` and then the gate answers the fail-closed `no` it
/// would have given with no mesh at all.
const ASK_ATTEMPTS: u32 = 3;

/// How long one re-check's wait lasts. The outbox runner drains every half
/// second, so one tick to carry the question and one to carry the answer fit
/// inside the first wait with room for a dial; the rest is margin for a link
/// still handshaking.
const ASK_WAIT: Duration = Duration::from_secs(1);

/// The edge kinds one answer may carry, every kind the privacy gate and the
/// block check read. `Unknown` is not asked for: it is the decode of a newer
/// peer's kind this build does not know, and a row this build cannot name is
/// not a row this build should write.
const EDGE_KINDS: [RelationshipKind; 7] = [
    RelationshipKind::Friend,
    RelationshipKind::PendingOutgoing,
    RelationshipKind::PendingIncoming,
    RelationshipKind::Follow,
    RelationshipKind::Block,
    RelationshipKind::Favorite,
    RelationshipKind::Mute,
];

/// The row-replication tier's relay: the ask half a fail-closed gate drives,
/// the answer half the mesh's ingest path drives, and the asked-set that keeps
/// the two honest about what may be applied.
///
/// A sibling of the conversation relay rather than a field of it, for the same
/// reason the conversation relay is a sibling of the room relay: the tiers
/// answer different questions of the store (who owns a fan-out, and who owns a
/// row) and share nothing but the mesh and the shape.
pub struct ReplicationRelay {
    mesh: SharedMesh,
    store: SharedStore,
    /// The accounts this process has an ask in flight for. Keyed by the
    /// account alone — the answer does not echo which `regarding` it answers,
    /// so the guard cannot be narrower than the ask without refusing answers
    /// two concurrent asks raced to produce. An entry is removed when the rows
    /// are applied or when the wait gives up, so a late answer to a spent ask
    /// is refused rather than written.
    asked_accounts: parking_lot::Mutex<HashSet<Id>>,
    /// The conversations this process has an ask in flight for, with the same
    /// lifecycle as the accounts above.
    asked_conversations: parking_lot::Mutex<HashSet<Id>>,
    /// The call store the third tier reads, writes, and mirrors. Absent on a
    /// relay built without one, which makes the whole call tier inert rather
    /// than wrong: this is the `Option` a process that serves no calls is
    /// honestly described by, and the two tiers above it are untouched by it.
    calls: Option<SharedCallStore>,
    /// The calls this process has an ask in flight for, with the same
    /// lifecycle as the accounts and conversations above.
    asked_calls: parking_lot::Mutex<HashSet<Id>>,
}

impl ReplicationRelay {
    /// Wraps the mesh and the store both halves read.
    #[must_use]
    pub fn new(mesh: SharedMesh, store: SharedStore) -> Self {
        Self {
            mesh,
            store,
            asked_accounts: parking_lot::Mutex::new(HashSet::new()),
            asked_conversations: parking_lot::Mutex::new(HashSet::new()),
            calls: None,
            asked_calls: parking_lot::Mutex::new(HashSet::new()),
        }
    }

    /// Binds the call store the third tier replicates.
    ///
    /// A builder rather than a fourth argument to [`Self::new`], because the
    /// call store is not part of what a relay *is* — the two row tiers above
    /// work on a node with no call service at all, and every test that builds
    /// a relay for them keeps working unchanged. The composition root binds
    /// the same `Arc` it hands `migo_calls::open`, so the service's writes and
    /// this tier's mirrors read one store, not two.
    #[must_use]
    pub fn with_calls(mut self, calls: SharedCallStore) -> Self {
        self.calls = Some(calls);
        self
    }

    /// Whether an account's rows are already held: the profile is the read the
    /// privacy gate fail-closed on, so the profile is the row the wait checks
    /// for — an account without its profile is a half-state no path here
    /// writes, and re-asking until the pair lands is the honest recovery.
    async fn holds_account(&self, account_id: Id) -> bool {
        self.store
            .profile(account_id)
            .await
            .map(|profile| profile.is_some())
            .unwrap_or(false)
    }

    /// Whether a conversation's row is already held.
    async fn holds_conversation(&self, conversation_id: Id) -> bool {
        self.store
            .conversation(conversation_id)
            .await
            .map(|row| row.is_some())
            .unwrap_or(false)
    }

    /// Pulls an account's rows from the owning node, then reports whether this
    /// node now holds them.
    ///
    /// The gate's fail-closed contract is untouched: an owner that is
    /// unreachable, paused, or silent costs the bounded wait and the answer is
    /// `false`, which is the same refusal the gate gives a stranger. Only the
    /// rows change hands — the gate still decides, now with real rows to read.
    pub(crate) async fn ensure_account(
        &self,
        account_id: Id,
        regarding: Id,
        now: Timestamp,
    ) -> bool {
        if self.holds_account(account_id).await {
            return true;
        }
        if self.ask_account(account_id, regarding, now).await == 0 {
            // No allowed peer to ask: the mesh is not wired to anyone who
            // could hold the rows, and waiting cannot change that.
            return false;
        }
        for _ in 0..ASK_ATTEMPTS {
            tokio::time::sleep(ASK_WAIT).await;
            if self.holds_account(account_id).await {
                return true;
            }
        }
        // The wait is spent. The asked entry leaves with it, so an answer that
        // arrives after the gate has already refused is refused in turn —
        // rows are applied only while an ask is still waiting for them.
        self.asked_accounts.lock().remove(&account_id);
        false
    }

    /// Registers the ask and broadcasts the routing question to every allowed
    /// peer, returning how many peers were asked.
    ///
    /// A broadcast rather than a directed ask, because the routing question is
    /// the whole point: an account row carries no home label to read, so every
    /// peer is asked and the one that holds the rows answers. Errors are
    /// folded into the count — a peer whose enqueue refuses is a peer not
    /// asked, and the wait's verdict does not change.
    pub(crate) async fn ask_account(&self, account_id: Id, regarding: Id, now: Timestamp) -> usize {
        self.asked_accounts.lock().insert(account_id);
        let peers = self.allowed_peers().await;
        if peers.is_empty() {
            return 0;
        }
        let query = FedAccountQuery {
            epoch: self.mesh.epoch(),
            account_id,
            regarding,
        };
        let payload = match encode_envelope(Opcode::FedAccountQuery, &query) {
            Ok(payload) => payload,
            Err(error) => {
                tracing::warn!(%error, "cannot encode an account query the gate asked for");
                return 0;
            }
        };
        let mut asked = 0;
        for peer in peers {
            let event = FederatedEvent {
                target_node: peer,
                opcode: Opcode::FedAccountQuery.to_wire() as i32,
                payload: payload.clone(),
            };
            match self.mesh.enqueue(event, now).await {
                Ok(_) => asked += 1,
                Err(error) => tracing::warn!(
                    %error,
                    peer = %peer.to_text(),
                    "cannot enqueue an account query for this peer"
                ),
            }
        }
        asked
    }

    /// Pulls a conversation's rows from the home node, then reports whether
    /// this node now holds the row. The same shape as [`Self::ensure_account`]
    /// with one difference the row pays for: a conversation *does* carry a
    /// home label, but the label is on the row being asked for, so the ask is
    /// still a broadcast — the membership read that triggered it came back
    /// empty precisely because the row is not here to read.
    pub(crate) async fn ensure_conversation(&self, conversation_id: Id, now: Timestamp) -> bool {
        if self.holds_conversation(conversation_id).await {
            return true;
        }
        if self.ask_conversation(conversation_id, now).await == 0 {
            return false;
        }
        for _ in 0..ASK_ATTEMPTS {
            tokio::time::sleep(ASK_WAIT).await;
            if self.holds_conversation(conversation_id).await {
                return true;
            }
        }
        self.asked_conversations.lock().remove(&conversation_id);
        false
    }

    /// Registers the ask and broadcasts the conversation question, returning
    /// how many peers were asked. The sibling of [`Self::ask_account`].
    pub(crate) async fn ask_conversation(&self, conversation_id: Id, now: Timestamp) -> usize {
        self.asked_conversations.lock().insert(conversation_id);
        let peers = self.allowed_peers().await;
        if peers.is_empty() {
            return 0;
        }
        let query = FedConversationQuery {
            epoch: self.mesh.epoch(),
            conversation_id,
        };
        let payload = match encode_envelope(Opcode::FedConversationQuery, &query) {
            Ok(payload) => payload,
            Err(error) => {
                tracing::warn!(%error, "cannot encode a conversation query the gate asked for");
                return 0;
            }
        };
        let mut asked = 0;
        for peer in peers {
            let event = FederatedEvent {
                target_node: peer,
                opcode: Opcode::FedConversationQuery.to_wire() as i32,
                payload: payload.clone(),
            };
            match self.mesh.enqueue(event, now).await {
                Ok(_) => asked += 1,
                Err(error) => tracing::warn!(
                    %error,
                    peer = %peer.to_text(),
                    "cannot enqueue a conversation query for this peer"
                ),
            }
        }
        asked
    }

    /// The answer half for accounts: a peer asked, and this node holds what it
    /// asked about.
    ///
    /// Silence is the honest answer for everything this node does not hold
    /// whole — no account row, no profile, or a deleted account whose rows a
    /// replica has no business vouching for — because the asker's bounded wait
    /// fails closed on silence exactly as it would on a refusal. The edges
    /// that cross are only the ones between the queried account and the
    /// asker's `regarding` account, never the queried account's whole graph:
    /// the answer may carry what the asker's gate reads, and no more. A stale
    /// epoch is refused the way the two tiers before this one refuse it, so
    /// the peer re-handshakes onto the current view before it may be answered.
    pub(crate) async fn answer_account(
        &self,
        peer: Id,
        query: FedAccountQuery,
        now: Timestamp,
    ) -> Result<()> {
        self.mesh.check_epoch(query.epoch)?;
        let account = match self.store.account_by_id(query.account_id).await? {
            Some(account) if account.status != AccountStatus::Deleted => account,
            _ => {
                tracing::debug!(
                    account = %query.account_id.to_text(),
                    "asked about an account this node does not hold whole; staying silent"
                );
                return Ok(());
            }
        };
        let Some(profile) = self.store.profile(query.account_id).await? else {
            tracing::debug!(
                account = %query.account_id.to_text(),
                "asked about an account whose profile this node does not hold; staying silent"
            );
            return Ok(());
        };
        let mut edges = Vec::new();
        if query.account_id != query.regarding {
            for kind in EDGE_KINDS {
                if let Some(edge) = self
                    .store
                    .relationship(query.account_id, query.regarding, kind)
                    .await?
                {
                    edges.push(FedAccountEdge {
                        other_id: edge.other_id,
                        kind: edge.kind,
                        created_at: edge.created_at,
                        accepted_at: edge.accepted_at,
                    });
                }
            }
        }
        // AUDIT (secret exposure): the passphrase hash crosses the mesh here,
        // read through `expose()` for the one trip a replica account needs it
        // for. The link is authenticated and allow-listed — the same boundary
        // sealed envelopes already cross — and the hash is written straight
        // into the replica's account row, never logged, never re-read here.
        let rows = FedAccountRows {
            account_id: account.account_id,
            username: account.username,
            passphrase_hash: account.passphrase_hash.expose().to_string(),
            locale: account.locale,
            created_at: account.created_at,
            display_name: profile.display_name,
            show_last_seen: visibility_wire(profile.show_last_seen),
            who_can_message: visibility_wire(profile.who_can_message),
            who_can_add: visibility_wire(profile.who_can_add),
            who_can_call_voice: Some(visibility_wire(profile.who_can_call_voice)),
            who_can_call_video: Some(visibility_wire(profile.who_can_call_video)),
            searchable: profile.searchable,
            profile_updated_at: profile.updated_at,
            edges,
            email: account.email,
            phone: account.phone,
            country: account.country,
            bio: profile.bio,
            avatar_media_id: profile.avatar_media_id,
            birth_year: profile.birth_year.map(|year| u32::from(year.max(0) as u16)),
            gender: profile
                .gender
                .map(|gender| u32::from(gender.to_i16() as u16)),
            custom_status: profile.custom_status,
        };
        self.mesh
            .enqueue(
                FederatedEvent {
                    target_node: peer,
                    opcode: Opcode::FedAccountRows.to_wire() as i32,
                    payload: encode_envelope(Opcode::FedAccountRows, &rows)?,
                },
                now,
            )
            .await
            .map(|_queued| ())
    }

    /// The answer half for conversations: the row verbatim — `home_region`
    /// included, the one fact every node holding a copy must read the same
    /// answer from — and the member ids of everyone still seated. Membership
    /// crosses as the id set because the replica answers existence and
    /// authorization, not per-member preferences, which stay facts of the node
    /// whose session set them.
    pub(crate) async fn answer_conversation(
        &self,
        peer: Id,
        query: FedConversationQuery,
        now: Timestamp,
    ) -> Result<()> {
        self.mesh.check_epoch(query.epoch)?;
        let Some(conversation) = self.store.conversation(query.conversation_id).await? else {
            tracing::debug!(
                conversation = %query.conversation_id.to_text(),
                "asked about a conversation this node does not hold; staying silent"
            );
            return Ok(());
        };
        let members = self
            .store
            .members(query.conversation_id)
            .await?
            .into_iter()
            .filter(|member| member.left_at.is_none())
            .map(|member| member.account_id)
            .collect::<Vec<_>>();
        let rows = FedConversationRows {
            conversation_id: conversation.conversation_id,
            kind: conversation.kind,
            encryption: conversation.encryption,
            home_region: conversation.home_region,
            created_by: conversation.created_by,
            created_at: conversation.created_at,
            last_seq: u64::try_from(conversation.last_seq.max(0)).unwrap_or(0),
            members,
            room_id: conversation.room_id,
            title: conversation.title,
            last_message_at: conversation.last_message_at,
            archived_at: conversation.archived_at,
        };
        self.mesh
            .enqueue(
                FederatedEvent {
                    target_node: peer,
                    opcode: Opcode::FedConversationRows.to_wire() as i32,
                    payload: encode_envelope(Opcode::FedConversationRows, &rows)?,
                },
                now,
            )
            .await
            .map(|_queued| ())
    }

    /// Applies an account answer, reporting whether rows were written.
    ///
    /// Three refusals guard the write, in the order the design owes them:
    /// unsolicited rows are refused (the asked-set is the asker's side of the
    /// handshake, and a peer may not push), a row already held is left alone
    /// (a peer cannot overwrite local truth), and each edge is written on its
    /// own tolerance because the store's foreign key on `other_id` is a fact
    /// the home node's answer cannot vouch for — the asker may not hold the
    /// regarding account yet when a racing answer arrives. The asked entry
    /// leaves with a successful apply, so a second answer to the same ask is
    /// refused rather than written twice; a failed apply leaves it in place
    /// for the wait to spend, which is the same lifecycle the ask owns.
    pub(crate) async fn apply_account_rows(&self, rows: FedAccountRows) -> Result<bool> {
        if !self.asked_accounts.lock().contains(&rows.account_id) {
            tracing::warn!(
                account = %rows.account_id.to_text(),
                "refusing account rows no ask of this process is waiting for"
            );
            return Ok(false);
        }
        let mut written = false;
        if self.store.account_by_id(rows.account_id).await?.is_none() {
            let account = NewAccount {
                account_id: rows.account_id,
                username: rows.username,
                email: rows.email,
                phone: rows.phone,
                // AUDIT (secret exposure): the replica's account row takes the
                // hash the owner's node sent, re-wrapped in the store's own
                // secret type on arrival and never read back here.
                passphrase_hash: migo_core::Secret::new(rows.passphrase_hash),
                locale: rows.locale,
                country: rows.country,
                created_at: rows.created_at,
            };
            if let Err(error) = self.store.create_account(account).await {
                tracing::warn!(
                    %error,
                    account = %rows.account_id.to_text(),
                    "cannot seat the replicated account row"
                );
                return Ok(false);
            }
            written = true;
        }
        if self.store.profile(rows.account_id).await?.is_none() {
            let profile = Profile {
                account_id: rows.account_id,
                display_name: rows.display_name,
                bio: rows.bio,
                avatar_media_id: rows.avatar_media_id,
                birth_year: rows
                    .birth_year
                    .map(|year| i16::try_from(year.min(i16::MAX as u32)).unwrap_or(i16::MAX)),
                gender: rows
                    .gender
                    .and_then(|gender| Gender::from_i16(i16::try_from(gender).unwrap_or(0))),
                show_last_seen: visibility_of(rows.show_last_seen),
                who_can_message: visibility_of(rows.who_can_message),
                who_can_add: visibility_of(rows.who_can_add),
                // Absent means the peer runs a build from before these columns existed,
                // and the honest reading of that is the column default — Friends, which
                // is what section 180 says a call policy starts as — rather than the
                // permissive Everyone a bare unwrap_or_default would choose.
                who_can_call_voice: rows
                    .who_can_call_voice
                    .map(visibility_of)
                    .unwrap_or(Visibility::Friends),
                who_can_call_video: rows
                    .who_can_call_video
                    .map(visibility_of)
                    .unwrap_or(Visibility::Friends),
                searchable: rows.searchable,
                custom_status: rows.custom_status,
                updated_at: rows.profile_updated_at,
            };
            if let Err(error) = self.store.create_profile(profile).await {
                tracing::warn!(
                    %error,
                    account = %rows.account_id.to_text(),
                    "cannot seat the replicated profile row"
                );
                return Ok(written);
            }
            written = true;
        }
        for edge in rows.edges {
            if edge.other_id == rows.account_id || edge.kind == RelationshipKind::Unknown {
                // A self-edge the store's own check would refuse, or a kind
                // this build cannot name: both are writes no honest path
                // produced, and skipping one edge costs less than failing the
                // answer that carried it.
                continue;
            }
            let relationship = Relationship {
                account_id: rows.account_id,
                other_id: edge.other_id,
                kind: edge.kind,
                created_at: edge.created_at,
                accepted_at: edge.accepted_at,
            };
            if let Err(error) = self.store.put_relationship(relationship).await {
                tracing::warn!(
                    %error,
                    account = %rows.account_id.to_text(),
                    "cannot seat one replicated edge; the rest of the rows stand"
                );
            }
        }
        self.asked_accounts.lock().remove(&rows.account_id);
        Ok(written)
    }

    /// Applies a conversation answer, reporting whether the row was written.
    /// The same refusals as [`Self::apply_account_rows`]: unsolicited rows are
    /// refused, a row already held is left alone, and the asked entry leaves
    /// with the apply.
    pub(crate) async fn apply_conversation_rows(&self, rows: FedConversationRows) -> Result<bool> {
        if !self
            .asked_conversations
            .lock()
            .contains(&rows.conversation_id)
        {
            tracing::warn!(
                conversation = %rows.conversation_id.to_text(),
                "refusing conversation rows no ask of this process is waiting for"
            );
            return Ok(false);
        }
        if self.holds_conversation(rows.conversation_id).await {
            self.asked_conversations
                .lock()
                .remove(&rows.conversation_id);
            return Ok(false);
        }
        let conversation = Conversation {
            conversation_id: rows.conversation_id,
            kind: rows.kind,
            encryption: rows.encryption,
            room_id: rows.room_id,
            home_region: rows.home_region,
            title: rows.title,
            last_seq: i64::try_from(rows.last_seq.min(i64::MAX as u64)).unwrap_or(i64::MAX),
            created_by: rows.created_by,
            created_at: rows.created_at,
            last_message_at: rows.last_message_at,
            archived_at: rows.archived_at,
        };
        if let Err(error) = self
            .store
            .create_conversation(conversation, rows.members)
            .await
        {
            tracing::warn!(
                %error,
                conversation = %rows.conversation_id.to_text(),
                "cannot seat the replicated conversation row"
            );
            return Ok(false);
        }
        self.asked_conversations
            .lock()
            .remove(&rows.conversation_id);
        Ok(true)
    }

    /// Whether this node holds the call row.
    ///
    /// False on a relay with no call store, which is the same answer as "the
    /// row is not here" and is all [`Self::ensure_call`] needs to stay inert.
    async fn holds_call(&self, call_id: Id) -> bool {
        let Some(calls) = &self.calls else {
            return false;
        };
        calls
            .get(call_id)
            .await
            .map(|row| row.is_some())
            .unwrap_or(false)
    }

    /// Pulls a call row from the node that minted it, then reports whether this
    /// node now holds it.
    ///
    /// The same bounded wait as the two tiers above, for the same reason: the
    /// ask must never wedge a client request, so a fleet that holds nothing
    /// costs `ASK_ATTEMPTS × ASK_WAIT` and the answer is `false`, which leaves
    /// the service to give the `NOT_FOUND` it would have given with no mesh at
    /// all.
    ///
    /// The call sites are the 1:1 lifecycle frames and only those — the
    /// frames that can only be about a call that already exists. A *new* call
    /// is minted here, so asking about it would be asking the fleet about a row
    /// nobody has yet: every invite would pay the whole wait for an answer that
    /// is silence by construction.
    pub(crate) async fn ensure_call(&self, call_id: Id, now: Timestamp) -> bool {
        if self.calls.is_none() {
            return false;
        }
        if self.holds_call(call_id).await {
            return true;
        }
        if self.ask_call(call_id, now).await == 0 {
            return false;
        }
        for _ in 0..ASK_ATTEMPTS {
            tokio::time::sleep(ASK_WAIT).await;
            if self.holds_call(call_id).await {
                return true;
            }
        }
        self.asked_calls.lock().remove(&call_id);
        false
    }

    /// Registers the ask and broadcasts the call question, returning how many
    /// peers were asked. The sibling of [`Self::ask_account`], and a broadcast
    /// for the same reason: a call row names the conversation it belongs to
    /// and nothing about which node holds it.
    pub(crate) async fn ask_call(&self, call_id: Id, now: Timestamp) -> usize {
        self.asked_calls.lock().insert(call_id);
        let peers = self.allowed_peers().await;
        if peers.is_empty() {
            return 0;
        }
        let query = FedCallQuery {
            epoch: self.mesh.epoch(),
            call_id,
        };
        let payload = match encode_envelope(Opcode::FedCallQuery, &query) {
            Ok(payload) => payload,
            Err(error) => {
                tracing::warn!(%error, "cannot encode a call query a lifecycle frame asked for");
                return 0;
            }
        };
        let mut asked = 0;
        for peer in peers {
            let event = FederatedEvent {
                target_node: peer,
                opcode: Opcode::FedCallQuery.to_wire() as i32,
                payload: payload.clone(),
            };
            match self.mesh.enqueue(event, now).await {
                Ok(_) => asked += 1,
                Err(error) => tracing::warn!(
                    %error,
                    peer = %peer.to_text(),
                    "cannot enqueue a call query for this peer"
                ),
            }
        }
        asked
    }

    /// The answer half for calls: a peer asked, and this node holds the row.
    ///
    /// The row crosses verbatim — both devices included, because a node that
    /// pulled it is a node that will route for one of them, and a replica
    /// missing `callee_device` is a replica that cannot route the caller's
    /// SDP. Silence is the answer for an id this node does not hold, exactly
    /// as it is for the two tiers above: the asker's bounded wait fails closed
    /// on silence, and a peer that guessed "not mine" out loud would be
    /// inventing an answer about somebody else's call.
    pub(crate) async fn answer_call(
        &self,
        peer: Id,
        query: FedCallQuery,
        now: Timestamp,
    ) -> Result<()> {
        self.mesh.check_epoch(query.epoch)?;
        let Some(calls) = &self.calls else {
            return Ok(());
        };
        let Some(call) = calls.get(query.call_id).await? else {
            tracing::debug!(
                call = %query.call_id.to_text(),
                "asked about a call this node does not hold; staying silent"
            );
            return Ok(());
        };
        self.mesh
            .enqueue(
                FederatedEvent {
                    target_node: peer,
                    opcode: Opcode::FedCallRows.to_wire() as i32,
                    payload: encode_envelope(Opcode::FedCallRows, &call_wire(&call))?,
                },
                now,
            )
            .await
            .map(|_queued| ())
    }

    /// Mirrors a call row this node just changed to every allowed peer.
    ///
    /// The push half, and deliberately infallible: it is called from the
    /// dispatcher's 1:1 lifecycle handlers *after* the client's own frame has
    /// already succeeded, so an encode failure or a peer that refuses the
    /// enqueue must not turn a call that answered into a call that errored.
    /// Everything here is logged and dropped, and the tier's guarantee is the
    /// one it can keep: the row is mirrored when the mesh carries it.
    ///
    /// Peers that do not hold the row ignore it — see
    /// [`Self::apply_call_rows`] — so the fan-out costs one small frame per
    /// write per peer, which for a call is four writes over its whole life.
    pub(crate) async fn publish_call(&self, call_id: Id, now: Timestamp) {
        let Some(calls) = &self.calls else {
            return;
        };
        let call = match calls.get(call_id).await {
            Ok(Some(call)) => call,
            Ok(None) => return,
            Err(error) => {
                tracing::warn!(%error, call = %call_id.to_text(), "cannot read the call row to mirror");
                return;
            }
        };
        let payload = match encode_envelope(Opcode::FedCallRows, &call_wire(&call)) {
            Ok(payload) => payload,
            Err(error) => {
                tracing::warn!(%error, call = %call_id.to_text(), "cannot encode the call row to mirror");
                return;
            }
        };
        for peer in self.allowed_peers().await {
            let event = FederatedEvent {
                target_node: peer,
                opcode: Opcode::FedCallRows.to_wire() as i32,
                payload: payload.clone(),
            };
            if let Err(error) = self.mesh.enqueue(event, now).await {
                tracing::warn!(
                    %error,
                    peer = %peer.to_text(),
                    "cannot enqueue a call row mirror for this peer"
                );
            }
        }
    }

    /// Applies a call row a peer sent, reporting whether it was written.
    ///
    /// The one rule the tier turns on: a peer may fill a row this node asked
    /// for and may update a row this node already holds, but may never plant a
    /// row this node never had and never asked about. The first branch is the
    /// pull's answer and the second is the push, and a frame that is neither
    /// is refused with a warning rather than written — an unsolicited row is
    /// how a peer would put a call this node never minted into a store whose
    /// readers treat it as local truth.
    ///
    /// An update is ordered by the state machine and not by arrival: the wire
    /// numbering is already the order the states advance in, so a row is
    /// written only when it moves the call forward. Two nodes that end the
    /// same call concurrently therefore converge on the first end rather than
    /// racing a `Connecting` mirror over an `Ended` row, and the reason the
    /// other party already heard is not rewritten under them.
    pub(crate) async fn apply_call_rows(&self, rows: FedCallRows) -> Result<bool> {
        let Some(calls) = &self.calls else {
            return Ok(false);
        };
        let asked = self.asked_calls.lock().contains(&rows.call_id);
        let held = calls.get(rows.call_id).await?;
        if !asked && held.is_none() {
            tracing::warn!(
                call = %rows.call_id.to_text(),
                "refusing call rows this node never held and no ask of this process is waiting for"
            );
            return Ok(false);
        }
        let Some(row) = call_of(&rows) else {
            tracing::warn!(
                call = %rows.call_id.to_text(),
                "refusing a replicated call row this build cannot decode"
            );
            self.asked_calls.lock().remove(&rows.call_id);
            return Ok(false);
        };
        if let Some(held) = &held {
            if state_rank(row.state) <= state_rank(held.state) {
                // Already at or past this row's state. The ask, if there was
                // one, is answered by what this node holds: there is nothing
                // left to wait for.
                self.asked_calls.lock().remove(&rows.call_id);
                return Ok(false);
            }
        }
        if let Err(error) = calls.put(&row).await {
            tracing::warn!(
                %error,
                call = %rows.call_id.to_text(),
                "cannot seat the replicated call row"
            );
            return Ok(false);
        }
        self.asked_calls.lock().remove(&rows.call_id);
        Ok(true)
    }

    /// The allowed peers an ask broadcasts to. Degraded peers still count —
    /// degraded is a signal about a link's health, not a suspension, and a
    /// slow answer still beats none (section 153).
    async fn allowed_peers(&self) -> Vec<Id> {
        self.mesh
            .peers(PEER_SCAN_LIMIT)
            .await
            .map(|peers| {
                peers
                    .into_iter()
                    .filter(|peer| peer.status.is_allowed())
                    .map(|peer| peer.node_id)
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// The wire numbering of a visibility setting. The store's own numbering is
/// the numbering the schema's `u32` carries (0 nobody, 1 friends, 2 everyone).
fn visibility_wire(visibility: migo_store::model::Visibility) -> u32 {
    u32::from(visibility.to_i16() as u16)
}

/// The store's visibility for a wire numbering. An unknown value reads as the
/// most private option, the same direction the store's own decode leans: a
/// replica that cannot read a setting must not widen it.
fn visibility_of(value: u32) -> migo_store::model::Visibility {
    migo_store::model::Visibility::from_i16(i16::try_from(value).unwrap_or(0))
}

/// A call row as the mesh carries it.
///
/// Every field, verbatim: the replica's whole job is to let a node that never
/// accepted the invite resolve the frames that arrive for it, and a row
/// reconstructed from anything less — a state without its deadline, an answer
/// without the device that gave it — would let that node answer a different
/// call than the one being asked about.
fn call_wire(call: &Call) -> FedCallRows {
    FedCallRows {
        call_id: call.call_id,
        conversation_id: call.conversation_id,
        caller_id: call.caller_id,
        caller_device: call.caller_device,
        callee_id: call.callee_id,
        media_kind: call.media_kind,
        state: call.state.to_wire(),
        expires_at: call.expires_at,
        callee_device: call.callee_device,
        end_reason: call.end_reason.map(EndReason::to_wire),
        answered_at: call.answered_at,
        ended_at: call.ended_at,
    }
}

/// The store's call row for a replicated one, or `None` when this build cannot
/// read it.
///
/// An unknown state is the refusal that matters: the state machine is a closed
/// set this build matches on exhaustively, so a row in a state no arm covers is
/// a row no handler could answer, and reading it as any known state would be
/// inventing a call. An unknown *reason* is read as no reason at all — the
/// state is what every reader branches on, and a call that ended for a reason
/// this build cannot name is still, unmistakably, over.
fn call_of(rows: &FedCallRows) -> Option<Call> {
    Some(Call {
        call_id: rows.call_id,
        conversation_id: rows.conversation_id,
        caller_id: rows.caller_id,
        caller_device: rows.caller_device,
        callee_id: rows.callee_id,
        callee_device: rows.callee_device,
        media_kind: rows.media_kind,
        state: CallState::from_wire(rows.state)?,
        end_reason: rows.end_reason.and_then(EndReason::from_wire),
        expires_at: rows.expires_at,
        answered_at: rows.answered_at,
        ended_at: rows.ended_at,
    })
}

/// How far along the state machine a call is, for the one ordering the tier
/// applies to a mirrored row.
///
/// Read off the wire numbering itself, which is already the order the states
/// advance in — ring 0, connecting 1, connected 2, ended 4 — with three left
/// out because it is the SFU's `Reconnecting`, a state no row this build
/// writes can carry.
fn state_rank(state: CallState) -> u32 {
    state.to_wire()
}

/// The late-bound cell the privacy gate holds, filled by the composition root
/// once the mesh and the relay exist.
///
/// The gate is built before the mesh — messaging opens above the store and
/// social graph, federation opens a layer later — and the handle is the same
/// one-slot cell the gateway handle is: the gate binds it now, empty, and the
/// composition root fills it the moment the relay is built. Before that point
/// the gate answers exactly as it did before this tier existed, which is
/// correct: no mesh, no rows to pull.
pub struct ReplicationHandle {
    relay: std::sync::OnceLock<std::sync::Arc<ReplicationRelay>>,
}

impl ReplicationHandle {
    /// An empty handle, to be filled once the relay is built.
    #[must_use]
    pub fn new() -> Self {
        Self {
            relay: std::sync::OnceLock::new(),
        }
    }

    /// Binds the relay. The first call wins and later calls are ignored,
    /// because a process has exactly one relay and rebinding it would be a bug
    /// rather than a feature.
    pub fn set(&self, relay: std::sync::Arc<ReplicationRelay>) {
        let _ = self.relay.set(relay);
    }

    /// The relay, once bound; `None` during the startup window before it is.
    pub(crate) fn get(&self) -> Option<&std::sync::Arc<ReplicationRelay>> {
        self.relay.get()
    }
}

impl Default for ReplicationHandle {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    //! A relay over a real mesh service and a memory store: the answer half's
    //! verbatim rows and its silences, the apply half's asked-set honesty, and
    //! the ask half's fast failures — with no wire at all, the same way the
    //! conversation relay's tests run. The waiting half (ask, sleep, apply,
    //! succeed) is the two-node test `dm_federation.rs` carries, because a
    //! bounded wait is only honest to test against a real second node.

    use super::*;
    use migo_calls::{MemoryCallStore, MEDIA_AUDIO};
    use migo_core::random::SeededRandom;
    use migo_core::Secret;
    use migo_crypto::NodeSecret;
    use migo_federation::{MeshService, NewPeerSpec};
    use migo_protocol::{from_frame, Frame};
    use migo_store::model::Visibility;
    use migo_store::MemoryStore;
    use std::sync::Arc;

    const NOW: i64 = 1_700_000_000_000;

    /// A mesh whose region is `region`, with `peer` admitted at `peer_region`.
    async fn mesh_in(region: &str, peer: Id, peer_region: &str) -> SharedMesh {
        let registry = migo_core::metrics::Registry::new();
        let mut seed = [0u8; 32];
        seed[..region.len()].copy_from_slice(region.as_bytes());
        let secret = NodeSecret::from_seed(&seed).expect("a 32-byte seed builds a key");
        let mesh = MeshService::new(
            Arc::new(MemoryStore::new()),
            migo_federation::MeshConfig::default(),
            Id::from(0xABCD),
            region.to_string(),
            secret,
            Box::new(SeededRandom::new(42)),
            &registry,
        )
        .expect("the mesh configuration is valid");
        let mesh: SharedMesh = Arc::new(mesh);
        mesh.add_peer(
            NewPeerSpec {
                node_id: peer,
                public_key: NodeSecret::from_seed(&[9u8; 32])
                    .expect("a seed builds a key")
                    .public()
                    .to_bytes()
                    .to_vec(),
                base_url: "wss://peer.test:9999".to_string(),
                region: peer_region.to_string(),
            },
            Timestamp::from_millis(NOW),
        )
        .await
        .expect("a fresh allow-list admits the peer");
        mesh
    }

    /// A mesh with no peers at all: the fast-failure shape.
    async fn lonely_mesh(region: &str) -> SharedMesh {
        let registry = migo_core::metrics::Registry::new();
        let mut seed = [0u8; 32];
        seed[..region.len()].copy_from_slice(region.as_bytes());
        let secret = NodeSecret::from_seed(&seed).expect("a 32-byte seed builds a key");
        let mesh = MeshService::new(
            Arc::new(MemoryStore::new()),
            migo_federation::MeshConfig::default(),
            Id::from(0xABCD),
            region.to_string(),
            secret,
            Box::new(SeededRandom::new(42)),
            &registry,
        )
        .expect("the mesh configuration is valid");
        Arc::new(mesh)
    }

    /// A store holding one account with its profile, the shape a node that
    /// *owns* an account always has.
    async fn store_with_account(account_id: Id, username: &str) -> SharedStore {
        let store: SharedStore = Arc::new(MemoryStore::new());
        store
            .create_account(NewAccount {
                account_id,
                username: username.to_string(),
                email: None,
                phone: None,
                passphrase_hash: Secret::new("argon2id-hash-of-the-passphrase"),
                locale: "en-US".to_string(),
                country: Some("ID".to_string()),
                created_at: Timestamp::from_millis(NOW),
            })
            .await
            .expect("a fresh store takes the account");
        store
            .create_profile(Profile {
                account_id,
                display_name: username.to_string(),
                bio: Some("a bio".to_string()),
                avatar_media_id: None,
                birth_year: Some(1990),
                gender: Some(Gender::Other),
                show_last_seen: Visibility::Everyone,
                who_can_message: Visibility::Friends,
                who_can_add: Visibility::Everyone,
                who_can_call_voice: Visibility::Friends,
                who_can_call_video: Visibility::Friends,
                searchable: true,
                custom_status: None,
                updated_at: Timestamp::from_millis(NOW + 1),
            })
            .await
            .expect("a fresh store takes the profile");
        store
    }

    /// The first queued answer, decoded from the outbox.
    async fn the_answer(mesh: &SharedMesh) -> Option<(Id, Opcode, Frame)> {
        let due = mesh
            .due(Timestamp::from_millis(NOW + 60_000))
            .await
            .expect("the queue reads");
        let first = due.first()?;
        let frame = Frame::decode(bytes::Bytes::from(first.payload.clone()))
            .expect("an outbox payload is an encoded frame");
        Some((
            first.target_node,
            Opcode::from_wire(frame.header.opcode)?,
            frame,
        ))
    }

    /// An answer carries the rows verbatim and only the edges the asker's gate
    /// reads: the account, the profile with its settings, and the one edge
    /// between the queried account and the regarding one — never the whole
    /// graph.
    #[tokio::test]
    async fn an_account_answer_carries_the_rows_and_the_shared_edges_only() {
        let owner = Id::from(0xAAAA);
        let peer = Id::from(0x4444);
        let regarding = Id::from(0xBBBB);
        let mesh = mesh_in("region-1", peer, "region-2").await;
        let store = store_with_account(owner, "theowner").await;
        store
            .put_relationship(Relationship {
                account_id: owner,
                other_id: regarding,
                kind: RelationshipKind::Friend,
                created_at: Timestamp::from_millis(NOW),
                accepted_at: Some(Timestamp::from_millis(NOW + 1)),
            })
            .await
            .expect("the friendship edge writes");
        // An edge to a third account: real, owned, and none of the asker's
        // business — the answer must not carry it.
        store
            .put_relationship(Relationship {
                account_id: owner,
                other_id: Id::from(0xCCCC),
                kind: RelationshipKind::Follow,
                created_at: Timestamp::from_millis(NOW),
                accepted_at: None,
            })
            .await
            .expect("the follow edge writes");
        let relay = ReplicationRelay::new(Arc::clone(&mesh), store);

        relay
            .answer_account(
                peer,
                FedAccountQuery {
                    epoch: mesh.epoch(),
                    account_id: owner,
                    regarding,
                },
                Timestamp::from_millis(NOW),
            )
            .await
            .expect("a held account answers");
        let (target, opcode, frame) = the_answer(&mesh).await.expect("the answer is queued");
        assert_eq!(target, peer, "the answer names the asker");
        assert_eq!(opcode, Opcode::FedAccountRows);
        let rows: FedAccountRows = from_frame(&frame).expect("the rows decode");
        assert_eq!(rows.account_id, owner);
        assert_eq!(rows.username, "theowner");
        assert_eq!(rows.passphrase_hash, "argon2id-hash-of-the-passphrase");
        assert_eq!(rows.who_can_message, 1, "the Friends setting crosses as 1");
        assert_eq!(
            (rows.who_can_call_voice, rows.who_can_call_video),
            (Some(1), Some(1)),
            "both call policies cross, because a peer that rings on the replicated \
             profile must apply the same policy the owning node would"
        );
        assert_eq!(
            rows.gender,
            Some(3),
            "the disclosure numbering crosses as 3"
        );
        assert_eq!(rows.country.as_deref(), Some("ID"));
        assert_eq!(rows.edges.len(), 1, "only the regarding edges cross");
        assert_eq!(rows.edges[0].kind, RelationshipKind::Friend);
        assert_eq!(rows.edges[0].other_id, regarding);
        assert!(rows.edges[0].accepted_at.is_some());
    }

    /// A node that holds nothing whole stays silent: silence is the answer the
    /// asker's bounded wait turns into the same fail-closed refusal.
    #[tokio::test]
    async fn an_account_answer_is_silent_without_whole_rows() {
        let peer = Id::from(0x4444);
        let mesh = mesh_in("region-1", peer, "region-2").await;
        let store: SharedStore = Arc::new(MemoryStore::new());
        let relay = ReplicationRelay::new(Arc::clone(&mesh), store);

        relay
            .answer_account(
                peer,
                FedAccountQuery {
                    epoch: mesh.epoch(),
                    account_id: Id::from(0xAAAA),
                    regarding: Id::from(0xBBBB),
                },
                Timestamp::from_millis(NOW),
            )
            .await
            .expect("not holding an account is a silence, not an error");
        assert!(
            mesh.due(Timestamp::from_millis(NOW + 60_000))
                .await
                .expect("the queue reads")
                .is_empty(),
            "nothing was answered"
        );
    }

    /// A stale epoch is refused, the same re-handshake contract the two tiers
    /// before this one enforce.
    #[tokio::test]
    async fn an_account_answer_refuses_a_stale_epoch() {
        let peer = Id::from(0x4444);
        let mesh = mesh_in("region-1", peer, "region-2").await;
        let owner = Id::from(0xAAAA);
        let store = store_with_account(owner, "theowner").await;
        let relay = ReplicationRelay::new(Arc::clone(&mesh), store);

        // The view has moved once, so the epoch one behind is genuinely stale.
        mesh.bump_epoch();
        let stale = mesh.epoch() - 1;
        let answer = relay
            .answer_account(
                peer,
                FedAccountQuery {
                    epoch: stale,
                    account_id: owner,
                    regarding: Id::from(0xBBBB),
                },
                Timestamp::from_millis(NOW),
            )
            .await;
        assert!(answer.is_err(), "a stale epoch is refused");
    }

    /// A conversation answer carries the row verbatim — the home label above
    /// all — and the members still seated.
    #[tokio::test]
    async fn a_conversation_answer_carries_the_row_and_its_current_members() {
        let peer = Id::from(0x4444);
        let mesh = mesh_in("region-1", peer, "region-2").await;
        let conversation_id = Id::from(0x1111);
        let store: SharedStore = Arc::new(MemoryStore::new());
        store
            .direct_conversation(
                Id::from(0xAAAA),
                Id::from(0xBBBB),
                conversation_id,
                migo_protocol::EncryptionMode::EndToEnd,
                "region-1".to_string(),
                Timestamp::from_millis(NOW),
            )
            .await
            .expect("a fresh pair builds a conversation");
        let relay = ReplicationRelay::new(Arc::clone(&mesh), store);

        relay
            .answer_conversation(
                peer,
                FedConversationQuery {
                    epoch: mesh.epoch(),
                    conversation_id,
                },
                Timestamp::from_millis(NOW),
            )
            .await
            .expect("a held conversation answers");
        let (target, opcode, frame) = the_answer(&mesh).await.expect("the answer is queued");
        assert_eq!(target, peer);
        assert_eq!(opcode, Opcode::FedConversationRows);
        let rows: FedConversationRows = from_frame(&frame).expect("the rows decode");
        assert_eq!(rows.conversation_id, conversation_id);
        assert_eq!(
            rows.home_region, "region-1",
            "the home label crosses verbatim"
        );
        assert_eq!(rows.members.len(), 2, "both members are seated");
        assert!(rows.members.contains(&Id::from(0xAAAA)));
        assert!(rows.members.contains(&Id::from(0xBBBB)));
    }

    /// Applied rows seat the account, the profile, and the edges — and the
    /// asked entry leaves with the apply, so a second answer to the same ask
    /// is refused rather than written twice.
    #[tokio::test]
    async fn applied_account_rows_seat_the_rows_and_spend_the_ask() {
        let peer = Id::from(0x4444);
        let mesh = mesh_in("region-1", peer, "region-2").await;
        let owner = Id::from(0xAAAA);
        let regarding = Id::from(0xBBBB);
        let home = store_with_account(owner, "theowner").await;
        // The edge the asker's gate will read: seeded on the owner's store so
        // the answer carries it — the apply below must seat it on the asker.
        home.put_relationship(Relationship {
            account_id: owner,
            other_id: regarding,
            kind: RelationshipKind::Friend,
            created_at: Timestamp::from_millis(NOW),
            accepted_at: Some(Timestamp::from_millis(NOW + 1)),
        })
        .await
        .expect("the friendship edge writes on the owner's store");
        let relay_home = ReplicationRelay::new(Arc::clone(&mesh), home);
        relay_home
            .answer_account(
                peer,
                FedAccountQuery {
                    epoch: mesh.epoch(),
                    account_id: owner,
                    regarding,
                },
                Timestamp::from_millis(NOW),
            )
            .await
            .expect("the owner answers");
        let (_, _, frame) = the_answer(&mesh).await.expect("the answer is queued");
        let rows: FedAccountRows = from_frame(&frame).expect("the rows decode");

        // The asker: an empty store, an ask in flight, and the answer.
        let asker_store: SharedStore = Arc::new(MemoryStore::new());
        let asker = ReplicationRelay::new(Arc::clone(&mesh), Arc::clone(&asker_store));
        assert!(
            !asker
                .apply_account_rows(rows.clone())
                .await
                .expect("unsolicited rows are refused, not an error"),
            "rows no ask is waiting for are not applied"
        );
        assert!(
            asker_store
                .account_by_id(owner)
                .await
                .expect("reads")
                .is_none(),
            "nothing was written"
        );

        assert_eq!(
            asker
                .ask_account(owner, regarding, Timestamp::from_millis(NOW))
                .await,
            1,
            "one allowed peer was asked"
        );
        assert!(
            asker
                .apply_account_rows(rows.clone())
                .await
                .expect("an answer to a live ask applies"),
            "the rows were written"
        );
        let account = asker_store
            .account_by_id(owner)
            .await
            .expect("reads")
            .expect("the replicated account row is seated");
        assert_eq!(account.username, "theowner");
        let profile = asker_store
            .profile(owner)
            .await
            .expect("reads")
            .expect("the replicated profile row is seated");
        assert_eq!(
            profile.who_can_message,
            Visibility::Friends,
            "the privacy setting crossed as the store's own enum"
        );
        let edge = asker_store
            .relationship(owner, regarding, RelationshipKind::Friend)
            .await
            .expect("reads")
            .expect("the shared friendship edge crossed");
        assert!(edge.accepted_at.is_some());

        // The ask is spent: a replayed answer is refused.
        assert!(
            !asker
                .apply_account_rows(rows)
                .await
                .expect("a spent ask is a refusal, not an error"),
            "the ask left with the apply"
        );
    }

    /// Applied conversation rows keep the home label, because that is the one
    /// fact every node holding a copy must read the same answer from.
    #[tokio::test]
    async fn applied_conversation_rows_keep_the_home_label() {
        let peer = Id::from(0x4444);
        let mesh = mesh_in("region-1", peer, "region-2").await;
        let conversation_id = Id::from(0x1111);
        let rows = FedConversationRows {
            conversation_id,
            kind: migo_protocol::ConversationKind::Direct,
            encryption: migo_protocol::EncryptionMode::EndToEnd,
            home_region: "region-2".to_string(),
            created_by: Id::from(0xAAAA),
            created_at: Timestamp::from_millis(NOW),
            last_seq: 7,
            members: vec![Id::from(0xAAAA), Id::from(0xBBBB)],
            room_id: None,
            title: None,
            last_message_at: None,
            archived_at: None,
        };
        let store: SharedStore = Arc::new(MemoryStore::new());
        let relay = ReplicationRelay::new(Arc::clone(&mesh), Arc::clone(&store));

        assert!(
            !relay
                .apply_conversation_rows(rows.clone())
                .await
                .expect("unsolicited rows are refused, not an error"),
            "no ask is waiting, so nothing applies"
        );
        assert_eq!(
            relay
                .ask_conversation(conversation_id, Timestamp::from_millis(NOW))
                .await,
            1
        );
        assert!(
            relay
                .apply_conversation_rows(rows)
                .await
                .expect("an answer to a live ask applies"),
            "the row was written"
        );
        let row = store
            .conversation(conversation_id)
            .await
            .expect("reads")
            .expect("the replicated row is seated");
        assert_eq!(row.home_region, "region-2");
        assert_eq!(
            store.members(conversation_id).await.expect("reads").len(),
            2,
            "the members crossed with the row"
        );
    }

    /// An ask with no allowed peer fails closed at once: the wait is for an
    /// answer, and a node linked to nobody cannot be sent one.
    #[tokio::test]
    async fn an_ask_with_no_peers_fails_closed_without_waiting() {
        let mesh = lonely_mesh("region-1").await;
        let store: SharedStore = Arc::new(MemoryStore::new());
        let relay = ReplicationRelay::new(Arc::clone(&mesh), Arc::clone(&store));

        let started = std::time::Instant::now();
        assert!(
            !relay
                .ensure_account(
                    Id::from(0xAAAA),
                    Id::from(0xBBBB),
                    Timestamp::from_millis(NOW)
                )
                .await,
            "no peer to ask, so the rows cannot arrive"
        );
        assert!(
            !relay
                .ensure_conversation(Id::from(0x1111), Timestamp::from_millis(NOW))
                .await,
            "no peer to ask, so the row cannot arrive"
        );
        assert!(
            started.elapsed() < ASK_WAIT,
            "the failure was immediate, not a spent wait"
        );
        assert!(
            mesh.due(Timestamp::from_millis(NOW + 60_000))
                .await
                .expect("the queue reads")
                .is_empty(),
            "nothing was enqueued"
        );
    }

    /// An already-held row needs no ask: the fast path the common case takes,
    /// on every send after the first.
    #[tokio::test]
    async fn a_held_row_needs_no_ask() {
        let peer = Id::from(0x4444);
        let mesh = mesh_in("region-1", peer, "region-2").await;
        let owner = Id::from(0xAAAA);
        let store = store_with_account(owner, "theowner").await;
        let relay = ReplicationRelay::new(Arc::clone(&mesh), Arc::clone(&store));

        assert!(
            relay
                .ensure_account(owner, Id::from(0xBBBB), Timestamp::from_millis(NOW))
                .await,
            "the rows are already held"
        );
        assert!(
            mesh.due(Timestamp::from_millis(NOW + 60_000))
                .await
                .expect("the queue reads")
                .is_empty(),
            "nothing was asked for"
        );
    }

    /// The row a node that minted the invite holds: ringing, no callee device,
    /// which is the shape the pull is asked about.
    fn ringing_call(call_id: Id, caller: Id, callee: Id) -> Call {
        Call {
            call_id,
            conversation_id: Id::from(0x1111),
            caller_id: caller,
            caller_device: Id::from(0xD1),
            callee_id: callee,
            callee_device: None,
            media_kind: MEDIA_AUDIO,
            state: CallState::Ringing,
            end_reason: None,
            expires_at: Timestamp::from_millis(NOW + 30_000),
            answered_at: None,
            ended_at: None,
        }
    }

    /// A call store holding one row.
    async fn store_with_call(call: &Call) -> SharedCallStore {
        let store: SharedCallStore = Arc::new(MemoryCallStore::new());
        store
            .put(call)
            .await
            .expect("a fresh call store takes the row");
        store
    }

    /// The third tier's one rule, in both directions: an answer to this node's
    /// own ask is seated, and a row nobody asked about and nobody holds is
    /// refused — an unsolicited row is how a peer would plant a call this node
    /// never minted into a store whose readers treat it as local truth.
    #[tokio::test]
    async fn a_queried_call_row_is_seated_and_an_unqueried_one_is_refused() {
        let peer = Id::from(0x4444);
        let call_id = Id::from(0xCA11);
        let mesh = mesh_in("region-1", peer, "region-2").await;
        let mine: SharedCallStore = Arc::new(MemoryCallStore::new());
        let relay = ReplicationRelay::new(Arc::clone(&mesh), Arc::new(MemoryStore::new()))
            .with_calls(Arc::clone(&mine));
        let row = ringing_call(call_id, Id::from(0xAAAA), Id::from(0xBBBB));

        assert!(
            !relay
                .apply_call_rows(call_wire(&row))
                .await
                .expect("the apply reads"),
            "a row this node never held and never asked for is refused"
        );
        assert!(
            mine.get(call_id).await.expect("the store reads").is_none(),
            "nothing was planted"
        );

        assert_eq!(
            relay.ask_call(call_id, Timestamp::from_millis(NOW)).await,
            1,
            "the ask reaches the one allowed peer"
        );
        let mut answered = row;
        answered.state = CallState::Connecting;
        answered.callee_device = Some(Id::from(0xD2));
        assert!(
            relay
                .apply_call_rows(call_wire(&answered))
                .await
                .expect("the apply reads"),
            "the answer to this node's own ask is seated"
        );
        let seated = mine
            .get(call_id)
            .await
            .expect("the store reads")
            .expect("the row is seated");
        assert_eq!(seated.state, CallState::Connecting);
        assert_eq!(
            seated.callee_device,
            Some(Id::from(0xD2)),
            "the device that answered crosses, because a replica without it cannot route the caller's SDP"
        );
    }

    /// An update is ordered by the state machine and not by arrival: two nodes
    /// that end the same call concurrently converge on the first end rather
    /// than letting a stale mirror resurrect a call both parties watched die.
    #[tokio::test]
    async fn a_call_mirror_never_moves_a_row_backwards() {
        let peer = Id::from(0x4444);
        let call_id = Id::from(0xCA11);
        let mesh = mesh_in("region-1", peer, "region-2").await;
        let mut ended = ringing_call(call_id, Id::from(0xAAAA), Id::from(0xBBBB));
        ended.state = CallState::Ended;
        ended.end_reason = Some(EndReason::ByCaller);
        ended.ended_at = Some(Timestamp::from_millis(NOW + 5));
        let mine = store_with_call(&ended).await;
        let relay = ReplicationRelay::new(Arc::clone(&mesh), Arc::new(MemoryStore::new()))
            .with_calls(Arc::clone(&mine));

        // A mirror that was in flight when the call ended, and lands after it.
        let mut stale = ended.clone();
        stale.state = CallState::Connecting;
        stale.callee_device = Some(Id::from(0xD2));
        stale.end_reason = None;
        stale.ended_at = None;
        assert!(
            !relay
                .apply_call_rows(call_wire(&stale))
                .await
                .expect("the apply reads"),
            "a row that does not advance the call is refused"
        );
        let held = mine
            .get(call_id)
            .await
            .expect("the store reads")
            .expect("the row is still held");
        assert_eq!(held.state, CallState::Ended, "the end stands");
        assert_eq!(held.end_reason, Some(EndReason::ByCaller));

        // The other node ended it too, and said so differently: the first end
        // is the one both parties converge on, not the last one to arrive.
        let mut other_end = stale;
        other_end.state = CallState::Ended;
        other_end.end_reason = Some(EndReason::ByCallee);
        other_end.ended_at = Some(Timestamp::from_millis(NOW + 9));
        assert!(
            !relay
                .apply_call_rows(call_wire(&other_end))
                .await
                .expect("the apply reads"),
            "a second end at the same rank is refused"
        );
        let held = mine
            .get(call_id)
            .await
            .expect("the store reads")
            .expect("the row is still held");
        assert_eq!(
            held.end_reason,
            Some(EndReason::ByCaller),
            "the reason both parties already heard is not rewritten under them"
        );
    }

    /// A relay built without a call store keeps the whole third tier inert
    /// rather than wrong: nothing is held, nothing is asked, nothing is
    /// mirrored, and the two tiers above it are untouched by its absence.
    #[tokio::test]
    async fn a_relay_without_a_call_store_keeps_the_call_tier_inert() {
        let peer = Id::from(0x4444);
        let call_id = Id::from(0xCA11);
        let mesh = mesh_in("region-1", peer, "region-2").await;
        let relay = ReplicationRelay::new(Arc::clone(&mesh), Arc::new(MemoryStore::new()));

        let started = std::time::Instant::now();
        assert!(
            !relay
                .ensure_call(call_id, Timestamp::from_millis(NOW))
                .await,
            "no store, so the row cannot arrive"
        );
        assert!(
            started.elapsed() < ASK_WAIT,
            "the failure was immediate, not a spent wait"
        );
        assert!(
            !relay
                .apply_call_rows(call_wire(&ringing_call(
                    call_id,
                    Id::from(0xAAAA),
                    Id::from(0xBBBB)
                )))
                .await
                .expect("the apply reads"),
            "there is nowhere to seat a replicated row"
        );
        relay
            .publish_call(call_id, Timestamp::from_millis(NOW))
            .await;
        assert!(
            mesh.due(Timestamp::from_millis(NOW + 60_000))
                .await
                .expect("the queue reads")
                .is_empty(),
            "nothing was asked for and nothing was mirrored"
        );
    }

    /// The push half: a row this node changed is mirrored to every allowed
    /// peer, whole, so the node that minted the invite learns the device the
    /// callee answered from.
    #[tokio::test]
    async fn a_changed_call_row_is_mirrored_to_every_allowed_peer() {
        let peer = Id::from(0x4444);
        let call_id = Id::from(0xCA11);
        let mesh = mesh_in("region-1", peer, "region-2").await;
        let mut answered = ringing_call(call_id, Id::from(0xAAAA), Id::from(0xBBBB));
        answered.state = CallState::Connected;
        answered.callee_device = Some(Id::from(0xD2));
        answered.answered_at = Some(Timestamp::from_millis(NOW + 2));
        let calls = store_with_call(&answered).await;
        let relay = ReplicationRelay::new(Arc::clone(&mesh), Arc::new(MemoryStore::new()))
            .with_calls(Arc::clone(&calls));

        relay
            .publish_call(call_id, Timestamp::from_millis(NOW))
            .await;
        let (target, opcode, frame) = the_answer(&mesh).await.expect("the mirror is queued");
        assert_eq!(target, peer, "the mirror names the peer");
        assert_eq!(opcode, Opcode::FedCallRows);
        let rows: FedCallRows = from_frame(&frame).expect("the rows decode");
        assert_eq!(rows.call_id, call_id);
        assert_eq!(rows.state, CallState::Connected.to_wire());
        assert_eq!(rows.callee_device, Some(Id::from(0xD2)));
        assert_eq!(rows.end_reason, None);
        assert_eq!(
            rows.expires_at,
            Timestamp::from_millis(NOW + 30_000),
            "the replica owes the sweeper the same deadline the owner holds"
        );
    }

    /// A row this node does not hold is not mirrored at all: the push is driven
    /// by a write, and there was none here.
    #[tokio::test]
    async fn a_call_this_node_does_not_hold_is_never_mirrored() {
        let peer = Id::from(0x4444);
        let mesh = mesh_in("region-1", peer, "region-2").await;
        let calls: SharedCallStore = Arc::new(MemoryCallStore::new());
        let relay = ReplicationRelay::new(Arc::clone(&mesh), Arc::new(MemoryStore::new()))
            .with_calls(calls);

        relay
            .publish_call(Id::from(0xCA11), Timestamp::from_millis(NOW))
            .await;
        assert!(
            mesh.due(Timestamp::from_millis(NOW + 60_000))
                .await
                .expect("the queue reads")
                .is_empty(),
            "nothing was mirrored"
        );
    }
}
