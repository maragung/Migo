//! Integration tests for the media library.
//!
//! Four claims hold this crate together, and each of them is a thing that goes wrong in
//! production rather than a thing that goes wrong in a compiler:
//!
//! 1. The type of an object comes from its bytes, never from the header a client sent.
//! 2. A ticket is bound to one account and one device, and a ticket that does not verify
//!    is indistinguishable from one presented by the wrong device.
//! 3. An object a caller may not have and an object that is not there are the same
//!    answer, so that an id is never an existence oracle.
//! 4. The server never sees plaintext it was not given, and never records a signed URL,
//!    a storage key, or an id in a metric label.
//!
//! The suite runs against the real rate limiter over a real cache and the real in-memory
//! store, so the budgets and the store's own refusals are exercised as deployed. Only
//! object storage is faked, because the alternative is an S3 endpoint.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use migo_cache::MemoryCache;
use migo_core::config::{Config, MediaConfig};
use migo_core::metrics::Registry;
use migo_core::random::SeededRandom;
use migo_core::{Id, Result, Secret, Timestamp};
use migo_media::model::{
    Caller, Commit, Destination, Grant, MediaKind, Policy, Progress, Scan, Stored, Ticket,
    UploadRequest, Verdict, CHUNK_BYTES, MAX_CHECKSUM_LEN, MAX_MIME_LEN, SNIFF_BYTES,
    TICKET_TTL_MS, VOICE_NOTE_MAX_MS,
};
use migo_media::service::Media;
use migo_media::traits::{storage_key, Head, Library, ScanQueue, Storage};
use migo_protocol::{codes, ConversationKind, EncryptionMode};
use migo_ratelimit::{CacheRateLimiter, Policies, TrustTier};
use migo_store::model::{Conversation, ConversationMember, NewAccount, Profile, Visibility};
use migo_store::traits::{AccountStore, MediaStore, MessagingStore};
use migo_store::MemoryStore;
use parking_lot::Mutex;

type TestMedia = Media<MemoryStore, CacheRateLimiter<MemoryCache>, FakeStorage>;

const SECOND: i64 = 1_000;
const MINUTE: i64 = 60 * SECOND;
const NOW: i64 = 1_700_000_000 * SECOND;

const ALICE: u128 = 1;
const BOB: u128 = 2;
const CAROL: u128 = 3;
const ALICE_PHONE: u128 = 101;
const ALICE_LAPTOP: u128 = 111;
const BOB_LAPTOP: u128 = 102;
const CAROL_PHONE: u128 = 103;
const CHAT: u128 = 500;
const SEALED_CHAT: u128 = 501;
const OTHER_CHAT: u128 = 502;

/// A PNG's eight-byte signature, which is all the sniffer reads.
const PNG: &[u8] = &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
/// A JPEG's three-byte start-of-image marker.
const JPEG: &[u8] = &[0xFF, 0xD8, 0xFF];
/// An Ogg page header, which is how an Opus voice note starts.
const OGG: &[u8] = b"OggS";

fn ts(millis: i64) -> Timestamp {
    Timestamp::from_millis(millis)
}

fn id(value: u128) -> Id {
    Id::from(value)
}

fn caller(account: u128, device: u128) -> Caller {
    Caller::new(id(account), id(device), TrustTier::Established, ts(NOW))
}

/// Object storage, faked in memory.
///
/// The only collaborator that is not the real thing. `Storage` is the crate's port onto
/// S3, so the alternative to a fake is a bucket, and a bucket makes the suite depend on
/// credentials and on the network to answer questions about a MAC and a size check.
///
/// It is deliberately more than a stub: it records the keys it was asked to sign and the
/// bytes it was told to hold, because several of this crate's guarantees are about what
/// the server hands to storage rather than about what it hands back to the client.
#[derive(Default)]
struct FakeStorage {
    objects: Mutex<HashMap<String, Vec<u8>>>,
    signed_uploads: Mutex<Vec<String>>,
    signed_downloads: Mutex<Vec<String>>,
    removals: Mutex<Vec<String>>,
    /// When set, every method fails, standing in for a bucket that is unreachable.
    broken: Mutex<bool>,
    urls: AtomicUsize,
}

impl FakeStorage {
    fn fail_from_now_on(&self) {
        *self.broken.lock() = true;
    }

    fn works_again(&self) {
        *self.broken.lock() = false;
    }

    fn refuse_if_broken(&self) -> Result<()> {
        if *self.broken.lock() {
            return Err(migo_protocol::fault::error(
                codes::INTERNAL_ERROR,
                "the bucket is unreachable",
            ));
        }
        Ok(())
    }

    /// Puts bytes at a key, the way a client's PUT to the signed URL would.
    fn upload(&self, key: &str, bytes: &[u8]) {
        self.objects.lock().insert(key.to_string(), bytes.to_vec());
    }

    /// The single key a signed upload was issued for, when exactly one was.
    fn only_upload_key(&self) -> String {
        let keys = self.signed_uploads.lock();
        assert_eq!(keys.len(), 1, "expected exactly one signed upload");
        keys[0].clone()
    }

    fn holds(&self, key: &str) -> bool {
        self.objects.lock().contains_key(key)
    }

    fn download_count(&self) -> usize {
        self.signed_downloads.lock().len()
    }

    fn removed(&self) -> Vec<String> {
        self.removals.lock().clone()
    }

    /// A URL that is different every time, so a test can tell two grants apart.
    fn mint_url(&self, key: &str) -> String {
        let serial = self.urls.fetch_add(1, Ordering::Relaxed);
        format!("https://storage.test/{key}?sig=deadbeef{serial}")
    }
}

#[async_trait]
impl Storage for FakeStorage {
    async fn sign_upload(
        &self,
        key: &str,
        _byte_size: u64,
        expires_at: Timestamp,
    ) -> Result<Grant> {
        self.refuse_if_broken()?;
        self.signed_uploads.lock().push(key.to_string());
        Ok(Grant::new(self.mint_url(key), expires_at))
    }

    async fn sign_download(&self, key: &str, expires_at: Timestamp) -> Result<Grant> {
        self.refuse_if_broken()?;
        self.signed_downloads.lock().push(key.to_string());
        Ok(Grant::new(self.mint_url(key), expires_at))
    }

    async fn head(&self, key: &str, head_len: usize) -> Result<Option<Head>> {
        self.refuse_if_broken()?;
        let objects = self.objects.lock();
        let Some(bytes) = objects.get(key) else {
            return Ok(None);
        };
        let mut head = [0u8; SNIFF_BYTES];
        let taken = head_len.min(SNIFF_BYTES).min(bytes.len());
        head[..taken].copy_from_slice(&bytes[..taken]);
        Ok(Some(Head {
            byte_size: bytes.len() as u64,
            head,
            head_len: taken,
        }))
    }

    async fn uploaded_bytes(&self, key: &str) -> Result<Option<u64>> {
        self.refuse_if_broken()?;
        Ok(self.objects.lock().get(key).map(|bytes| bytes.len() as u64))
    }

    async fn remove(&self, key: &str) -> Result<()> {
        self.refuse_if_broken()?;
        self.removals.lock().push(key.to_string());
        self.objects.lock().remove(key);
        Ok(())
    }
}

/// Everything a test needs, with the real limiter over a real cache and the real store.
struct Harness {
    media: TestMedia,
    store: Arc<MemoryStore>,
    storage: Arc<FakeStorage>,
    registry: Registry,
}

impl Harness {
    fn new() -> Self {
        Self::configured(&MediaConfig::default())
    }

    fn configured(media: &MediaConfig) -> Self {
        let settings = Config::default();
        let store = Arc::new(MemoryStore::new());
        let storage = Arc::new(FakeStorage::default());
        let registry = Registry::new();
        let policies =
            Policies::from_config(&settings.rate_limit).expect("the default policies are valid");
        let limiter = Arc::new(CacheRateLimiter::new(
            Arc::new(MemoryCache::new()),
            policies,
            &registry,
        ));
        // A fixed seed, so a test that asserts on two different media ids gets two
        // different media ids on every run and on every machine.
        let library = Media::new(
            Arc::clone(&store),
            limiter,
            Arc::clone(&storage),
            Box::new(SeededRandom::new(0x4d69_676f_5445_5354)),
            b"a-root-secret-that-exists-only-in-this-test-binary",
            media,
            &registry,
        );
        Self {
            media: library,
            store,
            storage,
            registry,
        }
    }

    /// Replaces the per-kind limits, for the tests that need a small ceiling.
    fn with_policy(self, policy: Policy) -> Self {
        let Self {
            media,
            store,
            storage,
            registry,
        } = self;
        Self {
            media: media.with_policy(policy),
            store,
            storage,
            registry,
        }
    }

    async fn person(&self, account: u128, username: &str) {
        self.store
            .create_account(NewAccount {
                account_id: id(account),
                username: username.to_string(),
                email: Some(format!("{username}@example.test")),
                phone: None,
                password_hash: Secret::new("$argon2id$v=19$m=19456,t=2,p=1$c2FsdA$aGFzaA"),
                locale: "id-ID".to_string(),
                country: Some("ID".to_string()),
                created_at: ts(SECOND),
            })
            .await
            .expect("a fresh username is free");
        self.store
            .create_profile(Profile {
                account_id: id(account),
                display_name: format!("{username} Nusantara"),
                bio: None,
                avatar_media_id: None,
                birth_year: Some(1995),
                gender: None,
                show_last_seen: Visibility::Everyone,
                who_can_message: Visibility::Everyone,
                who_can_add: Visibility::Everyone,
                searchable: true,
                updated_at: ts(SECOND),
            })
            .await
            .expect("a new account has no profile yet");
    }

    /// A conversation with the given members and encryption mode.
    async fn conversation(&self, conversation: u128, encryption: EncryptionMode, members: &[u128]) {
        self.store
            .create_conversation(
                Conversation {
                    conversation_id: id(conversation),
                    kind: ConversationKind::Group,
                    encryption,
                    room_id: None,
                    last_seq: 0,
                    created_by: id(members[0]),
                    created_at: ts(SECOND),
                    last_message_at: None,
                    archived_at: None,
                },
                members.iter().copied().map(id).collect(),
            )
            .await
            .expect("a fresh conversation id is free");
    }

