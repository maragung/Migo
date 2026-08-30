//! The media service.
//!
//! # The four rules this file exists to enforce
//!
//! **A signed URL is a credential.** Brief section 69: *"Signed URL TIDAK BOLEH ditulis
//! ke log, ke analytics, atau ke crash report, karena URL itu sendiri adalah
//! kredensial"*. Nothing in this file formats a URL, and the only type that holds one
//! wraps it in [`Secret`](migo_core::Secret). Look for `expose` and there is nothing
//! here.
//!
//! **Authorization happens when the URL is issued, not when the row is made.** Brief
//! section 168: *"Otorisasi diperiksa saat URL diterbitkan, bukan hanya saat record
//! dibuat"*. Membership changes — people leave conversations — and an upload authorised
//! last March does not authorise a download today. So [`Media::fetch_url`] re-reads
//! membership every time, and there is no cached decision to go stale.
//!
//! **Pending media is not served to anybody but its uploader.** Brief section 168:
//! *"Media pending TIDAK BOLEH disajikan ke pengguna lain"*. One branch, once, in
//! `authorize`, with the owner's exemption above it.
//!
//! **Existence is not disclosed.** Brief section 48: *"untuk objek yang seharusnya
//! tidak diketahui keberadaannya dijawab NOT_FOUND agar tidak membocorkan eksistensi"*.
//! Every refusal that would otherwise confirm an object exists — wrong conversation,
//! not the owner, tombstoned — is `NOT_FOUND`, and they are all produced by one
//! function so a later change cannot make one of them talkative.
//!
//! # Where these prices come from
//!
//! Brief section 145's media block, copied and not invented: `MEDIA_UPLOAD_BEGIN` 10,
//! `MEDIA_UPLOAD_STATUS` 2, `MEDIA_UPLOAD_COMMIT` 5, `MEDIA_UPLOAD_ABORT` 1,
//! `MEDIA_FETCH_URL` 3. The two operations with no opcode — `describe` and `delete` —
//! are priced by analogy: `describe` costs what a fetch costs, because it is the same
//! authorization work without the signature, and `delete` costs what a commit costs,
//! because it is the other end of the same object's life.
//!
//! # What this crate does not do
//!
//! It does not scan. [`Media::record_scan`] takes a verdict somebody else reached; the
//! scanner is a separate process, because a virus scanner in the request path is a
//! virus scanner that decides how long an upload takes.
//!
//! It does not proxy bytes, thumbnail, transcode, or compute a waveform. Brief section
//! 167 puts the waveform on the client — *"Waveform dihitung di client sebelum enkripsi.
//! Server tidak dapat menghitungnya, dan itu memang tujuannya"* — and the same reasoning
//! disposes of the rest: for end-to-end media the server holds ciphertext, so a server
//! that could thumbnail it would be a server that could read it.
//!
//! It does not enforce a storage quota per account. `MediaStore` has no way to sum an
//! account's objects, and adding one would be a table scan on the upload path; a quota
//! belongs in a periodic job that writes a number this crate could then read cheaply.
//! Saying so here is better than a `TODO`, and better than a quota check that silently
//! costs a full scan per upload.

use std::sync::Arc;

use async_trait::async_trait;
use migo_core::config::MediaConfig;
use migo_core::metrics::Registry;
use migo_core::{Error, Id, Random, Result, Timestamp};
use migo_crypto::mac::{MacKey, LABEL_MEDIA_URL};
use migo_protocol::{codes, fault, EncryptionMode};
use migo_ratelimit::{BucketKey, RateLimiter, SharedRateLimiter};
use migo_store::model::MediaObject;
use migo_store::{SharedStore, Store};
use parking_lot::Mutex;

use crate::metrics::{Granted, Meters, Refused};
use crate::model::{
    Caller, Commit, Destination, Grant, MediaKind, Policy, Progress, Scan, Stored, Ticket,
    UploadRequest, Verdict, MAX_CHECKSUM_LEN, MAX_MIME_LEN, SNIFF_BYTES,
};
use crate::sniff;
use crate::ticket::{self, Claim};
use crate::traits::{storage_key, Library, ScanQueue, SharedLibrary, SharedStorage, Storage};

