//! PROFILE_UPDATE and the social discovery opcodes: suggestions and search.
//!
//! Three opcodes, each a thin translation from a wire frame onto one service method:
//! [`ProfileUpdate`] onto the store's `update_profile`, [`SuggestReq`] onto
//! [`Graph::suggest`], and [`SearchReq`] onto [`Graph::search`]. The services own every
//! rule — the privacy gates, the block checks, the searchable opt-in — so these handlers
//! only decode, call, and reply.
//!
//! # What PROFILE_UPDATE is allowed to touch
//!
//! The caller's own profile and nothing else: `update_profile` takes the account id from
//! the caller, not from the frame, and the wire struct carries no account id at all. A
//! frame that tried to patch somebody else's profile would have nowhere to put the other
//! account's id — the shape makes the attack unwritable. The wire's optional fields map
//! onto the store's [`Patch`](migo_store::model::Patch) semantics: an absent field leaves
//! the column alone, and the server decides what "present but empty" means (an empty bio
//! clears it; an empty display name is refused by the service's own validation).
//!
//! # Discovery is a read of the social graph
//!
//! Suggestions and search are the social crate's to serve, not the store's: both have to
//! honour blocks and the searchable opt-in, and the graph is where those rules live.

use migo_core::Error;
use migo_gateway::ClientContext;
use migo_protocol::{
    fault, from_frame, Frame, ProfileUpdate, SearchReq, SearchResponse, SuggestReq, SuggestedUser,
    UserProfile,
};
use migo_social::Caller as SocialCaller;
use migo_social::SharedSocial;
use migo_store::model::Visibility;
use migo_store::model::{Patch, ProfilePatch};
use migo_store::SharedStore;

/// Builds the social caller every handler in this module needs.
fn caller(ctx: &ClientContext<'_>) -> SocialCaller {
    let identity = ctx.identity();
    SocialCaller::new(
        identity.account_id(),
        identity.device_id(),
        identity.tier,
        ctx.now(),
    )
}

/// `PROFILE_UPDATE` (111) → patches the caller's own profile and replies with the new card.
///
/// The patch semantics are the wire's: absent means leave alone, present means set. The
/// one subtlety is the mapping between the wire's flat optionals and the store's
/// three-valued `Patch`: the wire cannot express "clear this field" for the string
/// options (an absent field is "keep"), so clearing bio or avatar is a later wire
/// addition — an empty string sets an empty bio, which the service's own length
/// validation accepts or refuses on its own rules.
pub(crate) async fn handle_profile_update(
    ctx: &ClientContext<'_>,
    frame: &Frame,
    social: &SharedSocial,
    store: &SharedStore,
) -> Result<(), Error> {
    let who = caller(ctx);
    let request: ProfileUpdate = from_frame(frame).map_err(fault::from_wire)?;

    let patch = ProfilePatch {
        display_name: request.display_name,
        bio: request.bio.map(Patch::Set).unwrap_or(Patch::Keep),
        avatar_media_id: request
            .avatar_media_id
            .map(Patch::Set)
            .unwrap_or(Patch::Keep),
        birth_year: request
            .birth_year
            .and_then(|year| i16::try_from(year).ok())
            .map(Patch::Set)
            .unwrap_or(Patch::Keep),
        show_last_seen: request
            .show_last_seen
            .and_then(visibility_of)
            .map(Option::Some)
            .unwrap_or(None),
        who_can_message: request
            .who_can_message
            .and_then(visibility_of)
            .map(Option::Some)
            .unwrap_or(None),
        who_can_add: request
            .who_can_add
            .and_then(visibility_of)
            .map(Option::Some)
            .unwrap_or(None),
        searchable: request.searchable,
    };

    let profile = store.update_profile(who.account_id, patch, who.now).await?;
    let _ = social; // reserved: the graph may need to invalidate a cache on profile change
    let _ = profile;
    // The reply is the caller's own refreshed card, read back through the same path a
    // fetch would take so the client sees exactly what other users will see.
    let cards = social.profiles(&who, &[who.account_id]).await?;
    let card = cards
        .into_iter()
        .next()
        .ok_or_else(|| fault::internal("the profile that was just written cannot be read back"))?;
    ctx.reply(&wire_profile(card))
}

/// `SUGGESTIONS` (118) → friend suggestions from the graph.
pub(crate) async fn handle_suggestions(
    ctx: &ClientContext<'_>,
    frame: &Frame,
    svc: &SharedSocial,
) -> Result<(), Error> {
    let who = caller(ctx);
    let _request: SuggestReq = from_frame(frame).map_err(fault::from_wire)?;
    let suggestions = svc
        .suggest(&who, _request.limit.map(|limit| limit as u16))
        .await?;
    // The graph suggests ids and mutual counts; the names come from the profile read the
    // graph also owns, so a block silently removes a suggestion rather than leaking a
    // name the caller may not see.
    let ids: Vec<migo_core::Id> = suggestions.iter().map(|s| s.account_id).collect();
    let cards = svc.profiles(&who, &ids).await?;
    let by_id: std::collections::HashMap<migo_core::Id, migo_social::model::ProfileCard> = cards
        .into_iter()
        .map(|card| (card.account_id, card))
        .collect();
    let results = suggestions
        .into_iter()
        .filter_map(|s| {
            let card = by_id.get(&s.account_id)?;
            Some(SuggestedUser {
                account_id: s.account_id,
                username: card.username.clone(),
                display_name: card.display_name.clone(),
                mutual_friends: s.mutual_friends,
            })
        })
        .collect();
    ctx.reply(&SearchResponse { results })
}

/// `SEARCH` (119) → public profiles matching a prefix.
pub(crate) async fn handle_search(
    ctx: &ClientContext<'_>,
    frame: &Frame,
    svc: &SharedSocial,
) -> Result<(), Error> {
    let who = caller(ctx);
    let request: SearchReq = from_frame(frame).map_err(fault::from_wire)?;
    let found = svc
        .search(
            &who,
            &request.query,
            request.limit.map(|limit| limit as u16),
        )
        .await?;
    ctx.reply(&SearchResponse {
        results: found
            .into_iter()
            .map(|f| SuggestedUser {
                account_id: f.account_id,
                username: f.username,
                display_name: f.display_name,
                mutual_friends: 0,
            })
            .collect(),
    })
}

/// Maps a wire visibility number onto the graph's enum, refusing what this build does not know.
fn visibility_of(raw: u32) -> Option<Visibility> {
    i16::try_from(raw).ok().map(Visibility::from_i16)
}

/// Projects a profile card onto the wire shape, matching the dispatcher's own projection.
fn wire_profile(card: migo_social::model::ProfileCard) -> UserProfile {
    use migo_core::PublicId;
    UserProfile {
        user_id: card.account_id,
        public_id: card.account_id.public_id(PublicId::User),
        username: card.username,
        display_name: card.display_name,
        avatar_url: None,
        avatar_media_id: card.avatar_media_id,
        bio: card.bio,
        country: card.country,
        language: Some(card.locale),
        level: None,
        presence: None,
        badges: None,
        verified: None,
        custom_status: None,
    }
}