    /// The three accounts and the two conversations most tests want.
    ///
    /// Alice and Bob share `CHAT` (server-readable) and `SEALED_CHAT` (end-to-end).
    /// Carol shares nothing with either of them and is a member of `OTHER_CHAT`, which
    /// is what "a conversation that exists but is not yours" looks like.
    async fn cast(&self) {
        self.person(ALICE, "alice").await;
        self.person(BOB, "bobby").await;
        self.person(CAROL, "carol").await;
        self.conversation(CHAT, EncryptionMode::Transport, &[ALICE, BOB])
            .await;
        self.conversation(SEALED_CHAT, EncryptionMode::EndToEnd, &[ALICE, BOB])
            .await;
        self.conversation(OTHER_CHAT, EncryptionMode::Transport, &[CAROL])
            .await;
    }

    fn counter(&self, name: &'static str, label: &'static str, value: &'static str) -> u64 {
        self.registry.counter(name, "", &[(label, value)]).get()
    }

    fn plain(&self, name: &'static str) -> u64 {
        self.registry.counter(name, "", &[]).get()
    }

    /// Uploads bytes to the key the one outstanding ticket was signed for.
    fn push_bytes(&self, bytes: &[u8]) {
        self.storage.upload(&self.storage.only_upload_key(), bytes);
    }
}

/// A request for a small image into `CHAT`, which most tests vary one field of.
fn image_into(conversation: u128) -> UploadRequest {
    UploadRequest {
        kind: MediaKind::Image,
        mime: "image/png".to_string(),
        byte_size: 4_096,
        destination: Destination::Conversation(id(conversation)),
        width: Some(64),
        height: Some(64),
        duration_ms: None,
    }
}

/// An avatar, which is the one kind that needs no conversation.
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

/// Bytes of a given length whose leading bytes are a real signature.
fn payload(signature: &[u8], len: usize) -> Vec<u8> {
    let mut bytes = signature.to_vec();
    bytes.resize(len.max(signature.len()), b'\0');
    bytes
}

/// Asserts that a call was refused with a particular registry code.
fn expect_code<T>(result: Result<T>, code: u32) {
    let error = result.err().expect("this call must be refused");
    assert_eq!(
        error.code(),
        code,
        "expected code {code}, got {}: {error}",
        error.code()
    );
}

// ---------------------------------------------------------------------------
// Identity
//
// Every method on this crate spends the caller's budget and most of them mint a
// signed, authenticated ticket. A caller with no account is not a caller, and the
// check has to come before the charge -- otherwise an unauthenticated request is
// billed to `Id::NIL`, which is one bucket shared by every such request in the
// deployment, and the crate has invented a way to exhaust a stranger's budget.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_unidentified_caller_gets_no_ticket() {
    let harness = Harness::new();
    harness.cast().await;

    // No account.
    expect_code(
        harness
            .media
            .begin(
                &Caller::new(Id::NIL, id(ALICE_PHONE), TrustTier::Established, ts(NOW)),
                image_into(CHAT),
            )
            .await,
        codes::UNAUTHENTICATED,
    );
    // No device. A session that authenticated an account but not the connection it
    // arrived on cannot be given a ticket, because the ticket's whole job is to bind
    // the upload to one connection.
    expect_code(
        harness
            .media
            .begin(
                &Caller::new(id(ALICE), Id::NIL, TrustTier::Established, ts(NOW)),
                image_into(CHAT),
            )
            .await,
        codes::UNAUTHENTICATED,
    );

    // Nothing was signed and nothing was charged.
    assert!(harness.storage.signed_uploads.lock().is_empty());
    assert_eq!(
        harness.plain("migo_media_uploads_begun_total"),
        0,
        "an unidentified caller must not appear in the upload counters"
    );
}

#[tokio::test]
async fn every_method_needs_an_identified_caller() {
    let harness = Harness::new();
    harness.cast().await;
    let alice = caller(ALICE, ALICE_PHONE);

    // A real object and a real ticket, so that the refusal below is about the caller
    // and not about the thing being asked for.
    let ticket = harness
        .media
        .begin(&alice, avatar())
        .await
        .expect("an identified caller may begin an upload");
    harness.push_bytes(&payload(PNG, 1_024));
    let stored = harness
        .media
        .commit(
            &alice,
            &ticket.token,
            Commit {
                byte_size: 1_024,
                checksum: None,
            },
        )
        .await
        .expect("a complete upload commits");

    let second = harness
        .media
        .begin(&alice, avatar())
        .await
        .expect("a second upload may begin");

    let nobody = Caller::new(Id::NIL, Id::NIL, TrustTier::Established, ts(NOW));

    expect_code(
        harness.media.status(&nobody, &second.token).await,
        codes::UNAUTHENTICATED,
    );
    expect_code(
        harness
            .media
            .commit(&nobody, &second.token, Commit::default())
            .await,
        codes::UNAUTHENTICATED,
    );
    expect_code(
        harness.media.abort(&nobody, &second.token).await,
        codes::UNAUTHENTICATED,
    );
    expect_code(
        harness.media.fetch_url(&nobody, stored.media_id).await,
        codes::UNAUTHENTICATED,
    );
    expect_code(
        harness.media.describe(&nobody, stored.media_id).await,
        codes::UNAUTHENTICATED,
    );
    expect_code(
        harness.media.delete(&nobody, stored.media_id).await,
        codes::UNAUTHENTICATED,
    );

    // The object is untouched: no refusal above deleted anything or served anything.
    assert_eq!(harness.storage.download_count(), 0);
    assert!(harness.storage.removed().is_empty());
    harness
        .media
        .describe(&alice, stored.media_id)
        .await
        .expect("the owner can still read it");
}

// ---------------------------------------------------------------------------
// The request's own numbers
//
// Everything here is decided before a byte moves and before the conversation row is
// read, because these are the refusals a hostile client produces in a loop.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_request_with_no_mime_type_is_a_missing_field() {
    let harness = Harness::new();
    harness.cast().await;
    let alice = caller(ALICE, ALICE_PHONE);

    for blank in ["", "   ", "\t\n"] {
        let mut request = image_into(CHAT);
        request.mime = blank.to_string();
        expect_code(
            harness.media.begin(&alice, request).await,
            codes::FIELD_REQUIRED,
        );
    }
    assert_eq!(
        harness.counter("migo_media_upload_refusals_total", "reason", "invalid"),
        3
    );
}

#[tokio::test]
async fn a_mime_type_is_measured_and_capped() {
    let harness = Harness::new();
    harness.cast().await;
    let alice = caller(ALICE, ALICE_PHONE);

    // Exactly at the ceiling is accepted, and the ticket carries it, which is the point
    // of checking the length here rather than letting `seal` truncate it.
    let mut at_ceiling = image_into(CHAT);
    at_ceiling.mime = format!("image/{}", "x".repeat(MAX_MIME_LEN - "image/".len()));
    assert_eq!(at_ceiling.mime.len(), MAX_MIME_LEN);
    harness
        .media
        .begin(&alice, at_ceiling)
        .await
        .expect("a MIME type exactly at the ceiling is usable");

    let mut past_ceiling = image_into(CHAT);
    past_ceiling.mime = format!("image/{}", "x".repeat(MAX_MIME_LEN));
    expect_code(
        harness.media.begin(&alice, past_ceiling).await,
        codes::FIELD_TOO_LONG,
    );
}

#[tokio::test]
async fn an_upload_of_nothing_is_refused() {
    let harness = Harness::new();
    harness.cast().await;
    let mut request = image_into(CHAT);
    request.byte_size = 0;

    expect_code(
        harness
            .media
            .begin(&caller(ALICE, ALICE_PHONE), request)
            .await,
        codes::VALIDATION_FAILED,
    );
    // A zero-byte upload never reaches storage, so no key was signed and no object was
    // left in the bucket for a lifecycle rule to clean up.
    assert!(harness.storage.signed_uploads.lock().is_empty());
}

#[tokio::test]
async fn each_kind_has_its_own_ceiling_and_says_what_it_is() {
    let harness = Harness::new();
    harness.cast().await;
    let alice = caller(ALICE, ALICE_PHONE);
    let policy = harness.media.policy().clone();

    // The default config's ceiling is below the product default for a video, so the
    // clamp in `Policy::from_config` is doing something: an operator's roof lowers the
    // room, and an avatar stays small whatever the roof is.
    assert!(policy.max_bytes(MediaKind::Avatar) < policy.max_bytes(MediaKind::Image));
    assert_eq!(
        policy.max_bytes(MediaKind::Video),
        MediaConfig::default().max_upload_bytes,
        "a per-kind default above the operator's ceiling is clamped to it"
    );

    for kind in MediaKind::ALL {
        let ceiling = policy.max_bytes(kind);
        let mut at_ceiling = image_into(CHAT);
        at_ceiling.kind = kind;
        at_ceiling.byte_size = ceiling;
        if kind == MediaKind::Avatar {
            at_ceiling.destination = Destination::Profile;
        }
        harness
            .media
            .begin(&alice, at_ceiling)
            .await
            .expect("an object exactly at its kind's ceiling is accepted");

        let mut past_ceiling = image_into(CHAT);
        past_ceiling.kind = kind;
        past_ceiling.byte_size = ceiling + 1;
        let error = harness
            .media
            .begin(&alice, past_ceiling)
            .await
            .expect_err("one byte past the ceiling is refused");
        assert_eq!(error.code(), codes::UPLOAD_LIMIT_EXCEEDED);
        // The ceiling is disclosed, because a client that cannot learn the limit cannot
        // compress to fit it. It is the only number in the public message.
        assert!(
            error.public_message().contains(&ceiling.to_string()),
            "the refusal must name the ceiling: {}",
            error.public_message()
        );
    }
    assert_eq!(
        harness.counter("migo_media_upload_refusals_total", "reason", "too_large"),
        6
    );
}