/// Cost of `MEDIA_UPLOAD_BEGIN`, brief section 145 opcode 128.
const BEGIN_COST: u32 = 10;
/// Cost of `MEDIA_UPLOAD_STATUS`, brief section 145 opcode 129.
const STATUS_COST: u32 = 2;
/// Cost of `MEDIA_UPLOAD_COMMIT`, brief section 145 opcode 130.
const COMMIT_COST: u32 = 5;
/// Cost of `MEDIA_UPLOAD_ABORT`, brief section 145 opcode 131.
const ABORT_COST: u32 = 1;
/// Cost of `MEDIA_FETCH_URL`, brief section 145 opcode 132.
const FETCH_COST: u32 = 3;
/// Cost of `describe`: the same authorization work as a fetch, without the signature.
const DESCRIBE_COST: u32 = 3;
/// Cost of `delete`: the other end of a commit's life, priced the same.
const DELETE_COST: u32 = 5;

/// Media uploads, signed URLs, and the authorization checked before one is issued.
///
/// Generic over the store, the limiter, and object storage, all `?Sized` with `dyn`
/// defaults, so a test can substitute a memory store without the composition root
/// naming three concrete types.
pub struct Media<S: ?Sized = dyn Store, L: ?Sized = dyn RateLimiter, B: ?Sized = dyn Storage> {
    store: Arc<S>,
    limiter: Arc<L>,
    storage: Arc<B>,
    /// Authenticates upload tickets. See [`crate::ticket`].
    tickets: MacKey,
    policy: Policy,
    /// Mints object ids.
    ///
    /// A `Mutex` around a boxed generator, matching every other service in the tree. The
    /// lock is taken, an id is produced, and the guard is dropped inside one statement —
    /// it is never held across an `await`, because a lock held across a yield point in an
    /// async runtime is a deadlock waiting for the right interleaving.
    random: Mutex<Box<dyn Random>>,
    meters: Meters,
}

