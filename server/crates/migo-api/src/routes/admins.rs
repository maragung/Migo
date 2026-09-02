//! The global-admin management surface: the Owner/CEO's CRUD over who may
//! moderate every public room.
//!
//! Who the Owner/CEO is is named in configuration (`owner_account_id`), not
//! derived from data, so the write routes here reject everyone when the
//! deployment names no owner — the surface is closed rather than defaulted
//! open. The one route every signed-in account may call is `whoami`: the
//! client asks it before it fetches anything, so the management page can stay
//! hidden entirely for accounts that hold neither role.
//!
//! The handlers stay thin, like every route module here: map the JSON body to
//! the authenticator's own types, let the service charge, check the owner
//! designation, and audit, and map the result back.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{delete, get};
use axum::{Json, Router};
use serde::Deserialize;

use migo_auth::{AdminStanding, AdminView};
use migo_core::Id;

use crate::extract::Authenticated;
use crate::ApiState;

/// The admin routes: one read any signed-in account may make to learn its own
/// standing, and the owner-only list, grant, and revoke.
pub(crate) fn routes() -> Router<ApiState> {
    Router::new()
        .route("/admins/whoami", get(standing))
        .route("/admins", get(list).put(grant))
        .route("/admins/{account_id}", delete(revoke))
}

/// `GET /v1/admins/whoami` — what the caller may open. Never fails on
/// standing: an account that is neither owner nor admin gets `{"owner":
/// false, "admin": false}`, which is the answer, not an error.
async fn standing(
    State(state): State<ApiState>,
    auth: Authenticated,
) -> Result<Json<AdminStanding>, crate::ApiError> {
    let now = state.now();
    let context = auth.facts.context(now);
    let standing = state
        .authenticator()
        .admin_standing(&auth.identity, &context)
        .await?;
    Ok(Json(standing))
}

#[derive(serde::Serialize)]
struct AdminsResponse {
    admins: Vec<AdminView>,
}

/// `GET /v1/admins` — every global admin, with usernames resolved for the
/// owner's list. Owner-only.
async fn list(
    State(state): State<ApiState>,
    auth: Authenticated,
) -> Result<Json<AdminsResponse>, crate::ApiError> {
    let now = state.now();
    let context = auth.facts.context(now);
    let admins = state
        .authenticator()
        .global_admins(&auth.identity, &context)
        .await?;
    Ok(Json(AdminsResponse { admins }))
}

/// `PUT /v1/admins` — appoint a global admin by username. Idempotent: a
/// repeated appointment keeps the original grant. Owner-only.
#[derive(Deserialize)]
struct GrantAdminBody {
    /// The account's username, with or without the leading `@`.
    username: String,
}

async fn grant(
    State(state): State<ApiState>,
    auth: Authenticated,
    Json(body): Json<GrantAdminBody>,
) -> Result<Json<AdminView>, crate::ApiError> {
    let username = body.username.trim().to_string();
    if username.is_empty() {
        return Err(crate::ApiError::from(migo_protocol::fault::validation(
            "username",
            "a grant names the account by its username",
        )));
    }
    let now = state.now();
    let context = auth.facts.context(now);
    let view = state
        .authenticator()
        .grant_global_admin(&auth.identity, &username, &context)
        .await?;
    Ok(Json(view))
}

/// `DELETE /v1/admins/{account_id}` — revoke a global admin. Revoking an
/// account that is not one is a quiet 204, the same shape rule the wallet
/// archive follows. Owner-only.
async fn revoke(
    State(state): State<ApiState>,
    auth: Authenticated,
    Path(account_id): Path<Id>,
) -> Result<StatusCode, crate::ApiError> {
    let now = state.now();
    let context = auth.facts.context(now);
    state
        .authenticator()
        .revoke_global_admin(&auth.identity, account_id, &context)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