#[tokio::test]
async fn a_voice_note_longer_than_the_deployment_allows_is_refused() {
    let harness = Harness::new();
    harness.cast().await;
    let alice = caller(ALICE, ALICE_PHONE);

    let voice_note = |duration_ms: Option<u32>| UploadRequest {
        kind: MediaKind::VoiceNote,
        mime: "audio/ogg".to_string(),
        byte_size: 64 * 1024,
        destination: Destination::Conversation(id(CHAT)),
        width: None,
        height: None,
        duration_ms,
    };

    // Exactly at the ceiling is fine; the check is on being longer.
    harness
        .media
        .begin(&alice, voice_note(Some(VOICE_NOTE_MAX_MS)))
        .await
        .expect("a voice note exactly at the ceiling is accepted");
    expect_code(
        harness
            .media
            .begin(&alice, voice_note(Some(VOICE_NOTE_MAX_MS + 1)))
            .await,
        codes::VALIDATION_FAILED,
    );
    assert_eq!(
        harness.counter("migo_media_upload_refusals_total", "reason", "too_long"),
        1
    );

    // A voice note with no declared duration is not refused for length. The server has
    // no way to measure one before the bytes arrive, and refusing the upload would make
    // the field mandatory in a place the brief does not.
    harness
        .media
        .begin(&alice, voice_note(None))
        .await
        .expect("an undeclared duration is not a length violation");

    // The duration ceiling belongs to voice notes. An audio track of the same length is
    // a different product decision and is not measured against it.
    let mut track = voice_note(Some(VOICE_NOTE_MAX_MS * 10));
    track.kind = MediaKind::Audio;
    harness
        .media
        .begin(&alice, track)
        .await
        .expect("an audio track is not a voice note");
}

// ---------------------------------------------------------------------------
// Where an upload is going
//
// The one place in this crate that reads the conversation row, and the reason a room
// needs no separate branch.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_avatar_needs_no_conversation() {
    let harness = Harness::new();
    // Deliberately no cast: an account with no conversations at all may still upload
    // its own avatar, because there is nothing for a membership check to read.
    harness.person(ALICE, "alice").await;

    harness
        .media
        .begin(&caller(ALICE, ALICE_PHONE), avatar())
        .await
        .expect("an account may always upload its own avatar");
    assert_eq!(
        harness.counter("migo_media_uploads_begun_total", "kind", "avatar"),
        1
    );
}

#[tokio::test]
async fn an_unknown_conversation_and_somebody_elses_are_the_same_answer() {
    let harness = Harness::new();
    harness.cast().await;
    let alice = caller(ALICE, ALICE_PHONE);

    // A conversation that does not exist.
    let missing = harness
        .media
        .begin(&alice, image_into(9_999))
        .await
        .expect_err("an unknown conversation is refused");
    // A conversation that exists and is Carol's.
    let forbidden = harness
        .media
        .begin(&alice, image_into(OTHER_CHAT))
        .await
        .expect_err("a conversation the caller is not in is refused");

    // NOT_FOUND for both, and the same message, so an id is not an existence oracle.
    // PERMISSION_DENIED on the second would tell Alice that OTHER_CHAT is a real
    // conversation she is not in, which is the difference between "wrong id" and
    // "Carol is talking to somebody".
    assert_eq!(missing.code(), codes::NOT_FOUND);
    assert_eq!(forbidden.code(), codes::NOT_FOUND);
    assert_eq!(missing.public_message(), forbidden.public_message());
    assert_eq!(
        harness.counter("migo_media_upload_refusals_total", "reason", "denied"),
        2
    );
}

#[tokio::test]
async fn a_member_who_left_may_no_longer_upload() {
    let harness = Harness::new();
    harness.cast().await;
    let bob = caller(BOB, BOB_LAPTOP);

    harness
        .media
        .begin(&bob, image_into(CHAT))
        .await
        .expect("a member may upload");

    harness
        .store
        .remove_member(id(CHAT), id(BOB), ts(NOW - MINUTE))
        .await
        .expect("a member may leave");

    // Membership is tombstoned rather than deleted, so a former member is a row that
    // still exists. If the check read the row's existence instead of its `left_at`,
    // this would still succeed.
    expect_code(
        harness.media.begin(&bob, image_into(CHAT)).await,
        codes::NOT_FOUND,
    );
    assert!(harness
        .store
        .members(id(CHAT))
        .await
        .expect("the conversation still lists its history")
        .iter()
        .any(|member| member.account_id == id(BOB) && member.left_at.is_some()));

    // Rejoining restores it, which is the same one question being asked again rather
    // than a second code path.
    harness
        .store
        .add_member(ConversationMember {
            conversation_id: id(CHAT),
            account_id: id(BOB),
            role: 0,
            joined_at: ts(NOW),
            left_at: None,
            muted_until: None,
            pinned: false,
        })
        .await
        .expect("a former member may rejoin");
    harness
        .media
        .begin(&bob, image_into(CHAT))
        .await
        .expect("a rejoined member may upload again");
}

// ---------------------------------------------------------------------------
// The ticket
//
// The server holds no state between begin and commit, so everything the server would
// otherwise have to trust the client for lives inside a MAC. These tests are the
// reason that is safe.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_ticket_says_where_to_put_the_bytes_and_for_how_long() {
    let harness = Harness::new();
    harness.cast().await;
    let alice = caller(ALICE, ALICE_PHONE);

    let ticket: Ticket = harness
        .media
        .begin(&alice, image_into(CHAT))
        .await
        .expect("a member may upload");

    assert_eq!(ticket.chunk_bytes, CHUNK_BYTES);
    assert_eq!(ticket.expires_at.as_millis(), NOW + TICKET_TTL_MS);
    // The signed URL expires with the ticket, not after it: a URL that outlived the
    // permission it was issued under would be a permission with no expiry.
    assert_eq!(ticket.upload.expires_at, ticket.expires_at);
    assert_eq!(ticket.upload.remaining_ms(ts(NOW)), TICKET_TTL_MS);

    // The id is handed out before the row exists, so a client can reference the
    // attachment in the message it is composing.
    assert!(!ticket.media_id.is_nil());
    assert!(harness
        .store
        .media(ticket.media_id)
        .await
        .expect("the store answers")
        .is_none());

    // The key names the kind and the destination scope but nothing about the caller.
    let key = harness.storage.only_upload_key();
    assert!(key.starts_with("c/image/"), "unexpected key shape: {key}");
    assert!(key.ends_with(&ticket.media_id.to_text()));
    assert!(!key.contains(&id(ALICE).to_text()));
    assert!(!key.contains(&id(CHAT).to_text()));
}

#[tokio::test]
async fn two_begins_are_two_objects() {
    let harness = Harness::new();
    harness.cast().await;
    let alice = caller(ALICE, ALICE_PHONE);

    let first = harness
        .media
        .begin(&alice, image_into(CHAT))
        .await
        .expect("the first upload begins");
    let second = harness
        .media
        .begin(&alice, image_into(CHAT))
        .await
        .expect("the second upload begins");

    // A retry of begin that the client never saw the answer to mints a new id rather
    // than resuming the old one, and the abandoned one never becomes a row at all.
    assert_ne!(first.media_id, second.media_id);
    assert_ne!(first.token, second.token);
    let keys = harness.storage.signed_uploads.lock().clone();
    assert_ne!(keys[0], keys[1]);
}

#[tokio::test]
async fn a_ticket_is_bound_to_one_account_and_one_device() {
    let harness = Harness::new();
    harness.cast().await;
    let alice = caller(ALICE, ALICE_PHONE);

    let ticket = harness
        .media
        .begin(&alice, image_into(CHAT))
        .await
        .expect("alice begins an upload");
    harness.push_bytes(&payload(PNG, 4_096));
    let commit = Commit {
        byte_size: 4_096,
        checksum: None,
    };

    // Bob is in the same conversation and could have begun the same upload himself.
    // That does not make him able to finish Alice's.
    expect_code(
        harness
            .media
            .commit(&caller(BOB, BOB_LAPTOP), &ticket.token, commit.clone())
            .await,
        codes::VALIDATION_FAILED,
    );
    // Alice's other device is Alice, and still cannot: brief section 69 binds the token
    // to a device, so a ticket lifted off one phone is not usable from a laptop.
    expect_code(
        harness
            .media
            .commit(&caller(ALICE, ALICE_LAPTOP), &ticket.token, commit.clone())
            .await,
        codes::VALIDATION_FAILED,
    );

    // Both refusals are the ticket-invalid refusal, not a permission refusal: there is
    // nothing an honest client does differently with the distinction, and telling a
    // thief which half of the binding it got wrong is a hint.
    assert_eq!(
        harness.counter(
            "migo_media_upload_refusals_total",
            "reason",
            "ticket_invalid"
        ),
        2
    );
    assert_eq!(
        harness.counter("migo_media_upload_refusals_total", "reason", "denied"),
        0
    );

    // The rightful device still finishes it.
    harness
        .media
        .commit(&alice, &ticket.token, commit)
        .await
        .expect("the device the ticket was issued to may commit");
}

#[tokio::test]
async fn a_tampered_ticket_is_indistinguishable_from_a_forged_one() {
    let harness = Harness::new();
    harness.cast().await;
    let alice = caller(ALICE, ALICE_PHONE);

    let ticket = harness
        .media
        .begin(&alice, avatar())
        .await
        .expect("alice begins an upload");

    // Raising the size the ticket was issued for, which is the whole reason the MAC
    // exists: without it, a two-mebibyte avatar ticket commits a two-gigabyte object.
    let mut inflated = ticket.token.clone();
    inflated[67..75].copy_from_slice(&(4u64 * 1024 * 1024 * 1024).to_be_bytes());

    // Changing the owner to somebody else's account.
    let mut stolen = ticket.token.clone();
    stolen[17..33].copy_from_slice(id(BOB).as_bytes());

    // Flipping the end-to-end flag, which would skip the content sniff.
    let mut unsniffed = ticket.token.clone();
    unsniffed[83] |= 1;

    // A token that is simply made up, and one truncated to nothing.
    let invented = vec![0xAAu8; ticket.token.len()];
    let truncated = ticket.token[..ticket.token.len() - 1].to_vec();

    for token in [inflated, stolen, unsniffed, invented, truncated] {
        expect_code(
            harness
                .media
                .commit(
                    &alice,
                    &token,
                    Commit {
                        byte_size: 1_024,
                        checksum: None,
                    },
                )
                .await,
            codes::VALIDATION_FAILED,
        );
    }
    assert_eq!(
        harness.counter(
            "migo_media_upload_refusals_total",
            "reason",
            "ticket_invalid"
        ),
        5
    );
    assert_eq!(
        harness.counter(
            "migo_media_upload_refusals_total",
            "reason",
            "ticket_expired"
        ),
        0,
        "a forgery must not be reported as an expiry"
    );
    // Nothing was written, so a forgery leaves no trace in the library.
    assert_eq!(harness.plain("migo_media_uploads_committed_total"), 0);
}

