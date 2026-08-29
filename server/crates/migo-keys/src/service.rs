//! The key service.
//!
//! # The four rules, and where each one lives
//!
//! **The signature must verify.** [`Keyring::publish`] parses the identity key into a pair
//! of curve points and asks `migo_crypto` whether the signed prekey's signature was made
//! by it. Section 163 makes the refusal `INVALID_KEY_MATERIAL`. The check is `migo_crypto`'s
//! and not this file's -- the domain separator, the byte order of the signed message, and
//! the small-order point list are all decided there, because a second implementation of
//! "what a prekey signature covers" is how two implementations come to disagree.
//!
//! **An expired signed prekey is refused on arrival.** A prekey that is already dead when
//! it is published can only produce a session that fails later, and "later" is a moment
//! nobody will connect back to this one.
//!
//! **One one-time prekey per fetch, never twice.** The store consumes it inside the same
//! call that returns the bundle, so there is no window in which this crate holds a prekey
//! it has not yet marked used. That is why [`Keyring::bundles`] has no read-then-write and
//! no lock: the atomicity is the store's, which is the only layer that can actually
//! provide it.
//!
//! **Revoked devices are never served.** Also the store's, applied in the query. This
//! crate could not skip it if it wanted to, which is the point -- a filter a caller has to
//! remember is a filter that gets forgotten in the second caller.
//!
//! # What publishing replaces, and what that means for a client
//!
//! Publishing is a **replace**, including the one-time prekeys, matching both store
//! backends. That is the right semantics and not an implementation detail leaking through:
//! a device publishing is declaring what its current key material *is*, and the one thing
//! that must never happen is the server handing out a prekey whose private half the device
//! no longer holds. A reinstalled client has lost every old private key; a merge would
//! leave the server serving those for weeks, and every session formed from one would be
//! undecryptable by the recipient.
//!
//! So a top-up is a publication of a fresh batch under fresh key ids, and a client keeps
//! the private halves of the batch it just retired until any message already in flight has
//! landed. [`PublishOutcome::one_time_prekeys_remaining`] tells it what the server now
//! holds so it can decide when to do that again;
//! [`ONE_TIME_PREKEY_LOW_WATER`](crate::model::ONE_TIME_PREKEY_LOW_WATER) is the threshold
//! the clients use.
//!
//! # Where the prices come from
//!
//! The IDL: `KEY_PUBLISH` 20 and `KEY_BUNDLE_FETCH` 5, brief section 145. Both are charged
//! through `charge_opcode` rather than through a local constant, so repricing them is an
//! edit to the protocol and not an edit to this file. Publishing is the more expensive of
//! the two because it writes, and because the natural abuse of it -- republishing in a loop
//! to churn a device's key material -- costs the server a transaction each time.
//!
//! Two buckets on each: the endpoint under the account, and the account itself. The device
//! is deliberately *not* a bucket. A per-device budget would let an account with forty
//! devices publish forty times as often, and key churn is an account-level concern: it is
//! the account's contacts whose sessions break.

use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;
use migo_core::metrics::Registry;
use migo_core::{Id, Result};
use migo_crypto::{IdentityPublic, SignedPrekey, PUBLIC_KEY_LEN, SIGNATURE_LEN};
use migo_protocol::{codes, fault, Opcode};
use migo_ratelimit::{BucketKey, RateLimiter, SharedRateLimiter};
use migo_store::model::{KeyBundle as StoredBundle, PublishedKeys};
use migo_store::traits::KeyStore;
use migo_store::{SharedStore, Store};

use crate::metrics::{Meters, PublishRejection};
use crate::model::{
    Bundle, Caller, Fetched, KeysConfig, PublishOutcome, PublishRequest, MAX_BUNDLES_PER_FETCH,
    MAX_ONE_TIME_PREKEYS,
};
use crate::traits::Keyring;

/// A shared, fully erased key service.
pub type SharedKeyring = Arc<dyn Keyring>;

/// The key service over a store and a rate limiter.
///
/// No cache. Every read here consumes something -- a bundle fetch takes a one-time prekey
/// -- so there is nothing cacheable on the read path, and a cached identity key would be
/// the one thing in the system where a stale value means a sender encrypting to a key the
/// device has rotated away from.
///
/// No `Random`. This crate mints no key material and never will; section 163 forbids the
/// server from holding a private key, and a random source here would be the first thing a
/// future edit reached for on the way to breaking that.
pub struct Keys<S: ?Sized = dyn Store, L: ?Sized = dyn RateLimiter> {
    store: Arc<S>,
    limiter: Arc<L>,
    config: KeysConfig,
    meters: Meters,
}

