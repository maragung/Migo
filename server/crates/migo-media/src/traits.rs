//! What this crate needs from object storage, and what it offers the layer above.
//!
//! # Why storage is a trait and not an S3 client
//!
//! Brief section 168 is unambiguous about one thing: *"Chat server TIDAK BOLEH menjadi
//! proxy byte media"*. Bytes go from the client to object storage and back, and the
//! server's only involvement is minting signed URLs and asking storage three small
//! questions. That is a narrow enough surface — five methods — that putting an S3 SDK
//! in this crate would be building an integration where an interface will do.
//!
//! It also keeps the crate testable without a bucket, keeps the S3 credentials in the
//! composition root where every other credential lives, and lets a development
//! deployment use `MediaBackend::Filesystem` without this crate knowing there is such a
//! thing.
//!
//! # The one thing [`Storage`] must never be asked for
//!
//! There is no `get(key) -> Vec<u8>` and there never will be. The closest thing is
//! [`Storage::head`], which reads a bounded prefix for content sniffing, and its bound
//! is a parameter the caller sets from [`crate::model::SNIFF_BYTES`]. If a method that
//! returned an object's body appeared on this trait, the byte-proxy rule would be one
//! careless call site away from being broken, and it would be broken in the crate that
//! is supposed to enforce it.

use async_trait::async_trait;
use migo_core::{Id, Result, Timestamp};

use crate::model::{
    Caller, Commit, Destination, Grant, MediaKind, Progress, Scan, Stored, Ticket, UploadRequest,
    Verdict,
};

/// Where the leading bytes of an object came back from, and how many there are.
///
/// A fixed-size buffer rather than a `Vec`, because the only caller wants at most
/// [`crate::model::SNIFF_BYTES`] and a heap allocation per commit to hold thirty-two
/// bytes is the kind of thing that shows up in a profile of an upload-heavy hour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Head {
    /// The object's total size in bytes, as storage knows it.
    ///
    /// The number the commit path checks the client's claim against. Brief section 168:
    /// *"server memverifikasi ukuran serta content hash"*. The server can verify the
    /// size because storage knows it; the hash it can only record, because for
    /// end-to-end media the server holds ciphertext and hashing that would answer a
    /// question nobody asked.
    pub byte_size: u64,
    /// The leading bytes, for [`crate::sniff()`].
    pub head: [u8; crate::model::SNIFF_BYTES],
    /// How many of `head` are real.
    pub head_len: usize,
}

impl Head {
    /// A head with no bytes read, for a backend that cannot read a prefix.
    ///
    /// A backend returning this is saying "I know the size, I cannot show you the
    /// front". The commit path then treats content as unidentified, which for
    /// `Document` is allowed and for every other kind is a refusal — the conservative
    /// direction, and the one that makes a backend's inability to read a prefix a
    /// visible operational problem rather than a silent hole in a check.
    #[must_use]
    pub const fn sized(byte_size: u64) -> Self {
        Self {
            byte_size,
            head: [0u8; crate::model::SNIFF_BYTES],
            head_len: 0,
        }
    }

    /// The bytes that were actually read.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.head[..self.head_len.min(self.head.len())]
    }
}

/// Object storage, as this crate needs it.
///
/// Implemented once per backend in the composition root. Every method takes a storage
/// key this crate minted, so no implementation has to validate or interpret one.
#[async_trait]
pub trait Storage: Send + Sync {
    /// Signs a URL a client may upload to.
    ///
    /// `byte_size` is the size the ticket was issued for, so a backend that can bind
    /// the signature to a content length does. `expires_at` is absolute rather than a
    /// duration because the ticket already carries an absolute expiry and two clocks
    /// disagreeing about a relative one is a class of bug worth not having.
    ///
    /// # Errors
    ///
    /// Whatever the backend fails with, mapped to `MEDIA_UNAVAILABLE` by the caller.
    async fn sign_upload(&self, key: &str, byte_size: u64, expires_at: Timestamp) -> Result<Grant>;

    /// Signs a URL a client may download from.
    ///
    /// # Errors
    ///
    /// Whatever the backend fails with, mapped to `MEDIA_UNAVAILABLE` by the caller.
    async fn sign_download(&self, key: &str, expires_at: Timestamp) -> Result<Grant>;

    /// Reports an object's size and its first `head_len` bytes.
    ///
    /// `Ok(None)` means no such object — the normal answer for a commit whose upload
    /// never finished, and not an error.
    ///
    /// # Errors
    ///
    /// Whatever the backend fails with, mapped to `MEDIA_UNAVAILABLE` by the caller.
    async fn head(&self, key: &str, head_len: usize) -> Result<Option<Head>>;

    /// Reports how many bytes of a partial upload storage holds.
    ///
    /// `Ok(None)` means nothing has arrived. Backends that cannot answer — a plain
    /// `PUT` target with no multipart support — should return `Ok(None)`, which makes
    /// resume report zero progress and a client start over. That is worse for the
    /// client and correct for the server: reporting a number the backend guessed would
    /// make a client skip bytes that are not there.
    ///
    /// # Errors
    ///
    /// Whatever the backend fails with, mapped to `MEDIA_UNAVAILABLE` by the caller.
    async fn uploaded_bytes(&self, key: &str) -> Result<Option<u64>>;