#[tokio::test]
async fn an_expired_ticket_is_counted_apart_from_a_forged_one() {
    let harness = Harness::new();
    harness.cast().await;
    let alice = caller(ALICE, ALICE_PHONE);

    let ticket = harness
        .media
        .begin(&alice, avatar())
        .await
        .expect("alice begins an upload");
    harness.push_bytes(&payload(PNG, 1_024));

    // One millisecond before the expiry the ticket still works; the check is on the
    // expiry having passed, and a ticket that dies a millisecond early would fail an
    // upload that was inside its window.
    let nearly = Caller::new(
        id(ALICE),
        id(ALICE_PHONE),
        TrustTier::Established,
        ts(NOW + TICKET_TTL_MS - 1),
    );
    harness
        .media
        .status(&nearly, &ticket.token)
        .await
        .expect("a ticket one millisecond from expiry is still usable");

    let late = Caller::new(
        id(ALICE),
        id(ALICE_PHONE),
        TrustTier::Established,
        ts(NOW + TICKET_TTL_MS),
    );
    let error = harness
        .media
        .commit(
            &late,
            &ticket.token,
            Commit {
                byte_size: 1_024,
                checksum: None,
            },
        )
        .await
        .expect_err("an expired ticket is refused");
    assert_eq!(error.code(), codes::VALIDATION_FAILED);
    // An honest client is told to call begin again; the counters keep the two apart
    // because an expiry is a slow network and a forgery is somebody probing the MAC.
    assert!(error.public_message().contains("expired"));
    assert_eq!(
        harness.counter(
            "migo_media_upload_refusals_total",
            "reason",
            "ticket_expired"
        ),
        1
    );
    assert_eq!(
        harness.counter(
            "migo_media_upload_refusals_total",
            "reason",
            "ticket_invalid"
        ),
        0
    );
}

#[tokio::test]
async fn a_ticket_carries_the_numbers_the_client_declared() {
    let harness = Harness::new();
    harness.cast().await;
    let alice = caller(ALICE, ALICE_PHONE);

    let mut request = image_into(CHAT);
    request.width = Some(1_920);
    request.height = Some(1_080);
    let ticket = harness
        .media
        .begin(&alice, request)
        .await
        .expect("alice begins an upload");
    harness.push_bytes(&payload(JPEG, 4_096));
    let stored = harness
        .media
        .commit(
            &alice,
            &ticket.token,
            Commit {
                byte_size: 4_096,
                checksum: None,
            },
        )
        .await
        .expect("a complete upload commits");

    // The dimensions survive the round trip. They are the client's own description and
    // the server never measured them, but a chat client needs them to lay out a bubble
    // before the bytes arrive, and dropping them silently would make the column and the
    // documented field a lie.
    assert_eq!(stored.width, Some(1_920));
    assert_eq!(stored.height, Some(1_080));
    assert_eq!(stored.duration_ms, None);

    // And they are what the *ticket* said, not what the commit said, because the commit
    // request has no field for them at all: the numbers begin accepted are the numbers
    // the row records.
    let row = harness
        .store
        .media(stored.media_id)
        .await
        .expect("the store answers")
        .expect("the row exists");
    assert_eq!(row.width, Some(1_920));
    assert_eq!(row.height, Some(1_080));
}

#[tokio::test]
async fn a_voice_notes_duration_survives_to_the_row() {
    let harness = Harness::new();
    harness.cast().await;
    let alice = caller(ALICE, ALICE_PHONE);

    let ticket = harness
        .media
        .begin(
            &alice,
            UploadRequest {
                kind: MediaKind::VoiceNote,
                mime: "audio/ogg".to_string(),
                byte_size: 32 * 1024,
                destination: Destination::Conversation(id(CHAT)),
                width: None,
                height: None,
                duration_ms: Some(12_500),
            },
        )
        .await
        .expect("a voice note may be uploaded");
    harness.push_bytes(&payload(OGG, 32 * 1024));
    let stored = harness
        .media
        .commit(
            &alice,
            &ticket.token,
            Commit {
                byte_size: 32 * 1024,
                checksum: None,
            },
        )
        .await
        .expect("a complete voice note commits");

    // The duration was checked against the deployment's ceiling at begin. If it did not
    // reach the row, that check would have been decorative and a client could not draw
    // the waveform without downloading the audio first.
    assert_eq!(stored.duration_ms, Some(12_500));
    assert_eq!(stored.mime, "audio/ogg");
}

// ---------------------------------------------------------------------------
// Commit
//
// Brief section 168: the server verifies the size and the content hash and then makes
// the record. Brief section 122: the type comes from the bytes, never from the header
// the client sent.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_upload_nobody_pushed_bytes_for_is_not_a_row() {
    let harness = Harness::new();
    harness.cast().await;
    let alice = caller(ALICE, ALICE_PHONE);

    let ticket = harness
        .media
        .begin(&alice, avatar())
        .await
        .expect("alice begins an upload");
    // No bytes were pushed to the signed URL.
    let error = harness
        .media
        .commit(
            &alice,
            &ticket.token,
            Commit {
                byte_size: 1_024,
                checksum: None,
            },
        )
        .await
        .expect_err("an empty key is not a committed object");
    assert_eq!(error.code(), codes::VALIDATION_FAILED);
    assert_eq!(
        harness.counter(
            "migo_media_upload_refusals_total",
            "reason",
            "bytes_missing"
        ),
        1
    );
    assert!(harness
        .store
        .media(ticket.media_id)
        .await
        .expect("the store answers")
        .is_none());
}

#[tokio::test]
async fn storage_is_the_authority_on_how_many_bytes_arrived() {
    let harness = Harness::new();
    harness.cast().await;
    let alice = caller(ALICE, ALICE_PHONE);

    let ticket = harness
        .media
        .begin(&alice, image_into(CHAT))
        .await
        .expect("alice begins an upload");
    harness.push_bytes(&payload(PNG, 3_000));

    // The client claims a size it did not upload. Storage is holding the bytes, so
    // storage wins, and the disagreement is reported rather than papered over: a client
    // that miscounts its own upload is a client whose checksum this row would record.
    let error = harness
        .media
        .commit(
            &alice,
            &ticket.token,
            Commit {
                byte_size: 4_096,
                checksum: None,
            },
        )
        .await
        .expect_err("a size the client invented is refused");
    assert_eq!(error.code(), codes::VALIDATION_FAILED);

    // Under-claiming is the same disagreement, not a generous rounding.
    expect_code(
        harness
            .media
            .commit(
                &alice,
                &ticket.token,
                Commit {
                    byte_size: 2_000,
                    checksum: None,
                },
            )
            .await,
        codes::VALIDATION_FAILED,
    );
    assert_eq!(
        harness.counter(
            "migo_media_upload_refusals_total",
            "reason",
            "size_mismatch"
        ),
        2
    );

    // The truth commits.
    let stored = harness
        .media
        .commit(
            &alice,
            &ticket.token,
            Commit {
                byte_size: 3_000,
                checksum: None,
            },
        )
        .await
        .expect("the real size commits");
    assert_eq!(stored.byte_size, 3_000);
    assert_eq!(
        harness.counter("migo_media_bytes_committed_total", "kind", "image"),
        3_000
    );
}

#[tokio::test]
async fn an_upload_cannot_grow_past_the_ticket_it_was_issued_for() {
    let harness = Harness::new();
    harness.cast().await;
    let alice = caller(ALICE, ALICE_PHONE);

    // A one-kibibyte avatar ticket.
    let ticket = harness
        .media
        .begin(&alice, avatar())
        .await
        .expect("alice begins an upload");

    // The client asks to commit more than the ticket allows, before storage is even
    // consulted. The ticket's number was checked against the kind's ceiling at begin;
    // this is the client trying to raise it after the fact.
    let error = harness
        .media
        .commit(
            &alice,
            &ticket.token,
            Commit {
                byte_size: 64 * 1024 * 1024,
                checksum: None,
            },
        )
        .await
        .expect_err("a commit larger than the ticket is refused");
    assert_eq!(error.code(), codes::UPLOAD_LIMIT_EXCEEDED);
    assert!(error.public_message().contains("1024"));

    // And pushing the oversized bytes anyway does not help: the head is checked against
    // the ticket, so a client that lies about its own size and uploads the truth is
    // refused on the truth.
    harness.push_bytes(&payload(PNG, 64 * 1024));
    expect_code(
        harness
            .media
            .commit(
                &alice,
                &ticket.token,
                Commit {
                    byte_size: 64 * 1024,
                    checksum: None,
                },
            )
            .await,
        codes::UPLOAD_LIMIT_EXCEEDED,
    );
    assert_eq!(
        harness.counter(
            "migo_media_upload_refusals_total",
            "reason",
            "size_mismatch"
        ),
        2
    );
}

#[tokio::test]
async fn a_checksum_is_recorded_and_never_recomputed() {
    let harness = Harness::new();
    harness.cast().await;
    let alice = caller(ALICE, ALICE_PHONE);

    let ticket = harness
        .media
        .begin(&alice, avatar())
        .await
        .expect("alice begins an upload");
    harness.push_bytes(&payload(PNG, 1_024));

    // A checksum longer than the column takes is a client bug, refused by length rather
    // than truncated: a truncated hash is a hash that will not match on download.
    expect_code(
        harness
            .media
            .commit(
                &alice,
                &ticket.token,
                Commit {
                    byte_size: 1_024,
                    checksum: Some(vec![0x11; MAX_CHECKSUM_LEN + 1]),
                },
            )
            .await,
        codes::FIELD_TOO_LONG,
    );

    // A checksum that is nowhere near the bytes is still recorded verbatim. The server
    // holds ciphertext for end-to-end media and could not recompute one in general, so
    // it stores what the client said and lets the client verify after download.
    let claimed = vec![0x42; MAX_CHECKSUM_LEN];
    let stored = harness
        .media
        .commit(
            &alice,
            &ticket.token,
            Commit {
                byte_size: 1_024,
                checksum: Some(claimed.clone()),
            },
        )
        .await
        .expect("a checksum at the ceiling commits");
    assert_eq!(stored.checksum, Some(claimed));
}