/// Builds the key service the composition root hands around.
#[must_use]
pub fn open(
    store: SharedStore,
    limiter: SharedRateLimiter,
    registry: &Registry,
    config: KeysConfig,
) -> SharedKeyring {
    Arc::new(Keys::new(store, limiter, registry, config))
}

impl<S, L> Keys<S, L>
where
    S: KeyStore + ?Sized,
    L: RateLimiter + ?Sized,
{
    /// Assembles the service and registers every series at zero.
    pub fn new(store: Arc<S>, limiter: Arc<L>, registry: &Registry, config: KeysConfig) -> Self {
        Self {
            store,
            limiter,
            config,
            meters: Meters::new(registry),
        }
    }

    /// Charges an operation, priced from the IDL.
    async fn charge(&self, caller: &Caller, opcode: Opcode) -> Result<()> {
        let keys = [
            BucketKey::endpoint_write_of_account(caller.account_id, opcode),
            BucketKey::account(caller.account_id),
        ];
        self.limiter
            .charge_opcode(&keys, opcode, caller.tier, caller.now)
            .await?
            .into_result()
    }

    /// Refuses a caller that is not fully identified.
    ///
    /// Both halves matter here in a way they do not everywhere else: the device id is what
    /// the key material is filed under, so a nil one would publish an identity for a
    /// device that does not exist and a fetch would consume prekeys from nowhere.
    fn require_identity(caller: &Caller) -> Result<()> {
        if caller.account_id.is_nil() || caller.device_id.is_nil() {
            return Err(fault::unauthenticated(
                "publishing key material needs an identified account and device",
            ));
        }
        Ok(())
    }

    /// The refusal section 163 names for key material that does not hold up.
    ///
    /// Never carries the bytes it refused. These errors are produced while processing
    /// attacker-supplied input and they end up in logs; section 174 keeps key material out
    /// of a log line, and there is no diagnostic value in the bytes anyway -- a client
    /// whose signature does not verify has a bug in its signing code, not in its copy of
    /// this particular key.
    fn bad_material(&self, reason: PublishRejection, internal: &'static str) -> migo_core::Error {
        self.meters.rejected(reason);
        fault::error(codes::INVALID_KEY_MATERIAL, internal)
    }

    /// A length that is structurally wrong.
    ///
    /// `VALIDATION_FAILED` and not `INVALID_KEY_MATERIAL`, because a 31-byte key is a
    /// client that built the frame wrong rather than one whose crypto is wrong, and the two
    /// want different bug reports. The field name is in the error so the report says which.
    fn bad_length(&self, field: &'static str, expected: usize, actual: usize) -> migo_core::Error {
        self.meters.rejected(PublishRejection::Malformed);
        // A length is not key material, so it can safely be in the message. It is also the
        // one number that turns "the server refused my key" into a fixable bug report.
        fault::validation(field, &format!("must be {expected} bytes, got {actual}"))
    }

    /// Narrows a client-supplied key id to what the store's column can hold.
    ///
    /// The wire says `u32` and the schema says `i32`. Refusing the top half of the range is
    /// better than wrapping it: a wrapped id is a prekey the client thinks it published
    /// under one number and the server serves under another, which surfaces much later as
    /// a session that will not open.
    fn key_id(&self, field: &'static str, value: u32) -> Result<i32> {
        i32::try_from(value).map_err(|_| {
            self.meters.rejected(PublishRejection::Malformed);
            fault::validation(field, "key id is out of range")
        })
    }

    /// Projects a stored bundle into the domain type.
    fn bundle_of(stored: StoredBundle) -> Result<Bundle> {
        Ok(Bundle {
            account_id: stored.account_id,
            device_id: stored.device_id,
            identity_key: stored.identity_key,
            // The store's ids are non-negative by construction: every one of them was
            // narrowed from a `u32` by `key_id` on the way in. A negative value here is a
            // corrupted row rather than a client's fault, so it is an internal error.
            signed_prekey_id: u32::try_from(stored.signed_prekey_id)
                .map_err(|_| fault::internal("stored signed prekey id is negative"))?,
            signed_prekey: stored.signed_prekey,
            signed_prekey_signature: stored.signed_prekey_signature,
            signed_prekey_expires_at: stored.signed_prekey_expires_at,
            one_time_prekey: match stored.one_time_prekey {
                Some((id, key)) => Some((
                    u32::try_from(id)
                        .map_err(|_| fault::internal("stored one-time prekey id is negative"))?,
                    key,
                )),
                None => None,
            },
        })
    }
}