impl<S, L, B> Media<S, L, B>
where
    S: Store + ?Sized,
    L: RateLimiter + ?Sized,
    B: Storage + ?Sized,
{
    /// Builds a service.
    ///
    /// `root_secret` is the deployment signing secret, from which the ticket key is
    /// derived with `LABEL_MEDIA_URL`. `migod` refuses to start in production with an
    /// empty or default secret, so this constructor does not have to check.
    pub fn new(
        store: Arc<S>,
        limiter: Arc<L>,
        storage: Arc<B>,
        random: Box<dyn Random>,
        root_secret: &[u8],
        config: &MediaConfig,
        registry: &Registry,
    ) -> Self {
        Self {
            store,
            limiter,
            storage,
            tickets: MacKey::derive(root_secret, LABEL_MEDIA_URL),
            policy: Policy::from_config(config),
            random: Mutex::new(random),
            meters: Meters::new(registry),
        }
    }

    /// Replaces the derived limits, for a deployment that sets them itself.
    #[must_use]
    pub fn with_policy(mut self, policy: Policy) -> Self {
        self.policy = policy;
        self
    }

    /// The limits in force.
    #[must_use]
    pub const fn policy(&self) -> &Policy {
        &self.policy
    }

    /// Refuses a request that carries no identity.
    ///
    /// Before the charge, deliberately. `charge` keys its bucket on the account id, so a
    /// request with `Id::NIL` would be billed to a bucket every unidentified request in
    /// the deployment shares -- which turns an anonymous request into a way to exhaust a
    /// budget nobody owns. And a profile upload asks no membership question at all, so
    /// without this an unauthenticated caller would walk out with a signed URL and a
    /// MAC-authenticated ticket for a real storage key.
    ///
    /// The device matters as much as the account: brief section 69 asks for an
    /// attachment token *"yang terikat pada account dan device"*, and a ticket bound to
    /// `Id::NIL` as its device is a ticket bound to whatever presents it.
    fn require_identity(caller: &Caller) -> Result<()> {
        if caller.account_id.is_nil() || caller.device_id.is_nil() {
            return Err(fault::unauthenticated(
                "the media library needs an identified account and device",
            ));
        }
        Ok(())
    }

    /// Charges the caller's bucket.
    ///
    /// One key, the account. Not the device: brief section 70 gives a *user* an upload
    /// limit, and a per-device bucket would let one account upload as many times as it
    /// has devices — which is the wrong shape for a limit whose purpose is bounding what
    /// one person can put on the disk.
    async fn charge(&self, caller: &Caller, cost: u32) -> Result<()> {
        let keys = [BucketKey::account_write(caller.account_id)];
        self.limiter
            .charge(&keys, cost, caller.tier, caller.now)
            .await?
            .into_result()
    }

    /// Charges, and counts a refusal as one.
    ///
    /// Every entry point starts with this rather than with `charge` directly, so a
    /// rate-limited request shows up in the refusal counter alongside the substantive
    /// refusals. An operator looking at `migo_media_upload_refusals_total` wants one
    /// panel, not one panel and a note about a second series somewhere else.
    async fn charge_upload(&self, caller: &Caller, cost: u32) -> Result<()> {
        self.charge(caller, cost).await.inspect_err(|error| {
            if error.code() == codes::RATE_LIMITED {
                self.meters.refused(Refused::RateLimited);
            }
        })
    }

    /// Mints an id at `at`.
    fn mint(&self, at: Timestamp) -> Id {
        let mut random = self.random.lock();
        Id::generate_at(at, random.as_mut())
    }

    /// Checks that the caller may put an object at `destination`.
    ///
    /// A conversation destination is a membership question and nothing else — the same
    /// question `migo-messaging` asks before accepting a message, and the reason a room
    /// needs no separate branch, since joining a room inserts a `conversation_member`
    /// row. A profile destination is checked against nothing: an account may always
    /// upload its own avatar.
    ///
    /// Returns whether the destination is end-to-end, which the ticket carries so commit
    /// does not read the conversation again.
    async fn admit(&self, caller: &Caller, destination: Destination) -> Result<bool> {
        let Destination::Conversation(conversation_id) = destination else {
            return Ok(false);
        };
        let Some(conversation) = self.store.conversation(conversation_id).await? else {
            // NOT_FOUND rather than PERMISSION_DENIED: a caller who is not in a
            // conversation should not learn whether the id they tried names one.
            self.meters.refused(Refused::Denied);
            return Err(fault::not_found("conversation"));
        };
        if !self
            .store
            .is_member(conversation_id, caller.account_id)
            .await?
        {
            self.meters.refused(Refused::Denied);
            return Err(fault::not_found("conversation"));
        }
        Ok(conversation.encryption == EncryptionMode::EndToEnd)
    }

    /// Checks a request's own numbers, before anything is read or written.
    ///
    /// Ordered cheapest first, which is also most-likely-to-be-hostile first: a
    /// malformed MIME type costs a length check, and a caller sending those in a loop
    /// should not reach the database.
    fn vet(&self, request: &UploadRequest) -> Result<()> {
        if request.mime.trim().is_empty() {
            self.meters.refused(Refused::Invalid);
            return Err(fault::field_required("mime"));
        }
        if request.mime.len() > MAX_MIME_LEN {
            self.meters.refused(Refused::Invalid);
            return Err(fault::field_too_long("mime", MAX_MIME_LEN));
        }
        if request.byte_size == 0 {
            self.meters.refused(Refused::Invalid);
            return Err(fault::validation("byte_size", "must be greater than zero"));
        }
        let ceiling = self.policy.max_bytes(request.kind);
        if request.byte_size > ceiling {
            self.meters.refused(Refused::TooLarge);
            return Err(too_large(ceiling));
        }
        if request.kind == MediaKind::VoiceNote {
            if let Some(duration_ms) = request.duration_ms {
                if duration_ms > self.policy.voice_note_max_ms {
                    self.meters.refused(Refused::TooLong);
                    return Err(fault::validation(
                        "duration_ms",
                        "longer than this deployment allows for a voice note",
                    ));
                }
            }
        }
        Ok(())
    }

    /// Verifies a ticket and checks that this caller may use it.
    ///
    /// Brief section 69 asks for an attachment token *"terikat pada account dan
    /// device"*, so both are checked, and both failures produce the same refusal the
    /// MAC failure produces. A ticket presented by the wrong device is
    /// indistinguishable, to the caller, from a ticket that did not verify: there is
    /// nothing an honest client can do differently with the distinction, and telling a
    /// thief which half of the binding it got wrong is a hint.
    fn claim_of(&self, caller: &Caller, token: &[u8]) -> Result<Claim> {
        // The two rejections are separate counters because they mean different things:
        // an expired ticket is a slow network, an unusable one is a forgery or a key
        // that rotated. Both become the same VALIDATION_FAILED for the client.
        let claim = ticket::open(&self.tickets, token, caller.now).map_err(|rejection| {
            self.meters.refused(match rejection {
                ticket::Rejection::Expired => Refused::TicketExpired,
                ticket::Rejection::Unusable => Refused::TicketInvalid,
            });
            Error::from(rejection)
        })?;
        if claim.owner_id != caller.account_id || claim.device_id != caller.device_id {
            self.meters.refused(Refused::TicketInvalid);
            return Err(fault::validation("upload_ticket", "unusable"));
        }
        Ok(claim)
    }

    /// The single refusal for an object a caller may not have, or that is not there.
    ///
    /// See the module docs. Every caller of this function has already decided the answer
    /// is no; funnelling them through one constructor is what stops one of them from
    /// being more forthcoming than the others.
    fn hidden(&self, outcome: Granted) -> Error {
        self.meters.granted(outcome);
        fault::not_found("media")
    }

    /// Reads an object and decides whether `caller` may have it.
    ///
    /// The order is the order in the [`Library::fetch_url`] documentation, and it
    /// matters: the owner's exemption is above the scan check, so somebody whose own
    /// upload is still being scanned can still see it, and the membership check is above
    /// the scan check, so a stranger learns nothing about an object's scan state.
    async fn authorize(&self, caller: &Caller, media_id: Id) -> Result<MediaObject> {
        let Some(object) = self.store.media(media_id).await? else {
            return Err(self.hidden(Granted::Missing));
        };
        if object.deleted_at.is_some() {
            return Err(self.hidden(Granted::Missing));
        }
        if object.owner_id == caller.account_id {
            return Ok(object);
        }
        match Destination::of_column(object.conversation_id) {
            // Profile media. Any authenticated account may render an avatar, and there
            // is no conversation for a membership check to read.
            Destination::Profile => {}
            Destination::Conversation(conversation_id) => {
                if !self
                    .store
                    .is_member(conversation_id, caller.account_id)
                    .await?
                {
                    return Err(self.hidden(Granted::Denied));
                }
            }
        }
        Ok(object)
    }
}

