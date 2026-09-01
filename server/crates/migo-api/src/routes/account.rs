//! The account surface beyond the password: the ML-DSA identity ceremonies,
//! the device list, and the wallet registry.
//!
//! The identity routes are a second front door to a session (brief section
//! 182): a client that holds a root secret asks for a challenge, signs the
//! canonical bytes it is given, and answers with the signatures. The
//! handlers here stay as thin as the password ones — map the JSON body to
//! the authenticator's own types, let the service charge, verify, and audit,
//! and map the result back.
//!
//! The device and wallet routes are the authenticated read/write surface of
//! the account's own metadata. Nothing here moves a secret in either
//! direction: the device list carries a public key's *presence*, the wallet
//! registry carries an address, and the one write the device surface offers
//! takes a device away rather than handing anything out — a revoke ends the
//! device's sessions server-side (brief section 18), which is the whole of
//! what "this phone is gone" must mean.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use migo_auth::{
    AddDeviceAnswer, ChallengeAnswer, IdentityChallengeScope, IdentityPublication, RotationAnswer,
};
use migo_core::Id;

use crate::extract::{Authenticated, RequestFacts};
use crate::ratelimit::charge_ip;
use crate::routes::auth::{DeviceRequest, GrantResponse};
use crate::ApiState;

/// One unit charged against the caller's network bucket per anonymous
/// ceremony request, on top of the service's own charge.
const BOOTSTRAP_COST: u32 = 1;

/// The account routes: the identity ceremonies under `/auth/identity`, and
/// the device and wallet surfaces at the version root.
pub(crate) fn routes() -> Router<ApiState> {
    let identity = Router::new()
        .route("/challenge", post(issue_challenge))
        .route("/login", post(answer_login))
        .route("/add-device", post(answer_add_device))
        .route("/rotate/challenge", post(issue_rotation))
        .route("/rotate", post(rotate))
        .route("/key", post(publish_key));
    Router::new()
        .nest("/auth/identity", identity)
        .route("/devices", get(list_devices))
        .route("/devices/{device_id}/revoke", post(revoke_device))
        .route("/wallets", get(list_wallets).put(register_wallet))
        .route("/wallets/{wallet_id}", post(archive_wallet))
}

// --- challenges ---------------------------------------------------------------

/// `POST /v1/auth/identity/challenge` — ask for a ceremony payload.
///
/// One body shape covers both anonymous ceremonies: `{"purpose": "login",
/// "identifier": ..., "device_id": ...}` names a registered device to sign
/// in on, and `{"purpose": "add-device", "account_id": ..., "device": {...}}`
/// restores the account onto a new device from a `.migo` container.
#[derive(Deserialize)]
struct ChallengeBody {
    /// `"login"` or `"add-device"`.
    purpose: String,
    #[serde(default)]
    identifier: Option<String>,
    #[serde(default)]
    device_id: Option<Id>,
    #[serde(default)]
    account_id: Option<Id>,
    #[serde(default)]
    device: Option<DeviceRequest>,
}

/// The public view of an issued challenge. The payload is the canonical MSE
/// encoding the client signs byte for byte — a client never re-encodes it,
/// which is what keeps three ports from disagreeing about what was signed.
#[derive(Serialize)]
struct ChallengeViewBody {
    /// The bytes to sign, base64.
    payload: String,
    challenge_id: Id,
    device_id: Id,
    expires_at_ms: i64,
}

impl ChallengeViewBody {
    fn from(view: &migo_auth::ChallengeView) -> Self {
        use base64::Engine;
        Self {
            payload: base64::engine::general_purpose::STANDARD.encode(&view.payload),
            challenge_id: view.challenge_id,
            device_id: view.device_id,
            expires_at_ms: view.expires_at.as_unix_ms(),
        }
    }
}