#[tokio::test]
async fn the_type_comes_from_the_bytes() {
    let harness = Harness::new();
    harness.cast().await;
    let alice = caller(ALICE, ALICE_PHONE);

    // The client declares a PNG and uploads a JPEG. Brief section 122: do not trust the
    // Content-Type from the client. The row records what the bytes are.
    let ticket = harness
        .media
        .begin(&alice, image_into(CHAT))
        .await
        .expect("alice begins an upload");
    harness.push_bytes(&payload(JPEG, 4_096));
    let stored = harness
        .media
        .commit(
            &alice,
            &ticket.token,
            Commit {
                byte_size: 4_096,
                checksum: None,
            },
        )
        .await
        .expect("a real JPEG commits");

    assert_eq!(
        stored.mime, "image/jpeg",
        "the declared image/png must lose"
    );
    assert_eq!(
        harness.counter("migo_media_content_identified_total", "format", "jpeg"),
        1
    );
}

#[tokio::test]
async fn a_page_dressed_as_a_picture_is_refused() {
    let harness = Harness::new();
    harness.cast().await;
    let alice = caller(ALICE, ALICE_PHONE);

    // The three shapes that turn an image host into a cross-site scripting vector: a
    // page, a doctype, and an SVG. All three are refused outright rather than stored
    // with a corrected type, because there is no type at which they are safe to serve
    // from a domain that holds sessions.
    for bytes in [
        b"<html><body>hello".as_slice(),
        b"<!DOCTYPE html><html>".as_slice(),
        b"<svg xmlns=\"http://www.w3.org/2000/svg\">".as_slice(),
    ] {
        let ticket = harness
            .media
            .begin(&alice, image_into(CHAT))
            .await
            .expect("alice begins an upload");
        harness.storage.upload(
            harness.storage.signed_uploads.lock().last().expect("a key"),
            bytes,
        );
        let error = harness
            .media
            .commit(
                &alice,
                &ticket.token,
                Commit {
                    byte_size: bytes.len() as u64,
                    checksum: None,
                },
            )
            .await
            .expect_err("markup is not a picture");
        assert_eq!(error.code(), codes::UNSUPPORTED_MEDIA_TYPE);
        // The sniffer's reason is disclosed: it is one of six constants, none of which
        // contains anything from the object, and a client that uploaded the wrong file
        // can act on it.
        assert!(
            error.public_message().contains("forbidden"),
            "unexpected message: {}",
            error.public_message()
        );
    }
    assert_eq!(
        harness.counter(
            "migo_media_upload_refusals_total",
            "reason",
            "content_refused"
        ),
        3
    );
    assert_eq!(harness.plain("migo_media_uploads_committed_total"), 0);
}

#[tokio::test]
async fn a_pdf_is_not_an_image_and_says_so_specifically() {
    let harness = Harness::new();
    harness.cast().await;
    let alice = caller(ALICE, ALICE_PHONE);

    let ticket = harness
        .media
        .begin(&alice, image_into(CHAT))
        .await
        .expect("alice begins an upload");
    harness.push_bytes(b"%PDF-1.7\nsomething");
    let error = harness
        .media
        .commit(
            &alice,
            &ticket.token,
            Commit {
                byte_size: 18,
                checksum: None,
            },
        )
        .await
        .expect_err("a PDF is not an image");
    assert_eq!(error.code(), codes::UNSUPPORTED_MEDIA_TYPE);
    // "not_image" rather than "forbidden": a PDF is a real thing this deployment
    // accepts, just not under this kind, and a client can retry it as a document.
    assert!(
        error.public_message().contains("not_image"),
        "unexpected message: {}",
        error.public_message()
    );

    let document = harness
        .media
        .begin(
            &alice,
            UploadRequest {
                kind: MediaKind::Document,
                mime: "application/pdf".to_string(),
                byte_size: 18,
                destination: Destination::Conversation(id(CHAT)),
                width: None,
                height: None,
                duration_ms: None,
            },
        )
        .await
        .expect("a document upload begins");
    harness.storage.upload(
        harness.storage.signed_uploads.lock().last().expect("a key"),
        b"%PDF-1.7\nsomething",
    );
    let stored = harness
        .media
        .commit(
            &alice,
            &document.token,
            Commit {
                byte_size: 18,
                checksum: None,
            },
        )
        .await
        .expect("the same bytes commit as a document");
    // The client's declared type stands for a document, because `Document` is the open
    // set and the one kind this crate never claims to have identified by its bytes.
    assert_eq!(stored.mime, "application/pdf");
}

#[tokio::test]
async fn unrecognisable_bytes_are_refused_for_a_kind_that_should_be_recognisable() {
    let harness = Harness::new();
    harness.cast().await;
    let alice = caller(ALICE, ALICE_PHONE);

    // Not markup and not a known container. An image whose format the server cannot
    // name is not what the client said it was.
    let ticket = harness
        .media
        .begin(&alice, image_into(CHAT))
        .await
        .expect("alice begins an upload");
    harness.push_bytes(b"just some text, honestly");
    expect_code(
        harness
            .media
            .commit(
                &alice,
                &ticket.token,
                Commit {
                    byte_size: 24,
                    checksum: None,
                },
            )
            .await,
        codes::UNSUPPORTED_MEDIA_TYPE,
    );

    // The same bytes are a fine document, and nothing is counted as identified.
    let document = harness
        .media
        .begin(
            &alice,
            UploadRequest {
                kind: MediaKind::Document,
                mime: "text/plain".to_string(),
                byte_size: 24,
                destination: Destination::Conversation(id(CHAT)),
                width: None,
                height: None,
                duration_ms: None,
            },
        )
        .await
        .expect("a document upload begins");
    harness.storage.upload(
        harness.storage.signed_uploads.lock().last().expect("a key"),
        b"just some text, honestly",
    );
    harness
        .media
        .commit(
            &alice,
            &document.token,
            Commit {
                byte_size: 24,
                checksum: None,
            },
        )
        .await
        .expect("an unrecognised document commits");
    assert_eq!(harness.plain("migo_media_content_unidentified_total"), 1);
}

#[tokio::test]
async fn end_to_end_bytes_are_not_sniffed_and_are_cleared_at_once() {
    let harness = Harness::new();
    harness.cast().await;
    let alice = caller(ALICE, ALICE_PHONE);

    // Ciphertext. It matches no signature, and for a server-readable conversation it
    // would be refused as unrecognisable.
    let ciphertext = payload(&[0x9E, 0x3F, 0x00, 0xD1, 0x7B], 4_096);

    let sealed = harness
        .media
        .begin(&alice, image_into(SEALED_CHAT))
        .await
        .expect("an end-to-end upload begins");
    harness.push_bytes(&ciphertext);
    let stored = harness
        .media
        .commit(
            &alice,
            &sealed.token,
            Commit {
                byte_size: 4_096,
                checksum: None,
            },
        )
        .await
        .expect("ciphertext commits into an end-to-end conversation");

    // The declared type stands, because there is nothing to identify: section 122 says
    // the server sees only ciphertext, so content validation is not possible and what
    // it validates instead is size, quota, rate, and authorisation.
    assert_eq!(stored.mime, "image/png");
    // And it is cleared immediately, because nothing will ever scan it. Leaving it
    // pending would delete the feature rather than protect it.
    assert_eq!(stored.scan, Scan::Clean);
    assert_eq!(harness.plain("migo_media_content_unidentified_total"), 0);
    assert_eq!(harness.plain("migo_media_content_identified_total"), 0);

    // The same bytes into the server-readable conversation are refused, which is what
    // makes the exemption above an exemption rather than a hole.
    let open = harness
        .media
        .begin(&alice, image_into(CHAT))
        .await
        .expect("a server-readable upload begins");
    harness.storage.upload(
        harness.storage.signed_uploads.lock().last().expect("a key"),
        &ciphertext,
    );
    expect_code(
        harness
            .media
            .commit(
                &alice,
                &open.token,
                Commit {
                    byte_size: 4_096,
                    checksum: None,
                },
            )
            .await,
        codes::UNSUPPORTED_MEDIA_TYPE,
    );
}

#[tokio::test]
async fn a_server_readable_object_is_scanned_at_commit() {
    let harness = Harness::new();
    harness.cast().await;
    let alice = caller(ALICE, ALICE_PHONE);

    let ticket = harness
        .media
        .begin(&alice, image_into(CHAT))
        .await
        .expect("alice begins an upload");
    harness.push_bytes(&payload(PNG, 4_096));
    let stored = harness
        .media
        .commit(
            &alice,
            &ticket.token,
            Commit {
                byte_size: 4_096,
                checksum: None,
            },
        )
        .await
        .expect("a real PNG commits");

    // The built-in scanner runs on the head the commit already read, so a committed
    // server-readable object is never left pending: the previous design parked every
    // such object at Pending waiting for a scanner that no composition ever wired,
    // which quietly made room media and avatars permanently unservable to anyone but
    // the owner. The row starts from the verdict this scan found; a deployment running
    // a stricter scanner lowers it through `record_scan` afterwards.
    assert_eq!(stored.scan, Scan::Clean);
    assert_eq!(
        harness
            .media
            .status_of(stored.media_id)
            .await
            .expect("the queue answers"),
        Some(Scan::Clean)
    );
    // An avatar is a profile object with no conversation, so there is no encryption
    // mode to read — it is scannable, and the same inline scan clears it.
    let profile = harness
        .media
        .begin(&alice, avatar())
        .await
        .expect("an avatar upload begins");
    harness.storage.upload(
        harness.storage.signed_uploads.lock().last().expect("a key"),
        &payload(PNG, 1_024),
    );
    let face = harness
        .media
        .commit(
            &alice,
            &profile.token,
            Commit {
                byte_size: 1_024,
                checksum: None,
            },
        )
        .await
        .expect("an avatar commits");
    assert_eq!(face.scan, Scan::Clean);
}