/// The refusal for an object larger than its kind allows.
///
/// `UPLOAD_LIMIT_EXCEEDED` rather than `FIELD_TOO_LONG`: the client is not malformed,
/// it is over quota, and the two have different `ErrorClass` values and therefore
/// different retry semantics. The ceiling is disclosed, because a client that cannot
/// learn the limit cannot compress to fit it.
fn too_large(ceiling: u64) -> Error {
    fault::error(
        codes::UPLOAD_LIMIT_EXCEEDED,
        format!("declared size exceeds the {ceiling} byte ceiling for this kind"),
    )
    .public(format!("largest accepted size is {ceiling} bytes"))
}

/// The refusal for bytes that are not what they were declared to be.
///
/// The sniffer's reason is disclosed. It is one of six constants, none of which contains
/// anything from the object, and a client that uploaded a PDF as an avatar can act on
/// "not_image" in a way it cannot act on a generic refusal.
fn wrong_content(reason: sniff::Refusal) -> Error {
    fault::error(
        codes::UNSUPPORTED_MEDIA_TYPE,
        format!("content refused: {}", reason.label()),
    )
    .public(reason.label().to_string())
}

/// The refusal for an object that exists, is authorised, and is not cleared to serve.
///
/// Not `NOT_FOUND`: the caller is a member of the conversation and knows the object is
/// there — a message referenced it. `MEDIA_UNAVAILABLE` is the honest answer, and it is
/// retryable, which is what a client showing "scanning…" needs.
fn not_cleared() -> Error {
    fault::error(
        codes::MEDIA_UNAVAILABLE,
        "object has not been cleared for delivery",
    )
    .public("not available yet".to_string())
}

