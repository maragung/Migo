//! The extractors that turn an HTTP request into the facts a domain call needs.
//!
//! Three of them, each pulling one thing out of the request head:
//!
//! - [`RequestFacts`] gathers the per-request context every domain call wants — the caller's
//!   network address, the user agent for the session list, the correlation id — and never fails.
//! - [`Authenticated`] is the section 119 authenticate step: it verifies the bearer token's
//!   signature and expiry with no I/O, then confirms with the authenticator that the session is
//!   still live, and yields the resulting [`Identity`]. A handler that names it in its signature
//!   cannot run for an unauthenticated caller.
//! - [`IdempotencyKey`] lifts the optional idempotency header, so a state-changing endpoint can
//!   make a retry safe (section 118).
//!
//! The client address is read the way a service behind a CDN and an edge proxy actually receives
//! it (section 121): the first hop of `X-Forwarded-For`, then `X-Real-IP`, and only then the
//! transport peer address if the composition root attached one. A missing address is not an
//! error — it means the network-scoped rate-limit buckets are skipped for this request rather
//! than merged into one shared bucket.

use std::net::{IpAddr, SocketAddr};

use axum::extract::{ConnectInfo, FromRef, FromRequestParts};
use axum::http::header;
use axum::http::request::Parts;

use migo_auth::{Identity, RequestContext};
use migo_core::Timestamp;
use migo_protocol::fault;

use crate::error::ApiError;
use crate::ApiState;

/// The per-request facts a domain call needs, none of which the domain computes for itself.
///
/// Building a [`RequestContext`] from these is deferred to [`context`](RequestFacts::context) so
/// the caller can stamp it with the same `now` it uses for everything else in the request.
#[derive(Clone, Debug, Default)]
pub struct RequestFacts {
    /// The caller's address, as best the edge could report it; `None` when unknown.
    pub ip: Option<IpAddr>,
    /// The client's user agent, shown in the user's own session list.
    pub user_agent: Option<String>,
    /// The correlation id, so an audit row can be joined against a trace.
    pub request_id: Option<String>,
}

impl RequestFacts {
    /// Reads the facts out of a request head.
    fn from_parts(parts: &Parts) -> Self {
        Self {
            ip: client_ip(parts),
            user_agent: header_value(parts, &header::USER_AGENT),
            request_id: header_named(parts, "x-request-id"),
        }
    }

    /// Builds the domain request context, stamped with the given `now`.
    #[must_use]
    pub fn context(&self, now: Timestamp) -> RequestContext {
        let mut context = RequestContext::at(now);
        if let Some(ip) = self.ip {
            context = context.from_ip(ip);
        }
        if let Some(user_agent) = &self.user_agent {
            context = context.with_user_agent(user_agent.clone());
        }
        if let Some(request_id) = &self.request_id {
            context = context.with_request_id(request_id.clone());
        }
        context
    }
}

impl<S: Send + Sync> FromRequestParts<S> for RequestFacts {
    type Rejection = std::convert::Infallible;

    // No `.await` here — the facts are read synchronously from the request head. Written as a
    // non-async fn returning a ready future rather than an `async fn`: a newer clippy flags an
    // `async fn` trait impl with no await, and this form is equivalent on every clippy version.
    fn from_request_parts(
        parts: &mut Parts,
        _state: &S,
    ) -> impl std::future::Future<Output = Result<Self, Self::Rejection>> + Send {
        std::future::ready(Ok(Self::from_parts(parts)))
    }
}

/// An authenticated caller, and the facts their request arrived with.
///
/// Naming this in a handler signature is what makes the endpoint require authentication — the
/// handler body never runs otherwise. The [`facts`](Authenticated::facts) are carried through so
/// the handler can build one request context for both the authenticate step and the domain call.
pub struct Authenticated {
    /// The verified, non-revoked identity behind the request.
    pub identity: Identity,
    /// The facts the request arrived with.
    pub facts: RequestFacts,
}

impl<S> FromRequestParts<S> for Authenticated
where
    ApiState: FromRef<S>,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let api = ApiState::from_ref(state);
        let token = bearer(parts)
            .ok_or_else(|| ApiError::from(fault::unauthenticated("missing bearer")))?;
        let now = api.now();
        // Cheap first: signature and expiry only, no I/O. This also yields the device the token
        // was minted for, which the revocation-checked lookup needs.
        let claims = api.authenticator().verify_access(&token, now)?;
        let facts = RequestFacts::from_parts(parts);
        let context = facts.context(now);
        let identity = api
            .authenticator()
            .authenticate(&token, claims.device_id, &context)
            .await?;
        Ok(Self { identity, facts })
    }
}

/// The optional idempotency key a state-changing request may carry (brief section 118).
///
/// `None` when the header is absent. A handler that honours it uses the key to make a retried
/// request fold onto the first request's effect rather than repeat it.
pub struct IdempotencyKey(pub Option<String>);

impl<S: Send + Sync> FromRequestParts<S> for IdempotencyKey {
    type Rejection = std::convert::Infallible;

    // No `.await` here — the header lifts synchronously. A non-async fn returning a ready future
    // rather than an `async fn`, for the reason given on `RequestFacts` above.
    fn from_request_parts(
        parts: &mut Parts,
        _state: &S,
    ) -> impl std::future::Future<Output = Result<Self, Self::Rejection>> + Send {
        std::future::ready(Ok(Self(header_named(parts, "idempotency-key"))))
    }
}

/// Reads a header by its typed name into an owned string, if present and valid UTF-8.
fn header_value(parts: &Parts, name: &header::HeaderName) -> Option<String> {
    parts.headers.get(name)?.to_str().ok().map(str::to_owned)
}

/// Reads a header by its literal name into an owned string, if present and valid UTF-8.
fn header_named(parts: &Parts, name: &str) -> Option<String> {
    parts.headers.get(name)?.to_str().ok().map(str::to_owned)
}

/// The bearer token from the `Authorization` header, if one is present and non-empty.
fn bearer(parts: &Parts) -> Option<String> {
    let value = parts.headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let token = value
        .strip_prefix("Bearer ")
        .or_else(|| value.strip_prefix("bearer "))?
        .trim();
    if token.is_empty() {
        None
    } else {
        Some(token.to_owned())
    }
}

/// The caller's address, read the way a proxied deployment presents it.
fn client_ip(parts: &Parts) -> Option<IpAddr> {
    if let Some(forwarded) = header_named(parts, "x-forwarded-for") {
        if let Some(first) = forwarded.split(',').next() {
            if let Ok(ip) = first.trim().parse::<IpAddr>() {
                return Some(ip);
            }
        }
    }
    if let Some(real) = header_named(parts, "x-real-ip") {
        if let Ok(ip) = real.trim().parse::<IpAddr>() {
            return Some(ip);
        }
    }
    parts
        .extensions
        .get::<ConnectInfo<SocketAddr>>()
        .map(|info| info.0.ip())
}
