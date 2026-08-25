//! The port to the push providers, and what this crate offers the layer above.
//!
//! # Why sending is a trait and not an FCM client
//!
//! The same reason `migo-media` does not link an S3 SDK. A push provider is an HTTP
//! API with an OAuth flow, a JWT, a certificate, a rate limiter, and a set of error
//! codes that change without notice, and none of that is notification *logic*. What
//! is logic is: who should be told, whether they are already looking, whether they
//! were told ten seconds ago, and what the payload may contain. That is what lives
//! here.
//!
//! It also keeps the provider credentials in the composition root beside every other
//! credential, and lets the deterministic simulator run the whole notification path
//! with a sender that records instead of sends.
//!
//! # The one thing [`PushSender`] is never given
//!
//! There is no method that takes a message, a body, or a byte slice. The only payload
//! it can be handed is a [`Wakeup`], which structurally cannot hold a sentence. An
//! implementation is free to render [`Wakeup::alert`] into a provider's `notification`
//! field; it has nothing else to render, because it was given nothing else.

use async_trait::async_trait;
use migo_core::{Id, Result, Timestamp};
use migo_protocol::Platform;

use crate::model::{Caller, Delivery, Event, Inbox, RawToken, Wakeup};

/// One device to wake, with its token already opened.
///
/// The token is a plain `&str` and it is borrowed, both on purpose. Borrowed, because
/// an owned `String` here would be a copy of a credential with a lifetime nobody is
/// tracking. Plain, because the implementation is about to put it in an HTTP request
/// and a wrapper it has to unwrap first buys nothing at the point of use.
///
/// What protects it is that this struct exists only for the duration of one
/// [`PushSender::send`] call, is never stored, and is never `Debug` — deriving `Debug`
/// on it would put a live push credential one `tracing::debug!` away from a log file,
/// which brief section 174 forbids without exception.
#[allow(missing_debug_implementations)]
pub struct Target<'a> {
    /// Which device, so a failure can be attributed and the registration retired.
    pub device_id: Id,
    /// What the client said it is, which decides the payload shape.
    pub platform: Platform,
    /// Which service to talk to, numbered by `migo_store::model::PushProvider`.
    pub provider: i16,
    /// The provider's token for this device.
    pub token: &'a str,
}

/// What a provider said.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sent {
    /// Accepted for delivery. Not the same as arrived, and no provider offers that.
    Delivered,
    /// The token is dead. The caller retires the registration.
    ///
    /// This is the one outcome that changes stored state, which is why it is a
    /// distinct value rather than an error: an app that was uninstalled is a normal
    /// event, and treating it as a failure means the deployment pays to discover it
    /// again on every notification for the rest of the account's life.
    Unregistered,
    /// The provider is refusing traffic right now. The token is fine; try later.
    Throttled,
}

/// A push provider, as this crate needs it.
///
/// Implemented once per provider in the composition root. One method, because there
/// is one thing to do.
#[async_trait]
pub trait PushSender: Send + Sync {
    /// Wakes one device.
    ///
    /// An `Err` means the attempt failed in a way the provider did not describe — a
    /// network error, a malformed response, an expired credential. It must not be used
    /// for "the token is dead", which is [`Sent::Unregistered`] and is not a failure.
    async fn send(&self, target: Target<'_>, wakeup: &Wakeup) -> Result<Sent>;

    /// Whether this sender handles a given provider number.
    ///
    /// The composition root may register one sender per provider or one that speaks
    /// all of them; this is how the service finds out which without knowing either
    /// arrangement.
    fn handles(&self, provider: i16) -> bool;
}

/// A sender that accepts everything and does nothing.
///
/// The default when a deployment has configured no provider, which is the normal state
/// of a development machine. It reports [`Sent::Delivered`] rather than an error
/// because a missing push provider is a missing feature and not a broken one: an
/// account with no push still gets its inbox, its badge, and every event on its live
/// socket.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoPush;

#[async_trait]
impl PushSender for NoPush {
    async fn send(&self, _target: Target<'_>, _wakeup: &Wakeup) -> Result<Sent> {
        Ok(Sent::Delivered)
    }

    fn handles(&self, _provider: i16) -> bool {
        true
    }
}

/// The notification service, erased.
///
/// Every method that a client can reach takes a [`Caller`]. [`Notifier::notify`] does
/// not, and that asymmetry is the shape of the crate: notifying is something the server
/// does to somebody, on behalf of an event that has already been authorised by the
/// crate that raised it. A client cannot ask for a notification to be sent to another
/// account, because there is no method with which to ask.
#[async_trait]
pub trait Notifier: Send + Sync {
    /// Tells somebody that something happened.
    ///
    /// Stores it if the kind has nowhere else to live, wakes whichever of their devices
    /// is asleep and has not been woken for this kind recently, and returns what it
    /// did. Errors only on a storage failure; a provider that refused, a device that
    /// was already connected, and a budget that was spent are all outcomes in
    /// [`Delivery`], not errors — a gift that failed to buzz is still a gift that
    /// arrived.
    async fn notify(&self, event: Event) -> Result<Delivery>;

    /// Tells several people about the same thing.
    ///
    /// One call rather than a loop at the call site, because a room announcement to
    /// four thousand members is the shape this crate has to survive, and a caller
    /// looping would give it no chance to say so. Recipients are notified in order and
    /// a failure on one does not stop the rest.
    async fn notify_many(&self, recipients: &[Id], event: Event) -> Result<Delivery>;

    /// One page of the caller's inbox, newest first, with the unread count.
    async fn inbox(&self, caller: &Caller, limit: u16) -> Result<Inbox>;

    /// The caller's unread count on its own.
    ///
    /// Separate from [`Notifier::inbox`] because it is called far more often — every
    /// app foreground — and answering it costs one indexed count rather than a page of
    /// rows.
    async fn badge(&self, caller: &Caller) -> Result<u32>;

    /// Marks everything up to `through` as read, returning how many changed.
    ///
    /// A watermark rather than a list of ids: the client's gesture is "I have opened
    /// the bell", and a list would race with anything that arrived while the request
    /// was in flight.
    async fn acknowledge(&self, caller: &Caller, through: Timestamp) -> Result<u32>;

    /// Records the calling device's push registration.
    ///
    /// The token is sealed and hashed here; nothing downstream ever sees it. Takes the
    /// registration from any other device holding the same token, because the same
    /// token on two device rows means one phone getting two of everything.
    async fn register(&self, caller: &Caller, token: RawToken) -> Result<()>;

    /// Forgets the calling device's registration.
    ///
    /// Called on sign-out. A device that is signed out and still registered is a phone
    /// that keeps buzzing for an account somebody deliberately left.
    async fn unregister(&self, caller: &Caller) -> Result<()>;

    /// Deletes read notifications older than `before`, up to `limit` rows, returning
    /// how many went.
    ///
    /// Run by the maintenance job, not by a request. The caller loops until it returns
    /// zero.
    async fn sweep(&self, before: Timestamp, limit: u16) -> Result<u64>;
}

/// The service, shared.
pub type SharedNotifier = std::sync::Arc<dyn Notifier>;

/// A push sender, shared.
pub type SharedPushSender = std::sync::Arc<dyn PushSender>;
