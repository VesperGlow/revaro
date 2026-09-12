//! The authenticated-request extractor.
//!
//! ## Why an extractor instead of middleware
//!
//! The Go server wrapped the authenticated subtree in a `requireAuth`
//! middleware that pushed the username into the request context. Axum can do
//! that too, but an extractor composes better here:
//!
//! * a handler declares the dependency in its signature (`user: AuthUser`), so
//!   forgetting it is a compile error rather than a runtime lookup that panics;
//! * the rejection is an [`ApiError`], so the `401 authentication required`
//!   body is produced by the same error model as every other handler;
//! * later modules (files, uploads) can adopt it without a router-wide layer
//!   ordering decision, and public routes cannot accidentally sit under it.
//!
//! The cost is that each authenticated handler repeats `user: AuthUser` instead
//! of relying on an enclosing layer; that is a deliberate trade.

use std::sync::Arc;

use axum::extract::FromRequestParts;
use axum_extra::extract::cookie::CookieJar;
use http::request::Parts;
use revaro_core::ApiError;

use crate::state::AppState;

use super::SESSION_COOKIE;

/// The signed-in administrator, resolved from the session cookie.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthUser {
    /// The current administrator login name (read through the `settings` join,
    /// so a rename is visible immediately).
    pub username: String,
    /// The raw session token, needed by `logout` and by the TOTP operations
    /// that revoke every *other* session.
    pub token: String,
}

impl FromRequestParts<Arc<AppState>> for AuthUser {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &Arc<AppState>,
    ) -> Result<Self, Self::Rejection> {
        // `CookieJar` extraction is infallible for any state type; the `&()`
        // state is what the trait's generic parameter resolves to.
        let jar = match CookieJar::from_request_parts(parts, &()).await {
            Ok(jar) => jar,
            // `CookieJar` extraction is infallible; the arm documents that.
            Err(never) => match never {},
        };
        let Some(cookie) = jar.get(SESSION_COOKIE) else {
            return Err(ApiError::unauthorized("authentication required"));
        };
        let token = cookie.value().to_owned();
        match state.auth.authenticate(&token).await {
            Ok(username) => Ok(Self { username, token }),
            Err(_) => Err(ApiError::unauthorized("authentication required")),
        }
    }
}
