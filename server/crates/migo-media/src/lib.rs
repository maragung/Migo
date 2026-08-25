//! Media uploads, signed URLs, and the authorization checked before one is issued.
//!
//! # The shape of the thing
//!
//! Bytes never touch this process. Brief section 168: *"Chat server TIDAK BOLEH menjadi
//! proxy byte media"*. A client asks for permission to upload, gets a short-lived signed
//! URL and an authenticated ticket, pushes the bytes straight to object storage, and
//! comes back to commit. To download, it asks for a URL, the server checks membership,
//! and the bytes come straight from storage. This crate mints, verifies, and authorises;
//! it never carries.
//!
//! ```text
//! begin   →  ticket + signed upload URL      (nothing written)
//! ...        client PUTs chunks to storage
//! status  →  how many bytes storage holds    (resume)
//! commit  →  size verified, content sniffed, row created
//! fetch   →  membership checked, signed download URL
//! ```
//!
//! # The column that makes download authorization possible
//!
//! Brief section 168 says a download is authorised by asking *"apakah pemohon adalah
//! anggota conversation atau room yang memuat media tersebut"*. Nothing in the original
//! schema could answer that. `media_object` had an owner and a storage key; the link
//! between an object and the conversation it belongs to lived only in the message that
//! referenced it — and for end-to-end media that reference is *inside the ciphertext*, so
//! the server was never going to see it.
//!
//! So `media_object` has a nullable `conversation_id`, recorded at [`Library::begin`],
//! when the client says where the upload is going and the server checks that it may. That
//! is the only moment the server is ever told. `NULL` means profile media — an avatar —
//! which every authenticated account may render.
//!
//! The three alternatives were considered and rejected, and it is worth saying why, since
//! two of them look easier:
//!
//! - *Trust a conversation the client names at download time and check membership in
//!   that.* Any member of any conversation could then fetch any object by naming their
//!   own conversation. The check would run, pass, and mean nothing.
//! - *Encode the scope in the storage key and parse it back.* Sound — the server mints
//!   the key — and a trap. Authorization state smuggled through a string is
//!   authorization a reviewer cannot see, and string parsing has edge cases that
//!   membership checks do not.
//! - *Rely on the id being unguessable.* Ids are unguessable and this would work in
//!   practice, which is why it is tempting. It also directly contradicts a brief
//!   requirement, and "the identifier is secret" is a property that survives exactly
//!   until the first client logs one.
//!
//! One column, one index, one membership call that works for direct conversations,
//! groups, and rooms alike — the last because joining a room inserts a
//! `conversation_member` row, so `is_member` already answers for all three.
//!
//! # Layering
//!
//! Layer 3. Depends on `migo-core`, `migo-crypto`, `migo-protocol`, `migo-ratelimit`, and
//! `migo-store`, and on no other layer-3 crate. In particular it does not depend on
//! `migo-messaging`, even though it asks a messaging question: it asks the *store*, which
//! is where the membership rows are. Two domain crates depending on each other is how a
//! dependency graph becomes a cycle, and the cost of avoiding it here is one trait import.
//!
//! # What lives elsewhere
//!
//! Scanning is a separate process — see [`ScanQueue`]. Deleting somebody else's upload is
//! moderation, and lives in `migo-moderation` with an audit entry behind it. Signing is a
//! [`Storage`] implementation in the composition root, because only the backend knows its
//! own scheme and because the S3 credentials belong where every other credential lives.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod metrics;

pub mod model;
pub mod service;
pub mod sniff;
pub mod ticket;
pub mod traits;

pub use crate::model::{
    Caller, Commit, Destination, Grant, MediaKind, Policy, Progress, Scan, Stored, Ticket,
    UploadRequest, Verdict, CHUNK_BYTES, HARD_MAX_BYTES, MAX_CHECKSUM_LEN, MAX_MIME_LEN,
    SNIFF_BYTES, TICKET_TTL_MS, VOICE_NOTE_MAX_MS,
};
pub use crate::service::{open, Media};
pub use crate::sniff::{identify, sniff, Identity, Refusal};
pub use crate::ticket::{Claim, Rejection};
pub use crate::traits::{
    storage_key, Head, Library, ScanQueue, SharedLibrary, SharedStorage, Storage,
};
