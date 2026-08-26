//! The key-material contract.
//!
//! Two methods, and they are the two halves of one thing: a device says what its public
//! keys are, and a sender asks what somebody else's public keys are. They belong on one
//! trait because the same rule governs both — the server never holds a private key, never
//! vouches for a public one, and never lets one device speak for another — and a trait
//! that could serve a bundle without being the trait that verified the signature on it
//! would be the half of the operation without the check.
//!
//! # Why the server verifies a signature it is not trusted about
//!
//! The signature check in [`Keyring::publish`] is not what makes the protocol safe. The
//! sender verifies the bundle it receives, on the device, before it composes anything, and
//! that check is the one that matters: it is what makes a server that substitutes a prekey
//! it controls fail rather than succeed.
//!
//! The server checks anyway, at publication, because a malformed bundle that is stored is
//! a conversation that cannot start and gives no reason why. Refusing it at publication
//! turns "messages to this device silently fail" into "your client got
//! `INVALID_KEY_MATERIAL` when it published", which is a bug report instead of a mystery.
//! It is a data-integrity check performed by a party nobody trusts, and it is worth
//! having for exactly that reason and no more.
//!
//! # What is deliberately not here
//!
//! **No revocation.** [`migo_store::traits::KeyStore::revoke_device_keys`] exists and is
//! called by whoever removes a device, because revoking key material is one step of
//! removing a device and not an operation a client performs on its own. A `revoke` method
//! here would be a second way to do half of that, and the half that skipped the session
//! teardown would leave a device that cannot be written to but is still logged in.
//!
//! **No private key, in any form, anywhere.** Section 163 forbids it, and the shape of
//! this trait is how that is enforced rather than remembered: there is no parameter and no
//! return field a private key fits in, so a future edit that wanted to escrow one would
//! have to change this file, which is a change a reviewer sees.
//!
//! **No group epoch and no call key.** Section 163 marks both `STATUS: SPEC`. The
//! sender-key epoch is a room concern and the call media key is derived on the devices from
//! a session this crate never sees.
//!
//! **No fingerprint verification.** The server returns a fingerprint of what it stored;
//! deciding that a fingerprint is the *right* one is a thing two humans do out of band, and
//! a server-side "verified" bit would be the server asserting the one fact it is not
//! allowed to be believed about.

use async_trait::async_trait;
use migo_core::{Id, Result};

use crate::model::{Caller, Fetched, PublishOutcome, PublishRequest};

/// Publishing and fetching public key material.
#[async_trait]
pub trait Keyring: Send + Sync {
    /// Publishes this device's public key material, replacing what it had.
    ///
    /// Always for [`Caller::device_id`]. The request has no device field, so one device
    /// cannot publish an identity for another — which would be the whole attack, since the
    /// published identity is what every future sender verifies against.
    ///
    /// Replacing rather than merging, and that includes the one-time prekeys. A device
    /// publishing is declaring what its current key material *is*, and the one thing that
    /// must never happen is the server handing out a prekey whose private half the device no
    /// longer holds: a reinstalled client has lost every old private key, and a merge would
    /// leave the server serving those for weeks, with every session formed from one
    /// undecryptable by its recipient. So a top-up is a publication of a fresh batch under
    /// fresh key ids, and a client keeps the private halves of the batch it just retired
    /// until any message already in flight has landed.
    ///
    /// # Errors
    ///
    /// `INVALID_KEY_MATERIAL` when the identity key is not a valid pair of points, when the
    /// signed prekey's signature does not verify against it, when a one-time prekey is not
    /// a valid X25519 public key, or when the signed prekey is already expired at the
    /// moment it is published. `VALIDATION_FAILED` for a length or count that is
    /// structurally wrong.
    async fn publish(&self, caller: &Caller, request: PublishRequest) -> Result<PublishOutcome>;

    /// Fetches bundles for one device, or for every live device of an account.
    ///
    /// `device_id: None` means every live device, which is what a sender needs: a message
    /// is encrypted once per recipient *device*, so a client that fetched one bundle would
    /// deliver to one of somebody's phones and silently not to the others.
    ///
    /// **Consumes a one-time prekey per bundle returned.** That is the point of the
    /// operation and not a side effect: the same one-time prekey handed to two senders
    /// reduces the guarantee to the signed prekey alone for both of them, and neither would
    /// ever find out. When a device has none left the bundle still comes back, without one,
    /// and [`Fetched::any_exhausted`] says so.
    ///
    /// Key material for a revoked device is never returned. The store applies that filter
    /// in the query rather than after it, so a revoked device is not something this crate
    /// can forget to exclude.
    ///
    /// # Errors
    ///
    /// `PREKEYS_EXHAUSTED` only when
    /// [`KeysConfig::refuse_when_exhausted`](crate::model::KeysConfig::refuse_when_exhausted)
    /// is set. `VALIDATION_FAILED` for a nil `user_id`.
    async fn bundles(&self, caller: &Caller, user_id: Id, device_id: Option<Id>)
        -> Result<Fetched>;
}
