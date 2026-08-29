//! Identity for Migo.
//!
//! Registration, sign-in, session tokens, refresh rotation, and revocation. Brief
//! sections 46, 47, 79, and 119 describe what this crate owes the rest of the server: an
//! [`Identity`] that a request handler can trust without asking any further questions, and
//! a way to take one away that takes effect on the next request rather than at the next
//! token expiry.
//!
//! ```no_run
//! use std::sync::Arc;
//!
//! use migo_auth::{Authenticator, DeviceClaim, Registration, RequestContext};
//! use migo_core::{Secret, Timestamp};
//! use migo_protocol::Platform;
//!
//! # async fn example(auth: migo_auth::SharedAuth) -> migo_core::Result<()> {
//! let context = RequestContext::at(Timestamp::now()).from_ip("203.0.113.7".parse().unwrap());
//! let grant = auth
//!     .register(
//!         Registration {
//!             username: "ada".to_string(),
//!             email: None,
//!             phone: None,
//!             password: Secret::new("correct horse battery staple"),
//!             locale: "en".to_string(),
//!             country: None,
//!             device: DeviceClaim::new(Platform::Web, "Firefox on Linux"),
//!             captcha: None,
//!             server: None,
//!         },
//!         &context,
//!     )
//!     .await?;
//!
//! let identity = auth
//!     .authenticate(&grant.access_token, grant.device_id, &context)
//!     .await?;
//! assert_eq!(identity.account_id(), grant.account_id);
//! # Ok(())
//! # }
//! ```
//!
//! # The two halves of a session
//!
//! An **access token** is a signed, self-describing, short-lived value ([`token`]). It is
//! verified with a MAC and nothing else — no database read, no cache read — so the hot
//! path costs one hash. That is the whole reason it exists, and it buys one problem: for
//! its lifetime it is valid whatever else happens, which is why the lifetime is fifteen
//! minutes rather than a day.
//!
//! A **refresh token** is thirty-two opaque random bytes whose keyed tag is a row in the
//! database. Exchanging it mints a new pair and marks the old row rotated. Presenting a
//! rotated row is not a mistake that can happen by accident: it means two parties hold
//! the same token, which means one of them copied it, so the entire family is revoked and
//! every device on it has to sign in again. That is the trade — a rare and annoying
//! logout in exchange for a stolen refresh token being worth minutes rather than a month.
//!
//! # What is deliberately not here
//!
//! *No password reset.* Recovery is brief section 106 and it is not an authentication
//! problem: it is a deliverability problem, a rate-limiting problem, and an
//! account-takeover problem wearing an authentication costume. It gets its own crate with
//! its own audit trail.
//!
//! *No multi-factor enrolment.* [`migo_protocol::codes::MFA_REQUIRED`] exists and this
//! crate will return it once there is something to enrol into. Shipping half of MFA —
//! a code that can be demanded but not registered — would be worse than shipping none.
//!
//! *No sessions for bots.* A bot presents a bot token, and turning that into a session is
//! `migo-bots`' job, because the checks are different: no password, no device, no
//! presence, a token that an owner can rotate, and a much larger bucket.
//!
//! *No key material in this crate's state beyond the signing key.* Private keys are
//! generated on the device and never sent to the server (brief section 47), so there is
//! nothing here to protect on a user's behalf except the hash of their password and the
//! tags of their refresh tokens — both of which are one-way.

#![forbid(unsafe_code)]
#![warn(missing_docs, clippy::all)]

pub mod capability;
pub mod captcha;
pub mod credential;
pub mod endpoint;
pub mod metrics;
pub mod model;
pub mod service;
pub mod tier;
pub mod token;
pub mod traits;

pub use crate::capability::Capabilities;
pub use crate::endpoint::{
    is_loopback_host, QuicScheme, RestScheme, Scheme, ServerEndpoint, Transport, WsScheme,
};
pub use crate::model::{
    DeviceClaim, Grant, Refresh, Registration, RequestContext, SessionSummary, SignIn,
};
pub use crate::service::{open, Auth, ConcreteAuth, SharedAuth};
pub use crate::tier::{of_account as tier_of_account, PROBATION_MILLIS, TRUSTED_MILLIS};
pub use crate::token::{Claims, Signer, REFRESH_BYTES, TOKEN_BYTES, TOKEN_VERSION};
pub use crate::traits::{Authenticator, Identity, PasswordChange, REAUTH_WINDOW_MS};
pub use migo_captcha::CaptchaProof;