async fn issue_challenge(
    State(state): State<ApiState>,
    facts: RequestFacts,
    Json(body): Json<ChallengeBody>,
) -> Result<Json<ChallengeViewBody>, crate::ApiError> {
    charge_ip(&state, facts.ip, BOOTSTRAP_COST).await?;
    let now = state.now();
    let request = match body.purpose.as_str() {
        "login" => {
            let identifier = body.identifier.filter(|value| !value.trim().is_empty());
            let Some(identifier) = identifier else {
                return Err(crate::ApiError::from(migo_protocol::fault::validation(
                    "identifier",
                    "a login challenge needs the account identifier",
                )));
            };
            let Some(device_id) = body.device_id else {
                return Err(crate::ApiError::from(migo_protocol::fault::validation(
                    "device_id",
                    "a login challenge names the device it is bound to",
                )));
            };
            migo_auth::IdentityChallengeRequest {
                scope: IdentityChallengeScope::Login {
                    identifier,
                    device_id,
                },
                device: migo_auth::DeviceClaim::new(migo_protocol::Platform::Unknown, "unused"),
            }
        }
        "add-device" => {
            let Some(account_id) = body.account_id else {
                return Err(crate::ApiError::from(migo_protocol::fault::validation(
                    "account_id",
                    "an add-device challenge carries the account id from the .migo container",
                )));
            };
            let Some(device) = body.device else {
                return Err(crate::ApiError::from(migo_protocol::fault::validation(
                    "device",
                    "an add-device challenge describes the new device",
                )));
            };
            migo_auth::IdentityChallengeRequest {
                scope: IdentityChallengeScope::AddDevice { account_id },
                device: device.into_claim()?,
            }
        }
        other => {
            return Err(crate::ApiError::from(migo_protocol::fault::validation(
                "purpose",
                &format!(
                    "unknown ceremony purpose {other:?}: expected \"login\" or \"add-device\""
                ),
            )));
        }
    };
    let context = facts.context(now);
    let view = state
        .authenticator()
        .issue_identity_challenge(request, &context)
        .await?;
    Ok(Json(ChallengeViewBody::from(&view)))
}

/// `POST /v1/auth/identity/login` — answer a login challenge with both
/// signatures and receive a session.
#[derive(Deserialize)]
struct IdentityLoginBody {
    challenge_id: Id,
    /// Signature by the account identity key, base64.
    identity_signature: String,
    /// Signature by the device credential, base64.
    device_signature: String,
}

async fn answer_login(
    State(state): State<ApiState>,
    facts: RequestFacts,
    Json(body): Json<IdentityLoginBody>,
) -> Result<Json<GrantResponse>, crate::ApiError> {
    charge_ip(&state, facts.ip, BOOTSTRAP_COST).await?;
    let now = state.now();
    let answer = ChallengeAnswer {
        challenge_id: body.challenge_id,
        identity_signature: decode_signature(&body.identity_signature)?,
        device_signature: decode_signature(&body.device_signature)?,
    };
    let context = facts.context(now);
    let grant = state
        .authenticator()
        .answer_identity_challenge(answer, &context)
        .await?;
    Ok(Json(grant.into()))
}

/// `POST /v1/auth/identity/add-device` — answer an add-device challenge:
/// introduce the new device's credential and receive the restored session.
#[derive(Deserialize)]
struct AddDeviceBody {
    challenge_id: Id,
    /// Signature by the account identity key, base64.
    identity_signature: String,
    /// The new device's credential public key, base64.
    device_public_key: String,
    /// Signature by that credential, base64.
    device_signature: String,
}

async fn answer_add_device(
    State(state): State<ApiState>,
    facts: RequestFacts,
    Json(body): Json<AddDeviceBody>,
) -> Result<Json<GrantResponse>, crate::ApiError> {
    charge_ip(&state, facts.ip, BOOTSTRAP_COST).await?;
    let now = state.now();
    let answer = AddDeviceAnswer {
        challenge_id: body.challenge_id,
        identity_signature: decode_signature(&body.identity_signature)?,
        device_public_key: decode_signature(&body.device_public_key)?,
        device_signature: decode_signature(&body.device_signature)?,
    };
    let context = facts.context(now);
    let grant = state
        .authenticator()
        .answer_add_device(answer, &context)
        .await?;
    Ok(Json(grant.into()))
}

// --- rotation and the legacy upgrade ------------------------------------------

/// `POST /v1/auth/identity/rotate/challenge` — ask (as the caller's own
/// authenticated device) for a rotation challenge.
async fn issue_rotation(
    State(state): State<ApiState>,
    auth: Authenticated,
) -> Result<Json<ChallengeViewBody>, crate::ApiError> {
    let now = state.now();
    let context = auth.facts.context(now);
    let view = state
        .authenticator()
        .issue_rotation_challenge(&auth.identity, &context)
        .await?;
    Ok(Json(ChallengeViewBody::from(&view)))
}

/// `POST /v1/auth/identity/rotate` — answer a rotation challenge with the
/// current key's signature and the successor's public key.
#[derive(Deserialize)]
struct RotateBody {
    challenge_id: Id,
    /// Signature by the *current* identity key under the rotate context.
    signature: String,
    /// The successor's public key, base64.
    new_public_key: String,
}