    /// Removes an object's bytes.
    ///
    /// Called by the sweeper after a row is tombstoned and by the scan pipeline after a
    /// rejection. Must succeed for a key that holds nothing, because both callers can
    /// legitimately run twice.
    ///
    /// # Errors
    ///
    /// Whatever the backend fails with, mapped to `MEDIA_UNAVAILABLE` by the caller.
    async fn remove(&self, key: &str) -> Result<()>;
}

/// A shared storage backend.
pub type SharedStorage = std::sync::Arc<dyn Storage>;

/// The media service, as the layer above sees it.
///
/// One erased trait for the whole domain, so `migo-gateway` and `migo-api` depend on a
/// `dyn` and not on the generic service and its four type parameters.
///
/// # Where upload tickets live
///
/// `begin` seals the authorisation into a MAC'd token ([`crate::ticket`]) and returns it
/// in [`Ticket`], but the wire's `MediaTicket` carries only the media id — the protocol
/// has no field for the token bytes. So the service files each token at `begin`, and
/// `status`/`commit`/`abort` resolve the wire's `upload_id` back to it. The token is
/// still what authorises: the map is keyed by the id the service itself minted, and the
/// claim's account, device, and expiry are verified on every use exactly as a presented
/// token's would be. The trade is lifetime: a filed ticket lives in the process, so a
/// restart forgets unfinished uploads — whose owners re-begin, which is what a ticket
/// lifetime of minutes already asks of them.
#[async_trait]
pub trait Library: Send + Sync {
    /// Issues an upload ticket.
    ///
    /// Checks the destination, the size against the ceiling for the kind, the duration
    /// for a voice note, and the caller's rate. Writes nothing: see [`crate::ticket`]
    /// for why an unfinished upload leaves no row.
    ///
    /// # Errors
    ///
    /// `PERMISSION_DENIED` if the caller is not a member of the destination conversation,
    /// `NOT_FOUND` if it does not exist, `UPLOAD_LIMIT_EXCEEDED` if the declared size is
    /// over the ceiling for its kind, `VALIDATION_FAILED` for a malformed MIME type or a
    /// voice note over the duration ceiling, `RATE_LIMITED`, or `MEDIA_UNAVAILABLE` if
    /// storage cannot sign.
    async fn begin(&self, caller: &Caller, request: UploadRequest) -> Result<Ticket>;

    /// Reports how far an unfinished upload got.
    ///
    /// Brief section 168: *"Kegagalan pada 80 persen dilanjutkan dari sekitar 80 persen,
    /// bukan dari nol"*.
    ///
    /// The upload is named by the id `begin` handed out; the service resolves it to the
    /// filed ticket and verifies the caller against that ticket's claim.
    ///
    /// # Errors
    ///
    /// `VALIDATION_FAILED` if no ticket is filed for the id, or the filed ticket is
    /// expired or belongs to another account or device; `RATE_LIMITED`; or
    /// `MEDIA_UNAVAILABLE`.
    async fn status(&self, caller: &Caller, upload_id: Id) -> Result<Progress>;

    /// Turns an uploaded object into a row.
    ///
    /// Verifies the size against what storage reports, identifies the content from its
    /// leading bytes where the kind requires it, and records the client's checksum. The
    /// row's scan status is `Clean` for an end-to-end destination and `Pending`
    /// otherwise — see [`crate::model::Policy::clearance_at_commit`].
    ///
    /// # Errors
    ///
    /// `VALIDATION_FAILED` for no filed ticket, an expired one, one belonging to another
    /// account or device, or a size that disagrees with storage,
    /// `UNSUPPORTED_MEDIA_TYPE` if the leading bytes are not what the kind must hold,
    /// `RATE_LIMITED`, `MEDIA_UNAVAILABLE`, or `STORAGE_UNAVAILABLE`.
    async fn commit(&self, caller: &Caller, upload_id: Id, commit: Commit) -> Result<Stored>;

    /// Abandons an upload and removes whatever arrived.
    ///
    /// Idempotent, and safe to call for a ticket whose upload never started. A client
    /// that cannot reach this endpoint is not a problem: an abandoned upload has no row,
    /// and its bytes are removed by the same lifecycle rule that cleans up after a
    /// client that crashed.
    ///
    /// # Errors
    ///
    /// `VALIDATION_FAILED` for no filed ticket, an expired one, or one belonging to
    /// another account or device; `RATE_LIMITED`; or `MEDIA_UNAVAILABLE`.
    async fn abort(&self, caller: &Caller, upload_id: Id) -> Result<()>;