#[async_trait]
impl<S, L> Keyring for Keys<S, L>
where
    S: KeyStore + ?Sized + Send + Sync,
    L: RateLimiter + ?Sized + Send + Sync,
{
    async fn publish(&self, caller: &Caller, request: PublishRequest) -> Result<PublishOutcome> {
        Self::require_identity(caller)?;

        // Structure first, then cryptography, then the clock. Cheapest refusal first is
        // not the reason for the order -- the reason is that a signature check on a
        // wrong-length key is a check whose failure would be reported as a bad signature
        // when the real fault is a malformed frame.
        if request.signed_prekey.len() != PUBLIC_KEY_LEN {
            return Err(self.bad_length(
                "signed_prekey",
                PUBLIC_KEY_LEN,
                request.signed_prekey.len(),
            ));
        }
        if request.signed_prekey_signature.len() != SIGNATURE_LEN {
            return Err(self.bad_length(
                "signed_prekey_signature",
                SIGNATURE_LEN,
                request.signed_prekey_signature.len(),
            ));
        }
        if request.one_time_prekeys.len() > MAX_ONE_TIME_PREKEYS {
            self.meters.rejected(PublishRejection::Malformed);
            return Err(fault::validation(
                "one_time_prekeys",
                "too many one-time prekeys in one publication",
            ));
        }

        // Parses *and* validates: `IdentityPublic::parse` rejects a signing key that is not
        // on the curve and an exchange key that is a small-order point. Doing it here, at
        // publication, rather than at first use, is what keeps an unusable identity from
        // being stored and only failing months later when somebody tries to message it.
        let identity = IdentityPublic::parse(&request.identity_key).map_err(|_| {
            self.bad_material(
                PublishRejection::BadIdentity,
                "identity key is not a usable pair of public keys",
            )
        })?;

        let mut signed_prekey = [0u8; PUBLIC_KEY_LEN];
        signed_prekey.copy_from_slice(&request.signed_prekey);
        let mut signature = [0u8; SIGNATURE_LEN];
        signature.copy_from_slice(&request.signed_prekey_signature);
        SignedPrekey {
            key_id: request.signed_prekey_id,
            public_key: signed_prekey,
            signature,
        }
        .verify(&identity)
        .map_err(|_| {
            self.bad_material(
                PublishRejection::BadSignature,
                "signed prekey signature does not verify against the identity key",
            )
        })?;

        // Section 163: refused on arrival, and as `INVALID_KEY_MATERIAL` rather than the
        // `VALIDATION_FAILED` the store would give for the same condition. Both stores
        // check it too; that is a backstop for a caller that is not this one, not a
        // duplicate of this line.
        if !request
            .signed_prekey_expires_at
            .is_at_or_after(caller.now.saturating_add_millis(1))
        {
            return Err(self.bad_material(
                PublishRejection::Expired,
                "signed prekey is already expired at publication",
            ));
        }

        // Every one-time prekey is a public key too, and an invalid one is a prekey that
        // produces an all-zero shared secret for whoever is handed it. Checked here for
        // the same reason as the identity key: the alternative is a session that cannot be
        // opened and no way to trace it back.
        let signed_prekey_id = self.key_id("signed_prekey_id", request.signed_prekey_id)?;
        let mut seen: HashSet<i32> = HashSet::with_capacity(request.one_time_prekeys.len());
        let mut prekeys: Vec<(i32, Vec<u8>)> = Vec::with_capacity(request.one_time_prekeys.len());
        let mut skipped: u32 = 0;
        for (key_id, public_key) in request.one_time_prekeys {
            if public_key.len() != PUBLIC_KEY_LEN {
                return Err(self.bad_length("one_time_prekeys", PUBLIC_KEY_LEN, public_key.len()));
            }
            let mut bytes = [0u8; PUBLIC_KEY_LEN];
            bytes.copy_from_slice(&public_key);
            // `IdentityPublic::parse` is the only public door onto the small-order check,
            // so the prekey is checked as the exchange half of a throwaway pair. The
            // signing half is this device's real signing key, which is already known good.
            let mut probe = Vec::with_capacity(PUBLIC_KEY_LEN * 2);
            probe.extend_from_slice(&identity.signing);
            probe.extend_from_slice(&bytes);
            if IdentityPublic::parse(&probe).is_err() {
                return Err(self.bad_material(
                    PublishRejection::BadPrekey,
                    "a one-time prekey is not a usable public key",
                ));
            }
            let narrowed = self.key_id("one_time_prekeys", key_id)?;
            // A repeated id inside one publication is skipped rather than refused: the
            // publication as a whole is still coherent, and refusing it would make a
            // client with one duplicated id unable to publish at all.
            if seen.insert(narrowed) {
                prekeys.push((narrowed, public_key));
            } else {
                skipped += 1;
            }
        }

        let accepted = u32::try_from(prekeys.len()).unwrap_or(u32::MAX);
        self.store
            .publish_keys(PublishedKeys {
                account_id: caller.account_id,
                // The caller's device, never a field from the request. See `Caller`.
                device_id: caller.device_id,
                identity_key: identity.to_bytes().to_vec(),
                signed_prekey_id,
                signed_prekey: request.signed_prekey,
                signed_prekey_signature: request.signed_prekey_signature,
                signed_prekey_expires_at: request.signed_prekey_expires_at,
                one_time_prekeys: prekeys,
                created_at: caller.now,
            })
            .await?;

        // Charged after the write, unlike everywhere else in this codebase, and
        // deliberately: a publication that the store refused -- an unknown device, a
        // constraint -- should not spend a budget twenty times the price of a fetch. The
        // work that a limiter exists to bound is the write, and it has happened by here.
        // A caller cannot exploit the order: the store refuses the same request every
        // time, so the uncharged path is one that achieves nothing.
        self.charge(caller, Opcode::KeyPublish).await?;

        let remaining = self
            .store
            .one_time_prekey_count(caller.account_id, caller.device_id)
            .await?;
        self.meters.published(accepted, skipped);
        Ok(PublishOutcome {
            accepted_prekeys: accepted,
            identity_fingerprint: hex_of(&identity.fingerprint()),
            one_time_prekeys_remaining: remaining,
        })
    }

    async fn bundles(
        &self,
        caller: &Caller,
        user_id: Id,
        device_id: Option<Id>,
    ) -> Result<Fetched> {
        Self::require_identity(caller)?;
        if user_id.is_nil() {
            return Err(fault::field_required("user_id"));
        }
        if device_id.is_some_and(|id| id.is_nil()) {
            return Err(fault::field_required("device_id"));
        }

        // Charged before the read, unlike `publish`, because here the read *is* the
        // side effect: every bundle returned consumes a one-time prekey, so an uncharged
        // fetch would let one caller drain a device's prekeys for free.
        self.charge(caller, Opcode::KeyBundleFetch).await?;

        // No block check and no privacy gate. That is not an omission: a public key is
        // public, section 163 makes the bundle the thing a sender needs before it can send
        // anything at all, and a fetch refused on social grounds would be a second, weaker
        // copy of the gate that `migo_social` already applies to the send itself. The one
        // thing this does leak is that an account has published keys, which is true of
        // every account that has ever opened the app.
        let stored = match device_id {
            Some(device_id) => self
                .store
                .take_key_bundle(user_id, device_id)
                .await?
                .into_iter()
                .collect::<Vec<_>>(),
            None => self.store.take_key_bundles_for_account(user_id).await?,
        };
        if stored.len() > MAX_BUNDLES_PER_FETCH {
            // Not a refusal of the request: the bundles have already been taken and their
            // one-time prekeys are already spent, so throwing them away would consume key
            // material and deliver nothing. It is a fact worth a log line, because an
            // account with more devices than this is either a bug or a farm.
            tracing::warn!(
                devices = stored.len(),
                limit = MAX_BUNDLES_PER_FETCH,
                "key bundle fetch returned more devices than the ceiling"
            );
        }

        let mut bundles = Vec::with_capacity(stored.len());
        let mut exhausted = 0usize;
        for row in stored {
            let bundle = Self::bundle_of(row)?;
            if bundle.is_exhausted() {
                exhausted += 1;
            }
            bundles.push(bundle);
        }

        self.meters.served(bundles.len(), exhausted);
        if exhausted > 0 && self.config.refuse_when_exhausted {
            // Section 163's "bila kebijakan menuntut". The prekeys are already consumed by
            // now and this throws the bundles away, which is the honest cost of the policy:
            // a deployment that would rather fail than form a session without a one-time
            // prekey is choosing that, and the metric above records what it cost.
            self.meters.refused();
            return Err(fault::error(
                codes::PREKEYS_EXHAUSTED,
                "a device has no one-time prekey left and policy refuses the weaker session",
            ));
        }

        Ok(Fetched {
            any_exhausted: exhausted > 0,
            bundles,
        })
    }
}

/// Lower-case hex, for the fingerprint a user reads out loud.
///
/// Hand-rolled rather than a `hex` dependency, matching `migo_ratelimit`: one loop against
/// a crate in the dependency graph of every build is not a trade worth making.
fn hex_of(bytes: &[u8; 32]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(DIGITS[usize::from(byte >> 4)] as char);
        out.push(DIGITS[usize::from(byte & 0x0f)] as char);
    }
    out
}