// ---------------------------------------------------------------------------
// Resume and abandon
//
// Brief section 168: a failure at eighty per cent resumes from about eighty per cent,
// not from zero.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_interrupted_upload_reports_where_it_stopped() {
    let harness = Harness::new();
    harness.cast().await;
    let alice = caller(ALICE, ALICE_PHONE);

    let mut request = image_into(CHAT);
    request.byte_size = 10_000;
    let ticket = harness
        .media
        .begin(&alice, request)
        .await
        .expect("alice begins an upload");

    // Nothing pushed yet: zero, not an error. A client that lost the answer to begin
    // and asks where it got to is asking a reasonable question.
    let fresh: Progress = harness
        .media
        .status(&alice, &ticket.token)
        .await
        .expect("status answers for an untouched key");
    assert_eq!(fresh.uploaded_bytes, 0);
    assert_eq!(fresh.byte_size, 10_000);
    assert_eq!(fresh.media_id, ticket.media_id);
    assert_eq!(fresh.expires_at, ticket.expires_at);
    assert!(!fresh.is_complete());

    // Eight thousand of ten thousand.
    harness.push_bytes(&payload(PNG, 8_000));
    let partial = harness
        .media
        .status(&alice, &ticket.token)
        .await
        .expect("status answers");
    assert_eq!(partial.uploaded_bytes, 8_000);
    assert!(!partial.is_complete());

    // The rest.
    harness.push_bytes(&payload(PNG, 10_000));
    let done = harness
        .media
        .status(&alice, &ticket.token)
        .await
        .expect("status answers");
    assert!(done.is_complete());
}

#[tokio::test]
async fn an_abandoned_upload_takes_its_bytes_with_it() {
    let harness = Harness::new();
    harness.cast().await;
    let alice = caller(ALICE, ALICE_PHONE);

    let ticket = harness
        .media
        .begin(&alice, avatar())
        .await
        .expect("alice begins an upload");
    let key = harness.storage.only_upload_key();
    harness.push_bytes(&payload(PNG, 512));
    assert!(harness.storage.holds(&key));

    harness
        .media
        .abort(&alice, &ticket.token)
        .await
        .expect("the uploader may abandon it");

    assert_eq!(harness.storage.removed(), vec![key]);
    assert_eq!(harness.plain("migo_media_uploads_aborted_total"), 1);
    // Nothing was ever written, so aborting leaves no row to clean up either.
    assert!(harness
        .store
        .media(ticket.media_id)
        .await
        .expect("the store answers")
        .is_none());

    // Committing afterwards fails on the bytes being gone rather than on some
    // remembered abort flag, because the server kept no state about this upload at all.
    expect_code(
        harness
            .media
            .commit(
                &alice,
                &ticket.token,
                Commit {
                    byte_size: 512,
                    checksum: None,
                },
            )
            .await,
        codes::VALIDATION_FAILED,
    );
}

#[tokio::test]
async fn aborting_somebody_elses_upload_is_not_possible() {
    let harness = Harness::new();
    harness.cast().await;

    let ticket = harness
        .media
        .begin(&caller(ALICE, ALICE_PHONE), image_into(CHAT))
        .await
        .expect("alice begins an upload");
    harness.push_bytes(&payload(PNG, 512));

    expect_code(
        harness
            .media
            .abort(&caller(BOB, BOB_LAPTOP), &ticket.token)
            .await,
        codes::VALIDATION_FAILED,
    );
    assert!(
        harness.storage.removed().is_empty(),
        "a refused abort must not delete anything"
    );
    assert_eq!(harness.plain("migo_media_uploads_aborted_total"), 0);
}

#[tokio::test]
async fn the_same_ticket_committed_twice_is_one_object() {
    let harness = Harness::new();
    harness.cast().await;
    let alice = caller(ALICE, ALICE_PHONE);

    let ticket = harness
        .media
        .begin(&alice, avatar())
        .await
        .expect("alice begins an upload");
    harness.push_bytes(&payload(PNG, 1_024));
    let commit = Commit {
        byte_size: 1_024,
        checksum: None,
    };

    let first = harness
        .media
        .commit(&alice, &ticket.token, commit.clone())
        .await
        .expect("the upload commits");
    // A client that never saw the first answer retries. The id came from the ticket, so
    // the retry lands on the same row rather than making a second object out of one
    // upload -- which is the reason the id is minted at begin and not at commit.
    let second = harness
        .media
        .commit(&alice, &ticket.token, commit)
        .await
        .expect("a retry is idempotent");
    assert_eq!(first.media_id, second.media_id);
    assert_eq!(first.created_at, second.created_at);
}

// ---------------------------------------------------------------------------
// Serving
//
// One refusal for an object a caller may not have and an object that is not there, so
// that a media id is never an existence oracle.
// ---------------------------------------------------------------------------

/// Commits a real object of `kind` into `destination` and returns it.
async fn upload(harness: &Harness, who: &Caller, request: UploadRequest, bytes: &[u8]) -> Stored {
    let ticket = harness
        .media
        .begin(who, request)
        .await
        .expect("the upload begins");
    harness.storage.upload(
        harness
            .storage
            .signed_uploads
            .lock()
            .last()
            .expect("a key was signed"),
        bytes,
    );
    harness
        .media
        .commit(
            who,
            &ticket.token,
            Commit {
                byte_size: bytes.len() as u64,
                checksum: None,
            },
        )
        .await
        .expect("the upload commits")
}

#[tokio::test]
async fn an_owner_can_read_their_own_object_even_when_a_scanner_rejected_it() {
    let harness = Harness::new();
    harness.cast().await;
    let alice = caller(ALICE, ALICE_PHONE);

    let stored = upload(&harness, &alice, image_into(CHAT), &payload(PNG, 4_096)).await;
    assert_eq!(stored.scan, Scan::Clean);
    // The owner exemption has to survive a scanner's rejection, not just the pending
    // window it used to cover: otherwise a false positive from a future scanner means
    // sending a picture and never seeing the picture you sent. A rejection is lowered
    // through the same `record_scan` a deployment's stricter scanner uses.
    harness
        .media
        .record_scan(stored.media_id, Verdict::Rejected, ts(NOW + MINUTE))
        .await
        .expect("a scanner may record a rejection");

    let grant = harness
        .media
        .fetch_url(&alice, stored.media_id)
        .await
        .expect("the owner may fetch their own rejected object");
    assert_eq!(
        grant.expires_at.as_millis(),
        NOW + harness.media.policy().download_ttl_ms
    );
    assert_eq!(
        harness.counter("migo_media_url_grants_total", "outcome", "issued"),
        1
    );
    harness
        .media
        .describe(&alice, stored.media_id)
        .await
        .expect("the owner may describe it");
}

#[tokio::test]
async fn a_member_is_served_a_committed_object_and_blocked_only_by_a_rejection() {
    let harness = Harness::new();
    harness.cast().await;
    let alice = caller(ALICE, ALICE_PHONE);
    let bob = caller(BOB, BOB_LAPTOP);

    let stored = upload(&harness, &alice, image_into(CHAT), &payload(PNG, 4_096)).await;

    // Bob is in the conversation, so he is authorised, and the object was scanned at
    // commit, so he is served it at once. This is the path the old design dead-ended:
    // the object sat Pending forever and a member saw "come back later" for media that
    // had already passed every check this build knows how to run.
    harness
        .media
        .fetch_url(&bob, stored.media_id)
        .await
        .expect("a committed object is served to a member at once");
    assert_eq!(harness.storage.download_count(), 1);

    // A scanner's rejection puts it back out of reach for members, and the refusal
    // says "not cleared" rather than "no such thing": the answer changed on its own
    // once and can change again, so a client shows a retryable state, not a broken
    // image forever.
    harness
        .media
        .record_scan(stored.media_id, Verdict::Rejected, ts(NOW + MINUTE))
        .await
        .expect("a scanner may record a rejection");
    let error = harness
        .media
        .fetch_url(&bob, stored.media_id)
        .await
        .expect_err("a rejected object is not served to a member");
    assert_eq!(error.code(), codes::MEDIA_UNAVAILABLE);
    assert_eq!(
        harness.counter("migo_media_url_grants_total", "outcome", "not_cleared"),
        1
    );
    assert_eq!(harness.storage.download_count(), 1);
    assert_eq!(
        harness.counter("migo_media_scan_results_total", "status", "clean"),
        1
    );
}

#[tokio::test]
async fn a_stranger_and_a_missing_object_are_the_same_answer() {
    let harness = Harness::new();
    harness.cast().await;
    let alice = caller(ALICE, ALICE_PHONE);
    let carol = caller(CAROL, CAROL_PHONE);

    let stored = upload(&harness, &alice, image_into(CHAT), &payload(PNG, 4_096)).await;
    harness
        .media
        .record_scan(stored.media_id, Verdict::Clean, ts(NOW + MINUTE))
        .await
        .expect("a scanner may record a verdict");

    // Carol is in neither the conversation nor the object's history.
    let forbidden = harness
        .media
        .fetch_url(&carol, stored.media_id)
        .await
        .expect_err("a stranger is refused");
    let missing = harness
        .media
        .fetch_url(&carol, id(7_777))
        .await
        .expect_err("an id that names nothing is refused");

    assert_eq!(forbidden.code(), codes::NOT_FOUND);
    assert_eq!(missing.code(), codes::NOT_FOUND);
    assert_eq!(forbidden.public_message(), missing.public_message());

    // Describe answers the same way, so the two endpoints cannot be compared against
    // each other to recover the difference.
    let described = harness
        .media
        .describe(&carol, stored.media_id)
        .await
        .expect_err("a stranger cannot describe it either");
    assert_eq!(described.public_message(), missing.public_message());

    // The counters keep the distinction the client is not given, because an operator
    // watching a spike of one wants to know which one it is.
    assert_eq!(
        harness.counter("migo_media_url_grants_total", "outcome", "denied"),
        2
    );
    assert_eq!(
        harness.counter("migo_media_url_grants_total", "outcome", "missing"),
        1
    );
}

#[tokio::test]
async fn an_avatar_is_readable_by_anybody_who_is_signed_in() {
    let harness = Harness::new();
    harness.cast().await;
    let alice = caller(ALICE, ALICE_PHONE);

    let face = upload(&harness, &alice, avatar(), &payload(PNG, 1_024)).await;
    harness
        .media
        .record_scan(face.media_id, Verdict::Clean, ts(NOW + MINUTE))
        .await
        .expect("a scanner may record a verdict");

    // Carol shares no conversation with Alice at all. An avatar is rendered by everybody
    // who can see the account, so there is no membership question to ask.
    harness
        .media
        .fetch_url(&caller(CAROL, CAROL_PHONE), face.media_id)
        .await
        .expect("an avatar is served to any authenticated account");
    let described = harness
        .media
        .describe(&caller(CAROL, CAROL_PHONE), face.media_id)
        .await
        .expect("an avatar may be described");
    assert_eq!(described.owner_id, id(ALICE));
    assert_eq!(described.kind, MediaKind::Avatar);
}

