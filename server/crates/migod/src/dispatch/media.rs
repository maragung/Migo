//! SPEC opcodes for the MEDIA domain.
//!
//! Five opcodes translate the wire's `upload_id`/`object_id` framing onto the media
//! library, which is the one place that knows how to mint and verify upload tickets and
//! sign URLs. Bytes never touch this process (brief section 168); the handlers only carry
//! the caller's identity and the request, and hand both to [`migo_media::Library`].
//!
//! # The mapping from wire to domain
//!
//! The wire names uploads by an [`Id`] (`upload_id`/`object_id`), but the library authorises
//! and resumes an upload by the *ticket token* it sealed at `begin` — a MAC over the media
//! id, the account, the device, and the size. The token is the capability; the id is not
//! (see `migo_core::id` — an `Id` is not a secret). So `begin` returns the id the wire wants
//! in `MediaTicket.upload_id`, and `status`/`commit`/`abort` present the wire's `upload_id`
//! straight back to the library as the token bytes it actually checks. The client, having
//! received the ticket, carries the same `upload_id` it was given; this handler is the
//! bridge between the wire's id-shaped field and the library's token-shaped argument.

use migo_core::Error;
use migo_gateway::ClientContext;
use migo_media::model::{Commit, Destination, MediaKind, UploadRequest};
use migo_media::{Caller as MediaCaller, SharedLibrary};
use migo_protocol::{
    fault, from_frame, Acknowledged, Frame, MediaAbort, MediaBegin, MediaCommit, MediaFetch,
    MediaProgress, MediaStatusReq, MediaTicket, MediaUrl,
};

/// Builds the caller every media handler needs: the authenticated account and device, the
/// trust tier, and the one sampled `now`.
#[must_use]
fn caller(ctx: &ClientContext<'_>) -> MediaCaller {
    let identity = ctx.identity();
    MediaCaller::new(
        identity.account_id(),
        identity.device_id(),
        identity.tier,
        ctx.now(),
    )
}

/// `MEDIA_UPLOAD_BEGIN` (113) → a signed upload URL and the ticket that claims it later.
pub(crate) async fn handle_upload_begin(
    ctx: &ClientContext<'_>,
    frame: &Frame,
    svc: &SharedLibrary,
) -> Result<(), Error> {
    let call = caller(ctx);
    let request: MediaBegin = from_frame(frame).map_err(fault::from_wire)?;
    let upload = UploadRequest {
        kind: MediaKind::of_i16(request.kind as i16),
        mime: request.content_type,
        byte_size: request.size,
        destination: match request.conversation_id {
            Some(conversation_id) => Destination::Conversation(conversation_id),
            None => Destination::Profile,
        },
        width: request.width,
        height: request.height,
        // The wire carries a `u64`; the domain caps duration at `u32`. A value past the
        // ceiling is rejected later by `Policy`, so dropping the high bits here only loses
        // a duration the policy would have refused anyway.
        duration_ms: request
            .duration_ms
            .and_then(|value| u32::try_from(value).ok()),
    };
    let ticket = svc.begin(&call, upload).await?;
    let response = MediaTicket {
        upload_id: ticket.media_id,
        upload_url: ticket.upload.expose().to_string(),
        // Headers the client must send with the PUT live on the signed URL's own request;
        // the ticket carries none beyond the URL itself.
        headers: Vec::new(),
    };
    ctx.reply(&response)?;
    Ok(())
}

/// `MEDIA_UPLOAD_STATUS` (129) → how many bytes storage already holds, for resume.
pub(crate) async fn handle_upload_status(
    ctx: &ClientContext<'_>,
    frame: &Frame,
    svc: &SharedLibrary,
) -> Result<(), Error> {
    let call = caller(ctx);
    let request: MediaStatusReq = from_frame(frame).map_err(fault::from_wire)?;
    let progress = svc.status(&call, request.upload_id.as_bytes()).await?;
    let response = MediaProgress {
        received: progress.uploaded_bytes,
        expected: progress.byte_size,
    };
    ctx.reply(&response)?;
    Ok(())
}

/// `MEDIA_UPLOAD_COMMIT` (130) → turns the uploaded bytes into a row, returns `Acknowledged`.
pub(crate) async fn handle_upload_commit(
    ctx: &ClientContext<'_>,
    frame: &Frame,
    svc: &SharedLibrary,
) -> Result<(), Error> {
    let call = caller(ctx);
    let request: MediaCommit = from_frame(frame).map_err(fault::from_wire)?;
    let commit = Commit {
        // The wire carries only the digest; the authoritative byte count is read from
        // storage at commit and checked there, so the request has none to send.
        byte_size: 0,
        checksum: Some(request.digest),
    };
    svc.commit(&call, request.upload_id.as_bytes(), commit)
        .await?;
    ctx.reply(&Acknowledged { ok: true })?;
    Ok(())
}

/// `MEDIA_UPLOAD_ABORT` (131) → discards the bytes, returns `Acknowledged`.
pub(crate) async fn handle_upload_abort(
    ctx: &ClientContext<'_>,
    frame: &Frame,
    svc: &SharedLibrary,
) -> Result<(), Error> {
    let call = caller(ctx);
    let request: MediaAbort = from_frame(frame).map_err(fault::from_wire)?;
    svc.abort(&call, request.upload_id.as_bytes()).await?;
    ctx.reply(&Acknowledged { ok: true })?;
    Ok(())
}

/// `MEDIA_FETCH_URL` (132) → a short-lived, membership-checked download URL.
pub(crate) async fn handle_fetch_url(
    ctx: &ClientContext<'_>,
    frame: &Frame,
    svc: &SharedLibrary,
) -> Result<(), Error> {
    let call = caller(ctx);
    let request: MediaFetch = from_frame(frame).map_err(fault::from_wire)?;
    let grant = svc.fetch_url(&call, request.object_id).await?;
    let response = MediaUrl {
        url: grant.expose().to_string(),
        expires_at: grant.expires_at,
    };
    ctx.reply(&response)?;
    Ok(())
}
