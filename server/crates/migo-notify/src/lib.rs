//! The notification inbox, and push as a wake-up that cannot carry a message.
//!
//! Brief section 44 lists fourteen kinds of notification and then constrains all of them
//! with one sentence: *"Payload push TIDAK BOLEH memuat plaintext pesan, plaintext audio
//! voice note, atau isi signaling."* Section 77 says the same thing three more ways —
//! *"Push payload harus minimum"*, *"Push tidak berisi plaintext message"*, *"Gunakan
//! generic notification: 'New message'"* — and adds that the token itself is stored hashed
//! and never logged.
//!
//! This crate is what those sentences look like when they are types instead of comments.
//!
//! # The payload cannot hold a message
//!
//! [`Wakeup`] has five fields: a kind, two optional [`Id`](migo_core::Id)s, a `u32` badge,
//! and a timestamp. There is no `title`, no `body`, no `String`, and no `Vec<u8>`. The only
//! text that leaves this crate comes from [`Wakeup::alert`], which returns `&'static str`
//! from a match on the kind — fifteen fixed sentences, chosen at compile time.
//!
//! So the rule is not enforced by review. An author who wants the message preview in the
//! push has to add a field to a public struct and change a function's return type, which
//! is a diff a reviewer sees. Filling in a `body: Option<String>` that was already there
//! is not.
//!
//! The same shape answers the harder half of section 44. An incoming call *needs* to reach
//! a sleeping phone, and the brief allows exactly `call_id` plus a marker: *"Tidak ada SDP,
//! ICE candidate, atau isi signaling di dalam push."* Here that is
//! [`NotificationKind::IncomingCall`](migo_protocol::NotificationKind::IncomingCall) with
//! the call id in `subject_id`. An SDP offer does not fit in an `Option<Id>`.
//!
//! # The token is stored where it cannot be read
//!
//! Section 77 says push tokens are stored hashed. Taken literally that would be a token
//! nobody can push to, so the credential is split in two, and [`token`] holds the whole of
//! it:
//!
//! - **Sealed** with a key derived from the deployment secret, device-bound by using the
//!   device id as associated data. A dump of the `device` table is not a set of push
//!   credentials, and moving one row's ciphertext to another row's `push_token` column
//!   yields something that will not open.
//! - **Hashed** with an independent HKDF label. This is the handle: every lookup, every
//!   deduplication, and every log line and metric that has to mention a registration
//!   mentions the hash. That is what makes *never log the token* a rule somebody can follow
//!   while still debugging delivery.
//!
//! [`RawToken`] exists to mark the boundary. It has a hand-written `Debug` that prints a
//! length, it is dropped inside [`Notifier::register`], and the raw string reaches
//! `migo-store` never.
//!
//! # Not every kind is a row
//!
//! Six of section 44's fourteen kinds are deliberately not stored, and the schema comment
//! on `notification` gives the test: *does answering it make it go away by itself?* An
//! unread message does — `conversation_cursor` already holds `last_seq` and `read_seq`, and
//! a row per message would be that same count kept in a second place, disagreeing with the
//! first by the end of the week. A pending friend request does; the `relationship` row *is*
//! the inbox item. A ringing call does, by becoming a missed call, which is a row.
//!
//! A gift, a level up, a badge, a room invitation, an announcement, an event, a game
//! challenge, a missed call: each one happened, and nothing else records whether the person
//! it happened to has seen it. Those are the eight kinds this table exists for, and
//! [`migo_store::model::notification_kind::is_storable`] is where the list lives — checked
//! by both storage backends, so it is enforced rather than remembered.
//!
//! # A wake-up withheld is not a failure
//!
//! Four things stop a push, and none of them is an error:
//!
//! | Reason | What it means |
//! |---|---|
//! | [`Withheld::Connected`] | The device has a live socket and already has the event |
//! | [`Withheld::Coalesced`] | It was woken for this kind moments ago |
//! | [`Withheld::Budget`] | The device's wake-up bucket is spent |
//! | [`Withheld::Stale`] | The registration is older than the deployment trusts |
//!
//! All four are counted, returned in [`Delivery`], and reported `Ok`. Connected is the most
//! common of them, which is the system working as designed: section 44 also says *"Jangan
//! mengirim push notification untuk setiap event kecil."* A caller that received
//! `RATE_LIMITED` from a gift would have no correct way to respond to it — the gift arrived,
//! the row is written, the badge is right.
//!
//! [`Failure`] is the other axis, and it is the one worth an alert: a provider that rejected
//! the token, throttled the sender, or errored. A token the provider calls dead is retired
//! on the spot.
//!
//! # What this crate is not allowed to know
//!
//! Who should be told. A room announcement reaches members because `migo-rooms` decided so;
//! a gift notification reaches its recipient because `migo-economy` posted the transaction.
//! There is no membership read and no social-graph read here — two layer-3 crates that
//! depend on each other are how a dependency graph becomes a cycle, and the recipient list
//! is part of an authorisation decision that belongs with the crate that made it.
//!
//! Live-socket state is the one thing this crate does look up, and it asks
//! [`RoutingCache`](migo_cache::traits::RoutingCache) in layer 2 rather than `migo-presence` in
//! layer 3, for exactly that reason.
//!
//! It also does not link an FCM or APNs SDK. [`PushSender`] is a port; the composition root
//! implements it, the same way `migo-media` takes object storage as a port and
//! `migo-moderation` takes the staff roster as one. [`NoPush`] is the implementation for a
//! deployment with no push service and for tests.
//!
//! # Getting one
//!
//! ```ignore
//! let notifier = migo_notify::open(
//!     store,
//!     cache,
//!     limiter,
//!     Arc::new(FirebaseSender::new(credentials)),
//!     Box::new(OsRandom),
//!     config.signing_secret.expose().as_bytes(),
//!     NotifyConfig::default(),
//!     &registry,
//! );
//!
//! // migo-economy, having posted the transaction:
//! notifier.notify(Event::new(recipient, NotificationKind::Gift, now).by(giver)).await?;
//! ```

#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![deny(missing_debug_implementations)]
#![deny(clippy::all)]
#![warn(clippy::pedantic)]

mod metrics;
pub mod model;
pub mod service;
pub mod token;
pub mod traits;

pub use crate::model::{
    Caller, Delivery, Event, Failure, Inbox, Item, NotifyConfig, RawToken, Wakeup, Withheld,
    COALESCE_WINDOW_MS, MAX_INBOX_PAGE, MAX_TOKEN_LEN, REGISTRATION_TTL_MS,
};
pub use crate::service::{open, Notifications};
pub use crate::token::TokenKeeper;
pub use crate::traits::{
    NoPush, Notifier, PushSender, Sent, SharedNotifier, SharedPushSender, Target,
};