#[tokio::test]
async fn what_a_client_is_told_about_an_object_leaves_out_the_bucket() {
    let harness = Harness::new();
    harness.cast().await;
    let alice = caller(ALICE, ALICE_PHONE);

    let mut request = image_into(CHAT);
    request.width = Some(800);
    request.height = Some(600);
    let stored = upload(&harness, &alice, request, &payload(PNG, 4_096)).await;

    // Eleven fields, and neither of the two the server keeps to itself: the storage key
    // is the private naming of a private bucket, and the destination conversation is an
    // authorization input, which must not be echoed back to the party being authorized.
    assert_eq!(stored.media_id, stored.media_id);
    assert_eq!(stored.owner_id, id(ALICE));
    assert_eq!(stored.kind, MediaKind::Image);
    assert_eq!(stored.mime, "image/png");
    assert_eq!(stored.byte_size, 4_096);
    assert_eq!(stored.width, Some(800));
    assert_eq!(stored.height, Some(600));
    assert_eq!(stored.duration_ms, None);
    assert_eq!(stored.scan, Scan::Clean);
    assert_eq!(stored.checksum, None);
    assert_eq!(stored.created_at, ts(NOW));

    // The row does carry both, because the server needs them. The projection carries the
    // conversation — the committer named it in their own upload, and the commit handler
    // needs it to tell that conversation the object exists — but never the storage key,
    // which is the private naming of a private bucket. Neither reaches the wire: the
    // commit's wire reply is an `Acknowledged`, full stop.
    assert_eq!(stored.conversation_id, Some(id(CHAT)));
    let row = harness
        .store
        .media(stored.media_id)
        .await
        .expect("the store answers")
        .expect("the row exists");
    assert_eq!(row.conversation_id, Some(id(CHAT)));
    assert!(!row.storage_key.is_empty());
    let rendered = format!("{stored:?}");
    assert!(!rendered.contains(&row.storage_key));
}

// ---------------------------------------------------------------------------
// Deleting and scanning
// ---------------------------------------------------------------------------

#[tokio::test]
async fn only_the_owner_deletes_and_the_row_outlives_the_bytes() {
    let harness = Harness::new();
    harness.cast().await;
    let alice = caller(ALICE, ALICE_PHONE);
    let bob = caller(BOB, BOB_LAPTOP);

    let stored = upload(&harness, &alice, image_into(CHAT), &payload(PNG, 4_096)).await;
    harness
        .media
        .record_scan(stored.media_id, Verdict::Clean, ts(NOW + MINUTE))
        .await
        .expect("a scanner may record a verdict");

    // Bob is in the conversation and can read it, which is not the same as being able
    // to remove it. Deleting somebody else's upload is a moderation action, and a
    // conversation member is not a moderator.
    expect_code(
        harness.media.delete(&bob, stored.media_id).await,
        codes::NOT_FOUND,
    );
    harness
        .media
        .describe(&bob, stored.media_id)
        .await
        .expect("a refused delete changes nothing");

    harness
        .media
        .delete(&alice, stored.media_id)
        .await
        .expect("the owner may delete their own object");
    assert_eq!(harness.plain("migo_media_objects_deleted_total"), 1);

    // A tombstone, not a hole: the row stays so the sweeper can find the bytes, and the
    // bytes are still in the bucket until it does. Deleting them here would make the
    // request wait on object storage for something a background job does better.
    assert!(harness.storage.holds(&harness.storage.only_upload_key()));

    // And a deleted object is gone as far as every reader is concerned, including the
    // owner: the tombstone is not an owner-visible archive.
    expect_code(
        harness.media.fetch_url(&alice, stored.media_id).await,
        codes::NOT_FOUND,
    );
    expect_code(
        harness.media.describe(&alice, stored.media_id).await,
        codes::NOT_FOUND,
    );
    expect_code(
        harness.media.delete(&alice, stored.media_id).await,
        codes::NOT_FOUND,
    );
    assert_eq!(
        harness.plain("migo_media_objects_deleted_total"),
        1,
        "deleting twice deletes once"
    );
}

#[tokio::test]
async fn a_rejected_object_loses_its_bytes_and_keeps_its_row() {
    let harness = Harness::new();
    harness.cast().await;
    let alice = caller(ALICE, ALICE_PHONE);

    let stored = upload(&harness, &alice, image_into(CHAT), &payload(PNG, 4_096)).await;
    let key = harness.storage.only_upload_key();

    harness
        .media
        .record_scan(stored.media_id, Verdict::Rejected, ts(NOW + MINUTE))
        .await
        .expect("a scanner may reject an object");

    assert_eq!(harness.storage.removed(), vec![key.clone()]);
    assert!(!harness.storage.holds(&key));
    // The row stays so a repeat upload of the same checksum is refused without being
    // rescanned, which is the only reason to keep a row whose bytes are gone.
    assert_eq!(
        harness
            .media
            .status_of(stored.media_id)
            .await
            .expect("the queue answers"),
        Some(Scan::Rejected)
    );
    assert_eq!(
        harness.counter("migo_media_scan_results_total", "status", "rejected"),
        1
    );

    // Not servable to anybody but its owner, and the owner's exemption is about the
    // scan not having finished, not about the scan having said no. A rejected object is
    // rejected for everyone.
    expect_code(
        harness
            .media
            .fetch_url(&caller(BOB, BOB_LAPTOP), stored.media_id)
            .await,
        codes::MEDIA_UNAVAILABLE,
    );
}

#[tokio::test]
async fn a_scanner_asked_about_nothing_is_told_so() {
    let harness = Harness::new();
    harness.cast().await;

    expect_code(
        harness
            .media
            .record_scan(id(4_242), Verdict::Clean, ts(NOW))
            .await,
        codes::NOT_FOUND,
    );
    assert_eq!(
        harness
            .media
            .status_of(id(4_242))
            .await
            .expect("the queue answers"),
        None
    );
    // Nothing was counted, because nothing was scanned.
    assert_eq!(harness.plain("migo_media_scan_results_total"), 0);
}

#[tokio::test]
async fn a_scan_verdict_needs_no_caller_and_charges_nobody() {
    let harness = Harness::new();
    harness.cast().await;
    let alice = caller(ALICE, ALICE_PHONE);

    let stored = upload(&harness, &alice, image_into(CHAT), &payload(PNG, 4_096)).await;
    let before = harness.plain("migo_ratelimit_decisions_total");

    // The pipeline is a background job, not a request handler. It has no account to
    // charge and no device to bind, and giving it a `Caller` would mean inventing one.
    for _ in 0..50 {
        harness
            .media
            .record(stored.media_id, Verdict::Clean, ts(NOW + MINUTE))
            .await
            .expect("the queue may record repeatedly");
    }
    assert_eq!(
        harness.plain("migo_ratelimit_decisions_total"),
        before,
        "the scan pipeline must not consume a user's budget"
    );
}

// ---------------------------------------------------------------------------
// The budget
//
// Brief section 70: an upload limit belongs to a user, not a device. One bucket per
// account, so a second device does not buy a second allowance.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_upload_budget_belongs_to_the_account_not_the_device() {
    let harness = Harness::new();
    harness.cast().await;

    // Twenty begins at ten apiece fill an Established account's bucket exactly.
    for _ in 0..20 {
        harness
            .media
            .begin(&caller(ALICE, ALICE_PHONE), avatar())
            .await
            .expect("a begin inside the budget is served");
    }

    // The twenty-first from the same device is refused, and so is the first from a
    // second device: a per-device bucket would let one account upload as many times as
    // it has phones, which is the wrong shape for a limit whose purpose is bounding
    // what one person puts on the disk.
    expect_code(
        harness
            .media
            .begin(&caller(ALICE, ALICE_PHONE), avatar())
            .await,
        codes::RATE_LIMITED,
    );
    expect_code(
        harness
            .media
            .begin(&caller(ALICE, ALICE_LAPTOP), avatar())
            .await,
        codes::RATE_LIMITED,
    );

    // Bob's budget is his own.
    harness
        .media
        .begin(&caller(BOB, BOB_LAPTOP), avatar())
        .await
        .expect("one account's spending is not another's");

    // A rate-limited request shows up in the refusal counter with the substantive
    // refusals, because an operator looking at that panel wants one panel.
    assert_eq!(
        harness.counter("migo_media_upload_refusals_total", "reason", "rate_limited"),
        2
    );
    assert_eq!(
        harness.counter("migo_media_uploads_begun_total", "kind", "avatar"),
        21
    );
}

#[tokio::test]
async fn a_budget_refills_with_time() {
    let harness = Harness::new();
    harness.cast().await;

    for _ in 0..20 {
        harness
            .media
            .begin(&caller(ALICE, ALICE_PHONE), avatar())
            .await
            .expect("a begin inside the budget is served");
    }
    expect_code(
        harness
            .media
            .begin(&caller(ALICE, ALICE_PHONE), avatar())
            .await,
        codes::RATE_LIMITED,
    );

    // A second later the bucket has refilled enough for several more. A limit that did
    // not refill would be a quota, and a quota per thirty minutes is not what section 70
    // asks for.
    let later = Caller::new(
        id(ALICE),
        id(ALICE_PHONE),
        TrustTier::Established,
        ts(NOW + SECOND),
    );
    harness
        .media
        .begin(&later, avatar())
        .await
        .expect("the bucket refills");
}

#[tokio::test]
async fn cheap_calls_cost_less_than_expensive_ones() {
    let harness = Harness::new();
    harness.cast().await;
    let alice = caller(ALICE, ALICE_PHONE);

    let ticket = harness
        .media
        .begin(&alice, avatar())
        .await
        .expect("alice begins an upload");

    // Asking where an upload got to is the cheapest call in the crate, because a client
    // resuming a large upload asks it repeatedly and being rate limited out of a resume
    // would turn a slow network into a failed upload. Ninety-five of the remaining one
    // hundred and ninety units buy ninety-five of them.
    for _ in 0..95 {
        harness
            .media
            .status(&alice, &ticket.token)
            .await
            .expect("status is cheap");
    }
    expect_code(
        harness.media.status(&alice, &ticket.token).await,
        codes::RATE_LIMITED,
    );
}

