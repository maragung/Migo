//! The media plane's own socket: a process that binds `migo-sfu` to a QUIC listener.
//!
//! # What this crate is
//!
//! [`migo_sfu`] is the decision core — who is seated, who publishes, who subscribed to whom,
//! which simulcast layer flows — and it deliberately owns no socket. Section 92 says why the
//! socket lives in a process of its own rather than inside `migod`: TURN and the SFU never touch
//! plaintext media, so their load profile is bandwidth rather than application logic, and they
//! scale on a different axis than the signalling node. This crate is that separate process. It
//! opens the listener, frames what arrives, hands the decisions to the core, and writes what the
//! core decides back out.
//!
//! # The shape of the plane
//!
//! *Control* is a QUIC bidirectional stream, one per session, carrying length-prefixed frames in
//! the same `u32` big-endian framing `migod`'s own QUIC listener uses. *Media* is one sealed frame
//! per QUIC datagram: a frame the transport loses costs the frame, and the subscriber's own
//! adaptation (section 165) is what turns that loss into a lower rung. The MWP opcode table stays
//! closed (section 146), so nothing here allocates an opcode or appears in the signalling schema —
//! this plane speaks its own codec, defined in [`wire`].
//!
//! # Admission is a ticket, not a lookup
//!
//! A connection's first control frame presents a short-lived ticket that `migod` minted when the
//! device joined the call. The media process verifies it offline with a shared HMAC key: no
//! store, no session table, no round trip to the signalling node, and nothing about the call's
//! membership to keep in sync. What the ticket carries is exactly what a forwarder needs and
//! nothing more — the call, the account, the device, the expiry — so a compromised media process
//! learns no more from its own memory than it learns from the frames it is already trusted to
//! forward, all of which are sealed.
//!
//! # What this crate is not
//!
//! *Not a decryptor.* No cryptography is linked for the media path at all: a payload enters as
//! [`migo_sfu::SealedFrame`] and leaves the same way. The one key this process holds verifies
//! admissions and cannot open a frame.
//!
//! *Not a signalling node.* It serves no MWP connection, holds no session, and answers no opcode.
//! A call's membership changes reach it only as tickets and as the frames clients send.
//!
//! ```no_run
//! # async fn run(config: &migo_core::config::SfuNodeConfig, registry: &migo_core::metrics::Registry) -> anyhow::Result<()> {
//! # let shutdown = migo_core::Shutdown::new();
//! let server = std::sync::Arc::new(migo_sfu_node::Server::new(
//!     config,
//!     std::time::Duration::from_millis(30_000),
//!     registry,
//! )?);
//! let bound = server.bind(shutdown).await?;
//! // `migosfud` then serves /metrics on `sfu.metrics_bind` for as long as it runs.
//! # let _ = bound;
//! # Ok(())
//! # }
//! ```

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod plane;
pub mod server;
pub mod ticket;
pub mod wire;

pub use plane::{Outbound, Plane, Session, SessionId};
pub use server::{serve_metrics, ReadError, Server};
pub use ticket::{SfuTicket, TicketError, TicketKey, TicketKeyError, TOKEN_LEN};
pub use wire::{Reply, Request, WireError};
