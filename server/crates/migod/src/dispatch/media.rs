//! SPEC opcodes for the MEDIA domain.
//!
//! Five opcodes translate the wire's `upload_id`/`object_id` framing onto the media
//! library, which is the one place that knows how to mint and verify upload tickets and
//! sign URLs. Bytes never touch this process (brief section 168); the handlers only carry
//! the caller's identity and the request, and hand both to [`migo_media::Library`].
//!
//! # The mapping from wire to domain
//!
//! The wire names uploads by an [`Id`] (`upload_id`/`object_id`), and the library
//! resolves that id to the sealed ticket it filed at `begin` — the token is the
//! capability (see `migo_core::id`: an `Id` is not a secret), and it never crosses the
//! wire: the protocol's `MediaTicket` carries only the id and the URL. So these
//! handlers pass the wire's `upload_id` through unchanged, and the library checks the
//! account-and-device binding the filed ticket's claim still carries.

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

/// `MEDIA_UPLOAD_BEGIN` (128) → a signed upload URL and the ticket that claims it later.
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
    let progress = svc.status(&call, request.upload_id).await?;
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
        // The wire carries only the digest; the size it agrees on is the one the ticket
        // was issued for, checked against storage's own count of the bytes.
        byte_size: None,
        checksum: Some(request.digest),
    };
    let stored = svc.commit(&call, request.upload_id, commit).await?;
    ctx.reply(&Acknowledged { ok: true })?;

    // The conversation the object landed in learns the object exists — everyone except
    // the uploader, who is holding the reply that says so. An avatar has no
    // conversation and therefore nobody to tell: the profile it belongs to is fetched,
    // not subscribed to. Coalesced per object: a re-commit racing a state change leaves
    // the newest state for that object, which is the only one that matters.
    if let Some(conversation) = stored.conversation_id {
        let topic = migo_protocol::Topic {
            kind: migo_protocol::TopicKind::Conversation,
            id: conversation,
        };
        ctx.publish_excluding_self(
            &topic,
            migo_protocol::Opcode::MediaStateEvent,
            &migo_protocol::MediaStateEvent {
                object_id: stored.media_id,
                state: "committed".to_string(),
            },
            Some(crate::dispatch::coalesce_key_of(&stored.media_id)),
        )?;
    }
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
    svc.abort(&call, request.upload_id).await?;
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