    /// Issues a short-lived download URL, after checking authorization.
    ///
    /// # The check, in order
    ///
    /// 1. The object exists and is not tombstoned, or `NOT_FOUND`.
    /// 2. The caller owns it — served whatever its scan status, because withholding
    ///    somebody's own upload from them protects nobody.
    /// 3. Otherwise the object must have a destination and the caller must be a member
    ///    of it, or `NOT_FOUND`. Brief section 48: *"untuk objek yang seharusnya tidak
    ///    diketahui keberadaannya dijawab NOT_FOUND agar tidak membocorkan eksistensi"*.
    ///    An object with no destination is profile media, readable by any authenticated
    ///    account.
    /// 4. The object must be `Clean`, or `MEDIA_UNAVAILABLE`. Brief section 168:
    ///    *"Media pending TIDAK BOLEH disajikan ke pengguna lain"*.
    ///
    /// # Errors
    ///
    /// `NOT_FOUND` for an object that does not exist or that the caller may not have,
    /// `MEDIA_UNAVAILABLE` for one that is not cleared or that storage cannot sign,
    /// `RATE_LIMITED`, or `STORAGE_UNAVAILABLE`.
    async fn fetch_url(&self, caller: &Caller, media_id: Id) -> Result<Grant>;

    /// Reads an object's metadata, subject to the same authorization as [`Self::fetch_url`].
    ///
    /// For a client that has a `media_id` from a message and wants to render a
    /// placeholder — dimensions, duration, size — without asking for a URL it is not
    /// going to use yet.
    ///
    /// # Errors
    ///
    /// As [`Self::fetch_url`], except that an object which is not cleared is reported
    /// rather than refused: its metadata says `Scan::Pending`, which is what a client
    /// needs in order to show "scanning" instead of a broken image. Metadata is not the
    /// bytes, and section 168's rule is about serving the object.
    async fn describe(&self, caller: &Caller, media_id: Id) -> Result<Stored>;

    /// Tombstones an object at its owner's request.
    ///
    /// The row stays and the bytes are removed by the sweeper, which is what
    /// `MediaStore::delete_media` documents. Only the owner may do this: a conversation
    /// member deleting somebody else's upload is a moderation action, and moderation
    /// lives in its own crate with its own audit trail.
    ///
    /// # Errors
    ///
    /// `NOT_FOUND` if the object does not exist or the caller does not own it — not
    /// `PERMISSION_DENIED`, because "this exists and is not yours" is more than a
    /// stranger needs to know. `RATE_LIMITED` or `STORAGE_UNAVAILABLE`.
    async fn delete(&self, caller: &Caller, media_id: Id) -> Result<()>;

    /// Records a scanner's verdict.
    ///
    /// Called by the scan pipeline, not by a user. A rejection removes the bytes and
    /// leaves the row, so the same checksum is refused next time without being
    /// rescanned.
    ///
    /// # Errors
    ///
    /// `NOT_FOUND` for an object that does not exist, `STORAGE_UNAVAILABLE`, or
    /// `MEDIA_UNAVAILABLE` if the bytes of a rejected object cannot be removed.
    async fn record_scan(&self, media_id: Id, verdict: Verdict, at: Timestamp) -> Result<()>;
}

/// A shared media service.
pub type SharedLibrary = std::sync::Arc<dyn Library>;

/// Where an object may be stored, as a storage key.
///
/// # Why the key is not a secret and not an authorization input
///
/// The key names an object in a private bucket. Brief section 168 requires the bucket
/// to have no public or permanent URL, so knowing a key gets nobody anything without a
/// signature. That is why the key can be built from values a client supplied — the date
/// and the media id — without a check.
///
/// What the key must never become is the *place authorization lives*. An earlier draft
/// of this crate encoded the destination conversation into the key path and parsed it
/// back at download time, which works and which is a trap: a path is a string, string
/// parsing has edge cases, and a check that reads its input out of a filename is a check
/// a reviewer cannot see. The destination is a column now. The key is just a name.
#[must_use]
pub fn storage_key(
    kind: MediaKind,
    destination: Destination,
    media_id: Id,
    at: Timestamp,
) -> String {
    let scope = match destination {
        Destination::Conversation(_) => "c",
        Destination::Profile => "p",
    };
    // Date prefix so a bucket listing is browsable by an operator and so a lifecycle
    // rule can expire a day's worth of abandoned uploads with one prefix.
    let day = at.to_rfc3339();
    let day = day.get(..10).unwrap_or("0000-00-00");
    format!("{scope}/{}/{day}/{}", kind.label(), media_id.to_text())
}

/// The scan pipeline's view of what still needs looking at.
///
/// Not part of [`Library`]: the pipeline is a background job, not a request handler, and
/// giving it its own trait keeps a method that lists every unscanned object in the
/// deployment out of the interface the request path uses.
#[async_trait]
pub trait ScanQueue: Send + Sync {
    /// Records a verdict.
    ///
    /// # Errors
    ///
    /// As [`Library::record_scan`].
    async fn record(&self, media_id: Id, verdict: Verdict, at: Timestamp) -> Result<()>;

    /// Reads an object's current scan status.
    ///
    /// `Ok(None)` for an object that does not exist. The pipeline uses it to skip work
    /// it already did after a restart.
    ///
    /// # Errors
    ///
    /// `STORAGE_UNAVAILABLE`.
    async fn status_of(&self, media_id: Id) -> Result<Option<Scan>>;
}