async fn rotate(
    State(state): State<ApiState>,
    auth: Authenticated,
    Json(body): Json<RotateBody>,
) -> Result<StatusCode, crate::ApiError> {
    let now = state.now();
    let answer = RotationAnswer {
        challenge_id: body.challenge_id,
        signature: decode_signature(&body.signature)?,
        new_public_key: decode_signature(&body.new_public_key)?,
    };
    let context = auth.facts.context(now);
    state
        .authenticator()
        .rotate_identity(&auth.identity, answer, &context)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /v1/auth/identity/key` — publish the caller's identity (and
/// optionally device) public keys on a password-era account. The legacy
/// upgrade door, idempotent by design.
#[derive(Deserialize)]
struct PublishKeyBody {
    /// The account identity's ML-DSA-65 public key, base64.
    identity_public_key: String,
    /// The caller's device credential public key, base64, when there is one
    /// to register.
    #[serde(default)]
    device_public_key: Option<String>,
}

async fn publish_key(
    State(state): State<ApiState>,
    auth: Authenticated,
    Json(body): Json<PublishKeyBody>,
) -> Result<StatusCode, crate::ApiError> {
    let now = state.now();
    let publication = IdentityPublication {
        identity_public_key: decode_signature(&body.identity_public_key)?,
        device_public_key: body
            .device_public_key
            .as_deref()
            .map(decode_signature)
            .transpose()?,
    };
    let context = auth.facts.context(now);
    state
        .authenticator()
        .publish_identity_key(&auth.identity, publication, &context)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

// --- devices and wallets --------------------------------------------------------

#[derive(Serialize)]
struct DevicesResponse {
    devices: Vec<migo_auth::DeviceSummary>,
}

/// `GET /v1/devices` — the caller's own devices, for their security screen.
async fn list_devices(
    State(state): State<ApiState>,
    auth: Authenticated,
) -> Result<Json<DevicesResponse>, crate::ApiError> {
    let now = state.now();
    let context = auth.facts.context(now);
    let devices = state
        .authenticator()
        .devices(&auth.identity, &context)
        .await?;
    Ok(Json(DevicesResponse { devices }))
}

#[derive(Serialize)]
struct RevokeDeviceResponse {
    ok: bool,
    /// How many sessions died with the device.
    revoked: u64,
}

/// `POST /v1/devices/{device_id}/revoke` — remove one of the caller's devices.
///
/// Brief section 18: the device row is marked revoked and every session on it ends,
/// so nothing on that device can authenticate, refresh, or open a WebSocket again. The
/// count comes back because "your phone, with its two sessions, is gone" is a fact
/// worth confirming rather than assuming.
async fn revoke_device(
    State(state): State<ApiState>,
    auth: Authenticated,
    Path(device_id): Path<Id>,
) -> Result<Json<RevokeDeviceResponse>, crate::ApiError> {
    let now = state.now();
    let context = auth.facts.context(now);
    let revoked = state
        .authenticator()
        .revoke_device(&auth.identity, device_id, &context)
        .await?;
    Ok(Json(RevokeDeviceResponse { ok: true, revoked }))
}

#[derive(Serialize)]
struct WalletsResponse {
    wallets: Vec<migo_auth::WalletSummary>,
}

/// `GET /v1/wallets` — the caller's registered wallet addresses.
async fn list_wallets(
    State(state): State<ApiState>,
    auth: Authenticated,
) -> Result<Json<WalletsResponse>, crate::ApiError> {
    let now = state.now();
    let context = auth.facts.context(now);
    let wallets = state
        .authenticator()
        .wallets(&auth.identity, &context)
        .await?;
    Ok(Json(WalletsResponse { wallets }))
}

/// `PUT /v1/wallets` — register (or idempotently re-register) a wallet
/// address on the caller's account.
#[derive(Deserialize)]
struct WalletBody {
    /// The EVM address; `0x`-prefixed and checksummed forms are accepted.
    address: String,
    #[serde(default)]
    chain_type: String,
    #[serde(default)]
    label: Option<String>,
    derivation_index: i32,
}

async fn register_wallet(
    State(state): State<ApiState>,
    auth: Authenticated,
    Json(body): Json<WalletBody>,
) -> Result<Json<migo_auth::WalletSummary>, crate::ApiError> {
    let now = state.now();
    let registration = migo_auth::WalletRegistration {
        address: body.address,
        chain_type: body.chain_type,
        label: body.label,
        derivation_index: body.derivation_index,
    };
    let context = auth.facts.context(now);
    let summary = state
        .authenticator()
        .register_wallet(&auth.identity, registration, &context)
        .await?;
    Ok(Json(summary))
}

/// `POST /v1/wallets/{wallet_id}` — archive one of the caller's wallets.
async fn archive_wallet(
    State(state): State<ApiState>,
    auth: Authenticated,
    Path(wallet_id): Path<Id>,
) -> Result<StatusCode, crate::ApiError> {
    let now = state.now();
    let context = auth.facts.context(now);
    state
        .authenticator()
        .archive_wallet(&auth.identity, wallet_id, &context)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

// --- helpers ---------------------------------------------------------------------

/// Decodes a base64 signature or key from a request body.
fn decode_signature(value: &str) -> Result<Vec<u8>, crate::ApiError> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(value)
        .map_err(|_| {
            crate::ApiError::from(migo_protocol::fault::validation(
                "signature",
                "must be base64-encoded",
            ))
        })
}
