//! The media plane's door on this node: the tickets `migod` mints for it.
//!
//! Section 166 puts a forwarding SFU in front of every call of three or more, and section 92 puts
//! that SFU outside `migod` — its load profile is bandwidth, not application logic. The two
//! processes are separate deployments that scale apart, so they cannot share a session table and
//! must not share a database read on the admission path. What they share is a key: this module
//! signs a short-lived claim about one device in one call, the media process verifies it offline,
//! and neither has to ask the other anything.
//!
//! The claim is deliberately the smallest one that admits somebody — the call, the account, the
//! device, the expiry — because it is the whole of what a forwarder needs and nothing else. A
//! ticket names no conversation, carries no membership, and outlives nothing: it admits its holder
//! to a call's media socket and expires whether or not they ever dial it.
//!
//! # When this node mints nothing
//!
//! A node with no `sfu.public_url` has no media plane to name — either it runs none, or the
//! deployment has not been told where the one it runs is. A join is still answered with its TURN
//! list and its roster, a group call on such a node relays through the signalling plane exactly as
//! it always has, and the reply simply carries no `sfu`. That is not a degraded answer to hide:
//! the field's absence is how a client learns to mesh, and a node that fabricated a URL it does
//! not serve would send clients to dial nothing.
//!
//! Note which field decides that. It is `public_url` and not `bind`, because minting and binding
//! are done by two different processes: `migod` never opens the media socket, so requiring `bind`
//! here would mean a signalling node could not hand out seats on a plane running elsewhere — the
//! separation section 92 asks for, expressed as a configuration this node could not hold.

use std::sync::Arc;
use std::time::Duration;

use migo_core::config::SfuNodeConfig;
use migo_core::{Id, Timestamp};
use migo_protocol::CallSfuMedia;
use migo_sfu::Member;
use migo_sfu_node::ticket::{SfuTicket, TicketKey, TicketKeyError};

/// Mints the media plane's admissions on this node.
///
/// Constructed once by the composition root, from the one section of the configuration that both
/// processes read. A node that names no media plane builds none of these, which is why the
/// dispatcher holds an `Option`: absent is the ordinary state of a node that hands out no media
/// seats, not an error and not a switch.
pub struct SfuTickets {
    key: TicketKey,
    public_url: String,
    ttl_ms: i64,
}

impl std::fmt::Debug for SfuTickets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The key redacts itself; this type's own name in a log line is all an operator needs.
        f.debug_struct("SfuTickets")
            .field("public_url", &self.public_url)
            .field("ttl_ms", &self.ttl_ms)
            .finish_non_exhaustive()
    }
}

impl SfuTickets {
    /// Builds the minter for a node that names a media plane, or `None` for one that names none.
    ///
    /// `None` is the answer when `sfu.public_url` is unset: with no address to hand a client there
    /// is nothing to sign and nothing to say, and `sfu.ticket_key` is required to be set alongside
    /// it or unset alongside it, so the pair is never half-configured.
    ///
    /// # Errors
    ///
    /// Returns an error when this node *does* name a media plane and its ticket key is unusable.
    /// That is a deployment error: a node configured to mint tickets it cannot sign would refuse
    /// every device it admitted, at the first call rather than at startup.
    pub fn from_config(config: &SfuNodeConfig) -> Result<Option<Arc<Self>>, TicketKeyError> {
        if config.public_url.trim().is_empty() {
            return Ok(None);
        }
        let key = TicketKey::from_config(config.ticket_key.expose())?;
        Ok(Some(Arc::new(Self {
            key,
            public_url: config.public_url.clone(),
            ttl_ms: config.ticket_ttl_ms,
        })))
    }

    /// How long a minted ticket admits its holder.
    #[must_use]
    pub fn ttl(&self) -> Duration {
        Duration::from_millis(u64::try_from(self.ttl_ms).unwrap_or(0))
    }

    /// Mints the media ticket for one device's seat in one call.
    ///
    /// The claim is signed over `now + ttl`, from the caller's own clock reading rather than a
    /// second call to the clock: the expiry a client is told in `expires_at` is then the expiry
    /// that was actually signed, and a reply that named a different moment than the ticket would
    /// be a client's first bug report.
    #[must_use]
    pub fn issue(
        &self,
        call_id: Id,
        account_id: Id,
        device_id: Id,
        now: Timestamp,
    ) -> CallSfuMedia {
        let expires_at = now.saturating_add_millis(self.ttl_ms);
        let ticket = self.key.mint(&SfuTicket {
            call_id,
            member: Member {
                account_id,
                device_id,
            },
            expires_at,
        });
        CallSfuMedia {
            url: self.public_url.clone(),
            ticket,
            expires_at,
        }
    }
}
