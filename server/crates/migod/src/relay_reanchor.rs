//! The relay re-anchor: the subscribe cache's repair tick.
//!
//! # Why this lives in the composition root and not in the relays
//!
//! A relay's `subscribed` set says "this process has already asked the home
//! node to watch this conversation or room" — once per process, because the
//! outbox owes the peer one ask, not one per subscribing session. The watch
//! table that ask fills is memory on the *home* node's side, and the two
//! halves disagree about one event: a home node restarting. The replacement
//! process holds an empty table; this node's cache still says every ask was
//! answered; and no client `SUBSCRIBE` is coming to re-ask, because the
//! sessions that wanted the topics are already granted them. The tier is then
//! down for every conversation and room whose far subscribers all sit here,
//! silently — a publish on the home node finds no watchers and fans out to
//! nobody, which is indistinguishable on the wire from a room that simply
//! went quiet.
//!
//! The relays own no timer (the same rule that keeps the sweepers out of the
//! domain crates), so the composition root is where the tick belongs, in the
//! same seat the call, message, and typing sweepers sit in and for the same
//! reason: it must outlive every request and die with the node.
//!
//! Every tick, both relays re-send the asks their caches remember. The
//! receiving side makes this cheap and safe: `register_watcher` is idempotent
//! (a home node that never restarted re-inserts what it already holds, one
//! set entry), its epoch check admits the ask (a restarted home node's fresh
//! epoch is never ahead of a peer that has been running), and an ask that
//! fails outright is re-marked in the cache so the next interval tries again
//! rather than retiring the anchor.
//!
//! # The tick
//!
//! `federation.reanchor_interval_ms`, one minute by default, and deliberately
//! not derived from anything else: the interval is how long a tier can stay
//! down after a home node restarts, not a property of the mesh's handshake
//! or delivery timing. Each tick walks the two caches — bounded by how many
//! conversations and rooms this node's sessions subscribe to — and the asks
//! ride the same outbox every federated event rides, so a home node that is
//! still down costs one queued ask per remembered subscription, retried by
//! the outbox's own backoff.

use std::sync::Arc;
use std::time::Duration;

use migo_core::{Clock, Shutdown};

use crate::compose::App;
use crate::conversation_relay::ConversationRelay;
use crate::room_relay::RoomRelay;

impl App {
    /// Spawns the relay re-anchor, returning its handle.
    ///
    /// [`App::serve`] spawns this for the whole serve, exactly as it spawns
    /// the three sweepers. Public for the same reason theirs are: the
    /// behaviour it owns — a far subscriber hearing events again after the
    /// conversation's home node restarted — is observable only from outside,
    /// so the wire test starts the task by hand against an app that never
    /// serves, with the interval shrunk through the same environment
    /// variable an operator would set.
    #[must_use]
    pub fn spawn_relay_reanchor(&self) -> tokio::task::JoinHandle<()> {
        spawn(
            Arc::clone(&self.room_relay),
            Arc::clone(&self.conversation_relay),
            self.clock.clone(),
            self.shutdown.clone(),
            self.federation_reanchor_interval,
        )
    }
}

/// Runs the re-anchor forever, or until `shutdown` fires. The interval is the
/// configuration's, read once at spawn: an operator changing it takes effect
/// on the next boot, like every other federation tuning the mesh holds.
fn spawn(
    room_relay: Arc<RoomRelay>,
    conversation_relay: Arc<ConversationRelay>,
    clock: Arc<dyn Clock>,
    shutdown: Shutdown,
    interval: Duration,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => break,
                _ = tokio::time::sleep(interval) => {
                    let now = clock.now();
                    room_relay.reanchor(now).await;
                    conversation_relay.reanchor(now).await;
                }
            }
        }
    })
}