#[async_trait]
impl<S, L, B> Library for Media<S, L, B>
where
    S: Store + ?Sized + 'static,
    L: RateLimiter + ?Sized + 'static,
    B: Storage + ?Sized + 'static,
{
    async fn begin(&self, caller: &Caller, request: UploadRequest) -> Result<Ticket> {
        Self::require_identity(caller)?;
        self.charge_upload(caller, BEGIN_COST).await?;
        self.vet(&request)?;
        let end_to_end = self.admit(caller, request.destination).await?;

        let media_id = self.mint(caller.now);
        let key = storage_key(request.kind, request.destination, media_id, caller.now);
        let expires_at = caller.now.saturating_add_millis(self.policy.ticket_ttl_ms);

        let upload = self
            .storage
            .sign_upload(&key, request.byte_size, expires_at)
            .await
            .inspect_err(|_| self.meters.refused(Refused::Storage))?;

        let claim = Claim {
            media_id,
            owner_id: caller.account_id,
            device_id: caller.device_id,
            destination: request.destination,
            kind: request.kind,
            byte_size: request.byte_size,
            expires_at,
            end_to_end,
            mime: request.mime,
            width: request.width,
            height: request.height,
            duration_ms: request.duration_ms,
        };
        self.meters.begun(request.kind);
        Ok(Ticket {
            media_id,
            token: ticket::seal(&self.tickets, &claim),
            upload,
            chunk_bytes: self.policy.chunk_bytes,
            expires_at,
        })
    }

    async fn status(&self, caller: &Caller, token: &[u8]) -> Result<Progress> {
        Self::require_identity(caller)?;
        self.charge_upload(caller, STATUS_COST).await?;
        let claim = self.claim_of(caller, token)?;
        let key = storage_key(
            claim.kind,
            claim.destination,
            claim.media_id,
            // The key was built at begin, from a time this claim does not carry. It does
            // carry the expiry, and the ticket lifetime is a constant, so the original
            // moment is recoverable exactly. Storing the key in the ticket instead would
            // work and would make the token longer for a value that is derivable.
            claim
                .expires_at
                .saturating_add_millis(-self.policy.ticket_ttl_ms),
        );
        let uploaded_bytes = self
            .storage
            .uploaded_bytes(&key)
            .await
            .inspect_err(|_| self.meters.refused(Refused::Storage))?
            .unwrap_or(0);
        Ok(Progress {
            media_id: claim.media_id,
            uploaded_bytes,
            byte_size: claim.byte_size,
            expires_at: claim.expires_at,
        })
    }

    async fn commit(&self, caller: &Caller, token: &[u8], commit: Commit) -> Result<Stored> {
        Self::require_identity(caller)?;
        self.charge_upload(caller, COMMIT_COST).await?;
        let claim = self.claim_of(caller, token)?;

        if let Some(checksum) = &commit.checksum {
            if checksum.len() > MAX_CHECKSUM_LEN {
                self.meters.refused(Refused::Invalid);
                return Err(fault::field_too_long("checksum", MAX_CHECKSUM_LEN));
            }
        }
        if commit.byte_size > claim.byte_size {
            // Over the size the ticket was issued for. The ticket's number was checked
            // against the ceiling at begin; this is the client trying to raise it after
            // the fact, which is exactly what the MAC is for.
            self.meters.refused(Refused::SizeMismatch);
            return Err(too_large(claim.byte_size));
        }

        let key = storage_key(
            claim.kind,
            claim.destination,
            claim.media_id,
            claim
                .expires_at
                .saturating_add_millis(-self.policy.ticket_ttl_ms),
        );
        let Some(head) = self
            .storage
            .head(&key, SNIFF_BYTES)
            .await
            .inspect_err(|_| self.meters.refused(Refused::Storage))?
        else {
            self.meters.refused(Refused::BytesMissing);
            return Err(fault::validation("upload", "no bytes were uploaded"));
        };
        if head.byte_size != commit.byte_size {
            // The client and storage disagree. Storage is the authority — it is the one
            // holding the bytes — and the disagreement is reported rather than papered
            // over, because a client that miscounts its own upload is a client whose
            // checksum this row is about to record.
            self.meters.refused(Refused::SizeMismatch);
            return Err(fault::validation(
                "byte_size",
                "does not match what storage received",
            ));
        }
        if head.byte_size > claim.byte_size {
            self.meters.refused(Refused::SizeMismatch);
            return Err(too_large(claim.byte_size));
        }

        // Brief section 122: the type comes from the bytes, not from the header the
        // client sent. For end-to-end media the bytes are ciphertext and there is
        // nothing to identify, which section 122 states as a consequence rather than a
        // gap: "Untuk media E2E, server hanya melihat ciphertext, sehingga validasi isi
        // tidak mungkin dilakukan server. Yang tetap divalidasi server adalah ukuran,
        // kuota, laju, dan otorisasi."
        let mime = if claim.server_readable() {
            match sniff::identify(head.bytes(), head.head_len, claim.kind) {
                Ok(Some(identified)) => {
                    self.meters
                        .identified(sniff_identity(head.bytes(), head.head_len));
                    identified.to_string()
                }
                Ok(None) => {
                    self.meters.identified(None);
                    claim.mime.clone()
                }
                Err(reason) => {
                    self.meters.refused(Refused::WrongContent);
                    return Err(wrong_content(reason));
                }
            }
        } else {
            claim.mime.clone()
        };

        // Brief section 168: server-readable media carries a scan status, and pending
        // media must not be served to anyone but its owner. E2E media is ciphertext the
        // server cannot scan, so it is clean by construction. For server-readable media
        // the built-in scanner runs right here, on the head the commit already read: the
        // signature verdict is available at this moment, the bytes are in hand, and a
        // deployment with a slower scanner (an ML pass, say) can lower the verdict again
        // through `record_scan` — never raise it — because the row starts from the truth
        // this scan found rather than from a pending placeholder nobody was ever
        // scheduled to fill. A refusal here never becomes a row at all: the commit fails
        // with the reason, which is what a client can act on.
        let scan = if claim.end_to_end {
            Policy::clearance_at_commit(EncryptionMode::EndToEnd)
        } else {
            match sniff::sniff(head.bytes(), head.head_len) {
                sniff::Verdict::Identified(_) => {
                    // The same series the async pipeline's `record_scan` counts: every
                    // verdict this build reaches is counted, whichever path reached it.
                    self.meters.scanned(Scan::Clean);
                    Scan::Clean
                }
                // Polyglot HTML and SVG are refused outright: the object never becomes
                // a row, because the moment it existed it would be one `fetch_url` away
                // from being rendered as a page in somebody's browser origin.
                sniff::Verdict::Refused(sniff::Refusal::Forbidden) => {
                    self.meters.refused(Refused::WrongContent);
                    return Err(wrong_content(sniff::Refusal::Forbidden));
                }
                // Unrecognised bytes reached here only as a Document (every other kind
                // already refused them in the identify gate above, and the Empty case
                // cannot — a zero-byte upload fails the head read). A text document has
                // no magic bytes to find; the scanner found nothing wrong, which is
                // exactly what Clean records. A deployment running a stricter scanner
                // lowers the verdict later through `record_scan`, never raises it.
                sniff::Verdict::Refused(_) => Scan::Clean,
            }
        };

        // A client that never saw the answer to its first commit retries it. The id was
        // minted at begin, so the retry is the same row rather than a second object made
        // out of one upload, and it is answered from the row rather than refused: a
        // client that cannot learn the id of bytes it successfully uploaded has lost
        // them, and the ticket is the proof it is the same upload. The counters are not
        // touched on the way out, so one upload is committed exactly once no matter how
        // many times the answer was lost.
        if let Some(existing) = self.store.media(claim.media_id).await? {
            if existing.deleted_at.is_some() {
                // Committed, then deleted, then retried. There is nothing to hand back,
                // and saying so is honest rather than resurrecting a tombstone.
                return Err(fault::not_found("media object"));
            }
            if existing.owner_id != claim.owner_id {
                // The id space is the server's own and every id is minted from the
                // random source, so a valid ticket cannot reach somebody else's row.
                // If it ever does, it is a collision, not an idempotent retry.
                return Err(fault::already_exists("media object"));
            }
            return Ok(project(&existing));
        }

        let stored = self
            .store
            .create_media(MediaObject {
                media_id: claim.media_id,
                owner_id: claim.owner_id,
                kind: claim.kind.to_i16(),
                mime,
                byte_size: i64::try_from(head.byte_size).unwrap_or(i64::MAX),
                // The client's own description, as it stood at begin. The server never
                // measured these and does not claim to: they are what a chat client
                // needs to lay out a bubble before the bytes arrive, and taking them
                // from the ticket rather than from the commit request is what stops a
                // voice note from changing its duration after the ceiling was checked.
                width: claim.width.and_then(|value| i32::try_from(value).ok()),
                height: claim.height.and_then(|value| i32::try_from(value).ok()),
                duration_ms: claim
                    .duration_ms
                    .and_then(|value| i32::try_from(value).ok()),
                storage_key: key,
                conversation_id: claim.destination.conversation_id(),
                checksum: commit.checksum,
                scan_status: scan.to_i16(),
                created_at: caller.now,
                deleted_at: None,
            })
            .await?;

        self.meters.committed(claim.kind, head.byte_size);
        Ok(project(&stored))
    }

    async fn abort(&self, caller: &Caller, token: &[u8]) -> Result<()> {
        Self::require_identity(caller)?;
        self.charge_upload(caller, ABORT_COST).await?;
        let claim = self.claim_of(caller, token)?;
        let key = storage_key(
            claim.kind,
            claim.destination,
            claim.media_id,
            claim
                .expires_at
                .saturating_add_millis(-self.policy.ticket_ttl_ms),
        );
        self.storage
            .remove(&key)
            .await
            .inspect_err(|_| self.meters.refused(Refused::Storage))?;
        self.meters.aborted();
        Ok(())
    }

    async fn fetch_url(&self, caller: &Caller, media_id: Id) -> Result<Grant> {
        Self::require_identity(caller)?;
        self.charge(caller, FETCH_COST).await.inspect_err(|error| {
            if error.code() == codes::RATE_LIMITED {
                self.meters.granted(Granted::RateLimited);
            }
        })?;
        let object = self.authorize(caller, media_id).await?;
        if object.owner_id != caller.account_id && Scan::of_i16(object.scan_status) != Scan::Clean {
            self.meters.granted(Granted::NotCleared);
            return Err(not_cleared());
        }
        let expires_at = caller
            .now
            .saturating_add_millis(self.policy.download_ttl_ms);
        let grant = self
            .storage
            .sign_download(&object.storage_key, expires_at)
            .await
            .inspect_err(|_| self.meters.granted(Granted::Storage))?;
        self.meters.granted(Granted::Issued);
        Ok(grant)
    }

    async fn describe(&self, caller: &Caller, media_id: Id) -> Result<Stored> {
        Self::require_identity(caller)?;
        self.charge(caller, DESCRIBE_COST).await?;
        let object = self.authorize(caller, media_id).await?;
        Ok(project(&object))
    }

    async fn delete(&self, caller: &Caller, media_id: Id) -> Result<()> {
        Self::require_identity(caller)?;
        self.charge(caller, DELETE_COST).await?;
        let Some(object) = self.store.media(media_id).await? else {
            return Err(self.hidden(Granted::Missing));
        };
        // Only the owner, and a non-owner is told NOT_FOUND. Deleting somebody else's
        // upload is a moderation action, which lives in migo-moderation with an audit
        // entry behind it; a conversation member is not a moderator.
        if object.owner_id != caller.account_id || object.deleted_at.is_some() {
            return Err(self.hidden(Granted::Missing));
        }
        self.store.delete_media(media_id, caller.now).await?;
        self.meters.deleted();
        Ok(())
    }

    async fn record_scan(&self, media_id: Id, verdict: Verdict, at: Timestamp) -> Result<()> {
        let Some(object) = self.store.media(media_id).await? else {
            return Err(fault::not_found("media"));
        };
        let status = verdict.status();
        self.store
            .set_media_scan_status(media_id, status.to_i16(), at)
            .await?;
        if verdict == Verdict::Rejected {
            // The bytes go, the row stays. `media_scan::REJECTED` says why: "The bytes
            // are deleted; the row stays so a repeat upload of the same checksum can be
            // refused without rescanning." Removing the bytes after the status is
            // written, not before, so a crash between the two leaves an object that is
            // marked unservable rather than one that is servable and gone.
            self.storage.remove(&object.storage_key).await?;
        }
        self.meters.scanned(status);
        Ok(())
    }
}

