//! The message sweeper: disappearing messages that actually disappear.
//!
//! # Why this lives in the composition root and not in `migo-messaging`
//!
//! A disappearing message whose deadline passed has no client left to delete
//! it — the sender's device may be off, and the *point* of the deadline is
//! that nobody has to remember. `migo-messaging` owns no timer (the
//! [`Messaging::purge_expired`](migo_messaging::Messaging::purge_expired) doc
//! says so in as many words: "for the background sweeper, not for a request
//! handler"), and the composition root is where a task that must outlive every
//! request and die with the node belongs — the same seat the call sweeper
//! sits in, for the same reason.
//!
//! # The tick
//!
//! One minute, not derived from any message's `expires_in_ms`: the deadline is
//! the client's to honour locally (the sweep publishes nothing — a client that
//! learned of the expiry only from the server would show the message for a
//! full tick past when it promised to vanish), and the tick bounds only how
//! long a *row* outlives its deadline in the store. Each tick is one bounded
//! delete; a node that falls behind catches up at `SWEEP_LIMIT` rows per tick
//! rather than holding locks across the table in one statement.
//!
//! No fanout, and that is deliberate: every client that ever saw the message
//! was handed the same deadline at delivery, so a broadcast would say what
//! they already knew, timed to arrive after they acted on it.

use std::sync::Arc;
use std::time::Duration;

use migo_core::{Clock, Shutdown};
use migo_messaging::SharedMessaging;

use crate::compose::App;

/// How often the sweeper looks for expired messages.
const SWEEP_INTERVAL: Duration = Duration::from_secs(60);

/// How many rows one tick may delete. Bounded so a node catching up on a
/// backlog of expiries holds no long transaction against a table every send
/// appends to.
const SWEEP_LIMIT: u16 = 500;

impl App {
    /// Spawns the message sweeper, returning its handle.
    ///
    /// [`App::serve`] spawns this for the whole serve. Public for the same
    /// reason the call sweeper's spawn is: the behaviour it owns — a row that
    /// is gone from the store a tick past its deadline — is observable from
    /// outside, so a test starts the task by hand.
    #[must_use]
    pub fn spawn_message_sweeper(&self) -> tokio::task::JoinHandle<()> {
        spawn(
            self.messaging.clone(),
            self.clock.clone(),
            self.shutdown.clone(),
            SWEEP_INTERVAL,
        )
    }
}

/// Runs the sweep forever, or until `shutdown` fires. The interval is a
/// parameter so the test below can prove the loop purges without waiting out
/// the production minute.
fn spawn(
    messaging: SharedMessaging,
    clock: Arc<dyn Clock>,
    shutdown: Shutdown,
    interval: Duration,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => break,
                _ = tokio::time::sleep(interval) => {
                    sweep_once(messaging.as_ref(), clock.as_ref()).await;
                }
            }
        }
    })
}

