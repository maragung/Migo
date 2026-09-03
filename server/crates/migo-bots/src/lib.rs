//! Bots — the automation accounts of brief sections 40 to 42, 77 and 145 — and the three
//! things this crate is responsible for: registering one, authenticating the token it
//! presents, and bounding what it may do.
//!
//! # A bot is an account, not a parallel identity
//!
//! The mistake this crate refuses to make is inventing a second kind of principal. A bot
//! posts messages, joins conversations, is reported, and can be moderated — every one of
//! those is a thing the rest of the system already knows how to do *to an account*. So a
//! bot **is** an account: [`register`](service::open) creates a backing `account` row, its
//! profile, and the `bot` row that ties them together, in one store write, and from then on
//! the bot speaks through `bot.account_id` exactly as a human speaks through theirs. The
//! messaging layer, the moderation layer, and the social graph never learn that the account
//! belongs to a bot; they do not need to, and a special case they do not have is a special
//! case that cannot be got wrong.
//!
//! # The backing account can never be signed into
//!
//! A bot authenticates with a bearer token, never a passphrase — but `account.passphrase_hash`
//! is `NOT NULL`, and leaving it blank or filling it with a known sentinel would be a
//! passphrase anyone who read the schema could supply. So the account is given a *valid*
//! Argon2id hash of thirty-two random bytes that are computed once, used, and thrown away
//! ([`service::open`]). Nothing was kept that hashes to it, so no passphrase verifies against
//! it — not a blank, not a guess, not the sentinel a reader of this code might try. This is
//! the same move `migo-auth` makes for an absent account, for the same reason: an
//! unusable-but-valid hash is safer than a nullable column, because there is no "no passphrase
//! set" branch for a caller to reach.
//!
//! # The token is a lookup key, stored only as a keyed tag
//!
//! A bot token is thirty-two bytes of randomness, handed to the owner once in
//! base64url and never again. What the store keeps is not the token but a keyed
//! HMAC-SHA-256 tag of it ([`token`]): a database dump yields no working credential, and
//! because the tag is keyed rather than a bare hash, an attacker who has the dump but not
//! the deployment's key cannot even confirm a guess offline. The raw token is a
//! [`migo_core::Secret`], whose `Debug` and serialization both redact — brief sections 77
//! and 145 forbid a bot token, raw or hashed, from ever reaching a log, and this crate holds
//! to that by never formatting either.
//!
//! # Scopes: minimum by default
//!
//! What a bot may do is a [`Scopes`] bitmask over the six permissions of section 41 — read
//! messages, send messages, moderate, manage games, read the member list, send
//! announcements — and a freshly registered bot is granted [`Scopes::NONE`]. Section 41 is
//! explicit that the default must be the minimum, so the default is *nothing*: an owner
//! grants each capability deliberately, and a bot that was never granted a scope cannot use
//! it. The gateway checks the scope before dispatching; this crate defines what the bits
//! mean and hands the authenticated set back.
//!
//! # What this crate will not do
//!
//! It never hands a bot a store handle. Section 42 is that a bot has no direct database
//! access, and the shape of this crate enforces it: a bot receives a token and, through the
//! gateway, an account identity — never a [`migo_store`] object. The sandbox limits section
//! 42 also asks for — CPU, memory, network, request and message rates — are the rate
//! limiter's and the gateway's to enforce; a bot is priced at [`migo_ratelimit::TrustTier::Bot`],
//! which carries its own bucket. And it cannot mint anything but a token: there is no method
//! here that moves money, changes another account, or reads a conversation's contents.
//!
//! # Getting one
//!
//! ```ignore
//! let bots = migo_bots::open(store, limiter, BotsConfig::default(), &token_key, &registry)?;
//! let registered = bots.register(&owner, NewBotSpec {
//!     username: "weather".into(),
//!     display_name: "Weather".into(),
//!     scopes: Scopes::SEND_MESSAGES.with(Scopes::READ_MESSAGES),
//!     webhook_url: Some("https://example.test/hook".into()),
//!     locale: None,
//! }).await?;
//! // registered.token is shown to the owner exactly once.
//! let identity = bots.authenticate(registered.token.expose(), now).await?;
//! assert!(identity.may(Scopes::SEND_MESSAGES));
//! ```

#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![deny(clippy::all)]
#![warn(clippy::pedantic)]

mod metrics;
pub mod model;
pub mod service;
pub mod token;
pub mod traits;

pub use crate::model::{BotIdentity, BotView, BotsConfig, Caller, NewBotSpec, Registered, Scopes};
pub use crate::service::{open, BotService};
pub use crate::traits::{Bots, SharedBots, SharedWebhook};
