//! The media data plane: the byte routes the filesystem backend's URLs point at.
//!
//! Brief section 168 splits media into a control plane and a data plane. The control
//! plane — tickets, commits, authorization — rides the MWP opcodes in `migo-media`; the
//! data plane is HTTP, because "upload byte besar memang milik HTTP". For the S3 backend
//! the data plane is the object store itself and none of these routes exist; for the
//! development filesystem backend the storage's `public_base` points here, at
//! `{public_url}/media/{key}`, and this module is what answers.
//!
//! # What these routes are not
//!
//! They are not authorization. The URLs this backend mints are unsigned — there is no
//! secret to sign with and no store to honour a signature — so a `PUT` lands wherever
//! its key says and a `GET` answers whoever holds the link. That is the documented
//! posture of the development backend: the keys embed server-minted random ids and are
//! unguessable, the size and type a client claimed are enforced later at commit, and a
//! production deployment replaces the whole module with presigned S3 URLs rather than
//! widening it. The one rule this module enforces itself is the same one
//! [`FsStorage`](crate::ports::FsStorage) enforces: a key that tries to climb out of the
//! media root is refused rather than followed.

use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::put;
use axum::Router;
use migo_media::sniff;

use crate::ApiState;

/// Reading and writing raw object bytes, as the data plane needs them.
///
/// The media service's own [`Storage`](migo_media::Storage) port is deliberately richer —
/// signed grants, heads, removal — because that is what ticketing needs. The data plane
/// needs exactly two things: put the bytes where the key says, and get them back. Keeping
/// the port this narrow means the S3 deployment simply does not provide it, and nothing
/// else in the API surface can accidentally grow a dependency on byte access.
#[async_trait]
pub trait MediaFiles: Send + Sync {
    /// Writes `bytes` to `key`, creating parents as needed.
    ///
    /// # Errors
    ///
    /// `STORAGE_UNAVAILABLE` on any filesystem fault, including a key that would resolve
    /// outside the media root.
    async fn write(&self, key: &str, bytes: Bytes) -> migo_core::Result<()>;

    /// Reads the bytes at `key`.
    ///
    /// # Errors
    ///
    /// `STORAGE_UNAVAILABLE` on a fault; `NOT_FOUND` when the key holds nothing.
    async fn read(&self, key: &str) -> migo_core::Result<Bytes>;
}

/// The shared, erased handle the handlers use.
pub type SharedMediaFiles = Arc<dyn MediaFiles>;

/// A key is flat path segments: no empties, no dots, no climbing.
fn key_is_safe(key: &str) -> bool {
    !key.is_empty()
        && !key.starts_with('/')
        && key
            .split('/')
            .all(|segment| !segment.is_empty() && segment != "." && segment != "..")
}

/// The routes: one PUT and one GET under `/media/`, matching the `public_base` the
/// filesystem backend mints URLs against.
pub(crate) fn routes() -> Router<ApiState> {
    Router::new().route("/media/{*key}", put(upload).get(download))
}

/// `PUT /media/{key}` — the client side of an upload ticket.
///
/// The body is the object's bytes, whole; the filesystem backend has no chunking, and the
/// route enforces the deployment's `media.max_upload_bytes` so a stray upload cannot fill
/// the disk before commit ever sees it.
async fn upload(State(state): State<ApiState>, Path(key): Path<String>, body: Bytes) -> Response {
    // Absent data plane (the S3 posture): this process is not the host the URL names,
    // and the honest answer to a byte request is the one a wrong host gives.
    let Some(files) = state.media_files() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if !key_is_safe(&key) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let ceiling = usize::try_from(state.policy().media_max_upload_bytes).unwrap_or(usize::MAX);
    if body.len() > ceiling {
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            "upload exceeds the configured media ceiling",
        )
            .into_response();
    }
    if let Err(error) = files.write(&key, body).await {
        // The operator debugging a broken media path needs the reason; the caller of an
        // unsigned dev URL has no use for it, and the wire error carries no secret.
        tracing::warn!(%error, key, "media upload write failed");
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    StatusCode::NO_CONTENT.into_response()
}

/// `GET /media/{key}` — the client side of a fetch grant.
///
/// The content type comes from the bytes' own magic, not from any claim the uploader
/// made: a file served as `image/png` is a file whose bytes say PNG. Unidentified bytes
/// are served as `application/octet-stream`, which is what a browser treats as
/// "download, do not interpret", and the sniff's refusals (HTML, SVG) answer 404 — a
/// file the scanner refuses is a file this node does not serve at all.
async fn download(
    State(state): State<ApiState>,
    Path(key): Path<String>,
) -> Result<Response, StatusCode> {
    let Some(files) = state.media_files() else {
        return Err(StatusCode::NOT_FOUND);
    };
    if !key_is_safe(&key) {
        return Err(StatusCode::NOT_FOUND);
    }
    let bytes = files.read(&key).await.map_err(|error| {
        tracing::warn!(%error, key, "media download read failed");
        StatusCode::NOT_FOUND
    })?;
    let content_type = match sniff::sniff(&bytes, bytes.len().min(migo_media::SNIFF_BYTES)) {
        sniff::Verdict::Identified(identity) => identity.mime(),
        sniff::Verdict::Refused(_) => return Err(StatusCode::NOT_FOUND),
    };
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static(content_type),
    );
    // The keys are content-addressed by upload, so the bytes behind one never change:
    // immutable caching is safe and saves the client a round trip on every render.
    headers.insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("private, max-age=86400, immutable"),
    );
    Ok((StatusCode::OK, headers, bytes).into_response())
}
