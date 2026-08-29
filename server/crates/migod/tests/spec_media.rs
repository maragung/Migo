//! Integration coverage for the MEDIA domain SPEC opcodes.
//!
//! The dispatch handlers in `migod::dispatch::media` are `pub(crate)`, so an integration
//! test in a separate crate cannot call them directly. What can be driven from here is the
//! very [`Library`] method each handler delegates to, over the same in-memory backends the
//! composition root would assemble — so this file asserts the behaviour
//! `handle_upload_begin` relies on: a well-formed upload request mints a ticket bound to
//! the caller, with a token the later status call accepts.

use std::sync::Arc;

use async_trait::async_trait;
use migo_cache::MemoryCache;
use migo_core::config::Config;
use migo_core::metrics::Registry;
use migo_core::{Id, OsRandom, Timestamp};
use migo_media::model::{
    Caller, Destination, Grant, MediaKind, Ticket, UploadRequest, SNIFF_BYTES,
};
use migo_media::traits::{Head, Storage};
use migo_media::{open as media_open, SharedLibrary};
use migo_protocol::codes;
use migo_ratelimit::{CacheRateLimiter, Policies, TrustTier};
use migo_store::model::NewAccount;
use migo_store::traits::AccountStore;
use migo_store::{MemoryStore, SharedStore};

const NOW: i64 = 1_700_000_000_000;

/// The one collaborator that is not the real thing: a stand-in for the object bucket.
/// It only has to sign URLs, because the behaviour under test is ticket minting.
#[derive(Default)]
struct UnsignedBucket;

#[async_trait]
impl Storage for UnsignedBucket {
    async fn sign_upload(
        &self,
        key: &str,
        _byte_size: u64,
        expires_at: Timestamp,
    ) -> migo_core::Result<Grant> {
        Ok(Grant::new(
            format!("https://storage.test/up/{key}"),
            expires_at,
        ))
    }

    async fn sign_download(&self, key: &str, expires_at: Timestamp) -> migo_core::Result<Grant> {
        Ok(Grant::new(
            format!("https://storage.test/down/{key}"),
            expires_at,
        ))
    }

    async fn head(&self, _key: &str, head_len: usize) -> migo_core::Result<Option<Head>> {
        let _ = head_len.min(SNIFF_BYTES);
        Ok(None)
    }

    async fn uploaded_bytes(&self, _key: &str) -> migo_core::Result<Option<u64>> {
        Ok(None)
    }

    async fn remove(&self, _key: &str) -> migo_core::Result<()> {
        Ok(())
    }
}

/// The library the media handlers borrow, over in-memory backends.
fn harness(store: &SharedStore, registry: &Registry) -> SharedLibrary {
    let settings = Config::default();
    let policies = Policies::from_config(&settings.rate_limit).expect("default policies are valid");
    let limiter = Arc::new(CacheRateLimiter::new(
        Arc::new(MemoryCache::new()),
        policies,
        registry,
    ));
    media_open(
        Arc::clone(store),
        limiter,
        Arc::new(UnsignedBucket),
        Box::new(OsRandom),
        b"a-root-secret-that-exists-only-in-this-test-binary",
        &Config::default().media,
        registry,
    )
}

/// The account row `begin`'s identity check asks the store for.
async fn seed_account(store: &MemoryStore, account: u128) {
    use migo_core::Secret;
    store
        .create_account(NewAccount {
            account_id: Id::from(account),
            username: format!("user-{account}"),
            email: None,
            phone: None,
            password_hash: Secret::new("unused"),
            locale: "id-ID".to_string(),
            country: Some("ID".to_string()),
            created_at: Timestamp::from_millis(NOW),
        })
        .await
        .expect("seed account");
}

fn caller(account: u128, device: u128) -> Caller {
    Caller::new(
        Id::from(account),
        Id::from(device),
        TrustTier::Established,
        Timestamp::from_millis(NOW),
    )
}

fn avatar() -> UploadRequest {
    UploadRequest {
        kind: MediaKind::Avatar,
        mime: "image/png".to_string(),
        byte_size: 1_024,
        destination: Destination::Profile,
        width: Some(128),
        height: Some(128),
        duration_ms: None,
    }
}

/// The path `MEDIA_UPLOAD_BEGIN` drives: the library mints a ticket whose media id is
/// real and whose token the very next `status` call accepts.
#[tokio::test]
async fn begin_mints_a_ticket_the_caller_can_ask_status_for() {
    let mem = Arc::new(MemoryStore::new());
    seed_account(&mem, 1).await;
    let store: SharedStore = mem;
    let registry = Registry::new();
    let library = harness(&store, &registry);

    let ticket: Ticket = library
        .begin(&caller(1, 101), avatar())
        .await
        .expect("a well-formed upload request is minted a ticket");
    assert!(
        !ticket.media_id.is_nil(),
        "a minted ticket carries a real media id"
    );
    assert!(
        !ticket.token.is_empty(),
        "the ticket's token is what status, commit, and abort present"
    );

    let progress = library
        .status(&caller(1, 101), &ticket.token)
        .await
        .expect("the minted token is accepted by status");
    assert_eq!(
        progress.uploaded_bytes, 0,
        "nothing has been uploaded through the signed URL yet"
    );
}

/// A ticket minted for one device cannot be driven from another, which is the binding
/// the handler relies on when it hands the ticket to the client that asked for it.
#[tokio::test]
async fn a_ticket_is_bound_to_the_device_that_asked_for_it() {
    let mem = Arc::new(MemoryStore::new());
    seed_account(&mem, 1).await;
    let store: SharedStore = mem;
    let registry = Registry::new();
    let library = harness(&store, &registry);

    let ticket = library
        .begin(&caller(1, 101), avatar())
        .await
        .expect("the ticket is minted");

    let stolen = library
        .status(&caller(1, 202), &ticket.token)
        .await
        .expect_err("another device of the same account cannot present the ticket");
    assert_eq!(
        stolen.code(),
        codes::VALIDATION_FAILED,
        "a lifted ticket must not read another device's upload"
    );
}