/// One tick: delete what expired, log rather than die — a sweeper that crashed
/// on one failure would leave every later expiry alive.
async fn sweep_once(messaging: &dyn migo_messaging::Messaging, clock: &dyn Clock) {
    let now = clock.now();
    match messaging.purge_expired(now, SWEEP_LIMIT).await {
        Ok(purged) if purged > 0 => {
            tracing::debug!(
                purged,
                "disappearing messages passed their deadline and were deleted"
            );
        }
        Ok(_) => {}
        Err(error) => tracing::warn!(%error, "the message sweep failed"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::time::Duration;

    use migo_core::metrics::Registry;
    use migo_core::{ManualClock, Timestamp};
    use migo_messaging::model::Caller;
    use migo_messaging::{MessageGate, OpenGate};
    use migo_protocol::{
        ConversationCreateRequest, ConversationKind, MessageAccepted, MessageKind, MessageSend,
    };
    use migo_ratelimit::TrustTier;

    use migo_messaging::service::Messages;

    /// The sweeper, given a tick the test can live with: the loop, the
    /// shutdown race, and the per-tick error tolerance are what is under
    /// test, not the length of the production minute.
    const TEST_TICK: Duration = Duration::from_millis(25);

    /// A send window short enough that a handful of ticks spans it.
    const EXPIRES_IN_MS: u32 = 60;

    #[tokio::test]
    async fn a_disappearing_message_is_deleted_by_the_sweeper_alone() {
        let store = migo_store::MemoryStore::new();
        let cache = migo_cache::MemoryCache::new();
        let registry = Registry::new();
        let policies = migo_ratelimit::Policies::from_config(&Default::default())
            .expect("default policies are valid");
        let limiter =
            migo_ratelimit::CacheRateLimiter::new(std::sync::Arc::new(cache), policies, &registry);
        let gate: std::sync::Arc<dyn MessageGate> = std::sync::Arc::new(OpenGate);
        let messaging: SharedMessaging = std::sync::Arc::new(Messages::new(
            std::sync::Arc::new(store),
            // The typing cache the service shares with presence: a fresh memory
            // cache is what the composition root hands a memory-backend node.
            std::sync::Arc::new(migo_cache::MemoryCache::new()),
            limiter.into(),
            gate,
            std::sync::Arc::new(migo_messaging::FreeKicks),
            &registry,
            Box::new(migo_core::SeededRandom::new(0x5eed_9001)) as Box<dyn migo_core::Random>,
        ));

        // The clock the sweeper reads and the clock that stamps sends are the
        // same clock, because in production they are: `expires_at` is computed
        // from the server's own time precisely so no other clock can disagree.
        let clock = std::sync::Arc::new(ManualClock::new(Timestamp::from_millis(1_000_000)));

        let caller = Caller::new(
            migo_core::Id::from(1u128),
            migo_core::Id::from(101u128),
            TrustTier::Established,
            clock.now(),
        );
        let conversation = messaging
            .create(
                &caller,
                ConversationCreateRequest {
                    kind: ConversationKind::Direct,
                    members: vec![migo_core::Id::from(2u128)],
                    title: None,
                },
            )
            .await
            .expect("a direct conversation between two strangers is allowed")
            .conversation_id;

        let accepted: MessageAccepted = messaging
            .send(
                &caller,
                MessageSend {
                    message_id: migo_core::Id::from(41u128),
                    conversation_id: conversation,
                    kind: MessageKind::Text,
                    envelope: b"vanishes on its own".to_vec(),
                    expires_in_ms: Some(EXPIRES_IN_MS),
                    ..MessageSend::default()
                },
            )
            .await
            .expect("a disappearing message is accepted")
            .0;
        assert!(!accepted.duplicate.unwrap_or(false));

        let _sweeper = spawn(messaging.clone(), clock.clone(), Shutdown::new(), TEST_TICK);

        // Past the deadline — and only the sweeper deletes it: the test never
        // calls `purge_expired` itself, which is the whole point.
        clock.advance_millis(i64::from(EXPIRES_IN_MS) + 10);
        let deadline = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let count = remaining(messaging.as_ref(), &caller, conversation).await;
                if count == 0 {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        assert!(
            deadline.is_ok(),
            "the sweeper deleted the expired message without anyone asking"
        );

        _sweeper.abort();
    }

    /// How many messages `sync` still returns for the conversation — the view a
    /// client would see, which is the honest measure of "disappeared".
    async fn remaining(
        messaging: &dyn migo_messaging::Messaging,
        caller: &Caller,
        conversation: migo_core::Id,
    ) -> usize {
        let page = messaging
            .sync(
                caller,
                migo_protocol::SyncRequest {
                    conversation_id: conversation,
                    have_seq: 0,
                    limit: 50,
                    to_seq: None,
                    backwards: None,
                },
            )
            .await
            .expect("a member may read history")
            .messages
            .len();
        page
    }
}