#[tokio::test]
async fn a_malformed_request_is_charged_for() {
    let harness = Harness::new();
    harness.cast().await;
    let alice = caller(ALICE, ALICE_PHONE);

    // Twenty malformed requests spend the same budget twenty good ones would. The charge
    // is above the validation on purpose: a client sending garbage in a loop is the case
    // the limiter exists for, and validating first would make every refusal free.
    for _ in 0..20 {
        let mut request = image_into(CHAT);
        request.byte_size = 0;
        expect_code(
            harness.media.begin(&alice, request).await,
            codes::VALIDATION_FAILED,
        );
    }
    expect_code(
        harness.media.begin(&alice, image_into(CHAT)).await,
        codes::RATE_LIMITED,
    );
    // And none of them reached storage or the conversation table.
    assert!(harness.storage.signed_uploads.lock().is_empty());
}

// ---------------------------------------------------------------------------
// When object storage is having a bad day
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_bucket_that_is_down_is_reported_and_counted() {
    let harness = Harness::new();
    harness.cast().await;
    let alice = caller(ALICE, ALICE_PHONE);

    // A committed object to fetch later, made while storage still works.
    let stored = upload(&harness, &alice, image_into(CHAT), &payload(PNG, 4_096)).await;
    harness.storage.fail_from_now_on();

    // Begin cannot hand out a ticket for a URL it could not sign.
    harness
        .media
        .begin(&alice, avatar())
        .await
        .expect_err("no signature, no ticket");
    // Fetching cannot hand out a download URL either, and the failure is a storage
    // failure rather than a refusal: the caller was authorised and the object is there.
    harness
        .media
        .fetch_url(&alice, stored.media_id)
        .await
        .expect_err("no signature, no URL");

    assert_eq!(
        harness.counter("migo_media_upload_refusals_total", "reason", "storage"),
        1
    );
    assert_eq!(
        harness.counter("migo_media_url_grants_total", "outcome", "storage"),
        1
    );

    // Describe still answers, because it reads the row and never touches the bucket.
    // A client can still show the file name and size of something it cannot download.
    harness.storage.works_again();
    harness
        .media
        .describe(&alice, stored.media_id)
        .await
        .expect("describing an object needs no bucket");
}

// ---------------------------------------------------------------------------
// Metrics
//
// Brief section 174: no metric may be labelled by an account, a device, a
// conversation, or anything else that names a person. And every series exists before
// anything happens, so a dashboard that reads zero is reading a zero rather than a
// missing series.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn every_series_exists_before_anything_happens() {
    let harness = Harness::new();

    for kind in MediaKind::ALL {
        for series in [
            "migo_media_uploads_begun_total",
            "migo_media_uploads_committed_total",
            "migo_media_bytes_committed_total",
        ] {
            assert_eq!(
                harness
                    .registry
                    .counter(series, "", &[("kind", kind.label())])
                    .get(),
                0,
                "{series} is missing the {} label",
                kind.label()
            );
        }
    }
    for reason in [
        "rate_limited",
        "too_large",
        "denied",
        "too_long",
        "invalid",
        "ticket_expired",
        "ticket_invalid",
        "bytes_missing",
        "size_mismatch",
        "content_refused",
        "storage",
    ] {
        assert_eq!(
            harness.counter("migo_media_upload_refusals_total", "reason", reason),
            0,
            "the {reason} refusal has no series"
        );
    }
    for outcome in [
        "issued",
        "denied",
        "missing",
        "not_cleared",
        "rate_limited",
        "storage",
    ] {
        assert_eq!(
            harness.counter("migo_media_url_grants_total", "outcome", outcome),
            0,
            "the {outcome} grant has no series"
        );
    }
    for status in Scan::ALL {
        assert_eq!(
            harness.counter("migo_media_scan_results_total", "status", status.label()),
            0
        );
    }
    for series in [
        "migo_media_content_unidentified_total",
        "migo_media_objects_deleted_total",
        "migo_media_uploads_aborted_total",
    ] {
        assert_eq!(harness.plain(series), 0, "{series} is missing");
    }

    // A dashboard reading these on a fresh process gets zeroes, not gaps, which is the
    // difference between "nothing was refused" and "the panel is broken".
    let rendered = harness.registry.render();
    for series in [
        "migo_media_uploads_begun_total",
        "migo_media_upload_refusals_total",
        "migo_media_url_grants_total",
        "migo_media_scan_results_total",
    ] {
        assert!(rendered.contains(series), "{series} is not exposed");
    }
}

#[tokio::test]
async fn no_metric_is_labelled_by_a_person_or_an_object() {
    let harness = Harness::new();
    harness.cast().await;
    let alice = caller(ALICE, ALICE_PHONE);

    // Exercise every counter this crate has, so the rendered output below is not empty
    // of the interesting series.
    let stored = upload(&harness, &alice, image_into(CHAT), &payload(PNG, 4_096)).await;
    harness
        .media
        .record_scan(stored.media_id, Verdict::Clean, ts(NOW + MINUTE))
        .await
        .expect("a scanner may record a verdict");
    harness
        .media
        .fetch_url(&alice, stored.media_id)
        .await
        .expect("the owner may fetch it");
    expect_code(
        harness.media.fetch_url(&alice, id(1_234)).await,
        codes::NOT_FOUND,
    );
    let doomed = harness
        .media
        .begin(&alice, avatar())
        .await
        .expect("an upload to abandon");
    harness
        .media
        .abort(&alice, &doomed.token)
        .await
        .expect("it may be abandoned");
    harness
        .media
        .delete(&alice, stored.media_id)
        .await
        .expect("the owner may delete it");

    let rendered = harness.registry.render();
    let key = harness.storage.signed_uploads.lock()[0].clone();
    for forbidden in [
        id(ALICE).to_text(),
        id(ALICE_PHONE).to_text(),
        id(CHAT).to_text(),
        stored.media_id.to_text(),
        key,
        "image/png".to_string(),
    ] {
        assert!(
            !rendered.contains(&forbidden),
            "the metric output names {forbidden}"
        );
    }
    // The series themselves are there, so the assertion above is not passing because
    // nothing was recorded.
    assert!(rendered.contains("migo_media_objects_deleted_total"));
    assert!(rendered.contains("kind=\"image\""));
    assert!(rendered.contains("outcome=\"issued\""));
}

// ---------------------------------------------------------------------------
// Policy arithmetic
// ---------------------------------------------------------------------------

#[test]
fn an_operators_ceiling_lowers_every_kind_and_raises_none() {
    // A tiny deployment: every kind is clamped to the roof, including the ones whose
    // product default is far above it.
    let tiny = Policy::from_config(&MediaConfig {
        max_upload_bytes: 1_000,
        ..MediaConfig::default()
    });
    for kind in MediaKind::ALL {
        assert_eq!(tiny.max_bytes(kind), 1_000);
    }

    // A generous deployment: the roof rises and the rooms do not. An avatar stays two
    // mebibytes whatever the operator allows, because that limit was never about disk.
    let generous = Policy::from_config(&MediaConfig {
        max_upload_bytes: 4 * 1024 * 1024 * 1024,
        ..MediaConfig::default()
    });
    assert_eq!(generous.max_bytes(MediaKind::Avatar), 2 * 1024 * 1024);
    assert_eq!(generous.max_bytes(MediaKind::Image), 16 * 1024 * 1024);
    // And the roof itself is clamped to what this build will hold at all, so a
    // configuration typo cannot ask for a four-gibibyte upload.
    assert_eq!(generous.max_bytes(MediaKind::Video), 128 * 1024 * 1024);

    // Zero is not a usable ceiling, so it is clamped to one byte rather than making
    // every upload impossible in a way that looks like a bug in this crate.
    let zero = Policy::from_config(&MediaConfig {
        max_upload_bytes: 0,
        ..MediaConfig::default()
    });
    assert_eq!(zero.max_bytes(MediaKind::Image), 1);

    // The download TTL comes from configuration in seconds and is used in milliseconds.
    let ttl = Policy::from_config(&MediaConfig {
        signed_url_ttl_seconds: 90,
        ..MediaConfig::default()
    });
    assert_eq!(ttl.download_ttl_ms, 90_000);
}

#[tokio::test]
async fn a_deployment_may_set_its_own_limits() {
    // The policy a deployment installs is the policy enforced, not merely recorded.
    let harness = Harness::new().with_policy(Policy {
        max_bytes: [512; 6],
        ..Policy::default()
    });
    harness.cast().await;
    let alice = caller(ALICE, ALICE_PHONE);

    assert_eq!(harness.media.policy().max_bytes(MediaKind::Image), 512);
    let mut request = image_into(CHAT);
    request.byte_size = 513;
    expect_code(
        harness.media.begin(&alice, request).await,
        codes::UPLOAD_LIMIT_EXCEEDED,
    );
    let mut fits = image_into(CHAT);
    fits.byte_size = 512;
    harness
        .media
        .begin(&alice, fits)
        .await
        .expect("an object inside the deployment's own ceiling is accepted");
}

#[test]
fn a_storage_key_names_the_kind_and_the_day_and_nothing_else() {
    let media_id = id(0x1234);
    let key = storage_key(
        MediaKind::VoiceNote,
        Destination::Conversation(id(CHAT)),
        media_id,
        ts(NOW),
    );

    // Scope, kind, day, id. The date prefix is what lets an operator expire a day's
    // worth of abandoned uploads with one lifecycle rule.
    let parts: Vec<&str> = key.split('/').collect();
    assert_eq!(parts.len(), 4);
    assert_eq!(parts[0], "c");
    assert_eq!(parts[1], "voice_note");
    assert_eq!(parts[2].len(), 10, "a date prefix, not a timestamp");
    assert_eq!(parts[3], media_id.to_text());

    // The conversation is not in the key. An earlier draft encoded it there and parsed
    // it back at download time, which makes authorization depend on string parsing; the
    // destination is a column now and the key is just a name.
    assert!(!key.contains(&id(CHAT).to_text()));

    // Profile media is a different scope, so a bucket listing separates avatars from
    // conversation attachments without reading a database.
    let profile = storage_key(MediaKind::Avatar, Destination::Profile, media_id, ts(NOW));
    assert!(profile.starts_with("p/avatar/"));
}