#[async_trait]
impl<S, L, B> ScanQueue for Media<S, L, B>
where
    S: Store + ?Sized + 'static,
    L: RateLimiter + ?Sized + 'static,
    B: Storage + ?Sized + 'static,
{
    async fn record(&self, media_id: Id, verdict: Verdict, at: Timestamp) -> Result<()> {
        Library::record_scan(self, media_id, verdict, at).await
    }

    async fn status_of(&self, media_id: Id) -> Result<Option<Scan>> {
        Ok(self
            .store
            .media(media_id)
            .await?
            .map(|object| Scan::of_i16(object.scan_status)))
    }
}

/// Projects a stored row into what a client is told.
///
/// Drops `storage_key` and `conversation_id`. See [`Stored`] for why.
fn project(object: &MediaObject) -> Stored {
    Stored {
        media_id: object.media_id,
        owner_id: object.owner_id,
        kind: MediaKind::of_i16(object.kind),
        mime: object.mime.clone(),
        byte_size: u64::try_from(object.byte_size).unwrap_or(0),
        width: object.width.and_then(|value| u32::try_from(value).ok()),
        height: object.height.and_then(|value| u32::try_from(value).ok()),
        duration_ms: object
            .duration_ms
            .and_then(|value| u32::try_from(value).ok()),
        scan: Scan::of_i16(object.scan_status),
        checksum: object.checksum.clone(),
        created_at: object.created_at,
        conversation_id: object.conversation_id,
    }
}

/// The identity behind an `Ok(Some(mime))`, for the metric.
///
/// `sniff::identify` returns the canonical MIME string, which is what the row needs;
/// the metric wants the enum. Sniffing twice is a dozen byte comparisons and keeps
/// `identify`'s signature about the one thing its caller needs.
fn sniff_identity(head: &[u8], len: usize) -> Option<sniff::Identity> {
    match sniff::sniff(head, len) {
        sniff::Verdict::Identified(identity) => Some(identity),
        sniff::Verdict::Refused(_) => None,
    }
}

/// Builds the media service the composition root shares.
///
/// # Errors
///
/// None. The signature returns a value rather than a `Result` because every failure a
/// media service can have — no bucket, bad credentials, unreachable endpoint — belongs
/// to the [`Storage`] implementation, which the caller has already constructed.
#[must_use]
pub fn open(
    store: SharedStore,
    limiter: SharedRateLimiter,
    storage: SharedStorage,
    random: Box<dyn Random>,
    root_secret: &[u8],
    config: &MediaConfig,
    registry: &Registry,
) -> SharedLibrary {
    Arc::new(Media::new(
        store,
        limiter,
        storage,
        random,
        root_secret,
        config,
        registry,
    ))
}
