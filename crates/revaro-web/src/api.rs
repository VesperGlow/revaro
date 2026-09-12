//! The slice of the HTTP client the shell needs.
//!
//! The Vue app funnelled every request through `web/src/api.ts`, which merged a
//! caller `AbortSignal` with a 60 s timeout, defaulted `Content-Type` to JSON and
//! unwrapped the `{error:{status,code,message}}` envelope into an `ApiError`.
//! This module is the same contract for the authenticated shell: session and
//! login, logout, folder metadata/children and the read-only trash listing.
//!
//! The request/response bodies are the shared types from
//! [`revaro_core::api::auth`], so a field rename on the server breaks this build
//! rather than failing at runtime. The error envelope is likewise the shared
//! [`ErrorEnvelope`], which is how the login form can branch on the
//! `totp_required` code without string-matching a message.
//!
//! Only compiled for wasm; the pure parts of the client live in `crate::logic`.

use gloo_net::http::Request;
use revaro_core::api::auth::{LoginRequest, Session};
use revaro_core::api::files::{Children, FileDetail, Trash};
use revaro_core::{ErrorCode, ErrorEnvelope};

/// A failed request to an authenticated JSON endpoint.
///
/// Keeping the status and optional shared error code alongside the message
/// lets the view distinguish an expired session from a server-side failure
/// without matching translated text. The browser only receives the server's
/// envelope; transport and malformed-response failures are represented with
/// status `0`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestError {
    /// HTTP status, or `0` when no usable HTTP response arrived.
    pub status: u16,
    /// Machine-readable code from the shared error envelope, when present.
    pub code: Option<ErrorCode>,
    /// Human-readable failure detail.
    pub message: String,
}

impl RequestError {
    /// True when the browser should return to the login screen.
    #[must_use]
    pub fn is_unauthorized(&self) -> bool {
        self.status == 401
    }
}

/// A rejected login, decoded from the shared error envelope when possible.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginError {
    /// HTTP status the server answered with.
    pub status: u16,
    /// Machine-readable code, when the server sent one.
    pub code: Option<ErrorCode>,
    /// Human-readable message: the envelope's, or a generic fallback.
    pub message: String,
}

impl LoginError {
    /// True when the server is asking for a TOTP or recovery code.
    ///
    /// `useAuthSession.submitLogin` treated this code as "show the second field
    /// and keep going", not as a final failure, so the distinction has to
    /// survive deserialization.
    #[must_use]
    pub fn requires_second_factor(&self) -> bool {
        self.code.as_ref() == Some(&ErrorCode::TOTP_REQUIRED)
    }

    /// True when a supplied second factor was rejected.
    #[must_use]
    pub fn second_factor_rejected(&self) -> bool {
        self.code.as_ref() == Some(&ErrorCode::INVALID_SECOND_FACTOR)
    }
}

/// Fetch the current session, or `None` when the visitor is not signed in.
///
/// A 401 is the ordinary "logged out" answer, not an error worth surfacing:
/// `checkSession` in the Vue app swallowed every failure and showed the login
/// page, and the shell keeps that behaviour.
pub async fn fetch_session() -> Option<Session> {
    let response = Request::get("/api/auth/me").send().await.ok()?;
    if !response.ok() {
        return None;
    }
    response.json::<Session>().await.ok()
}

/// Fetch a directory's metadata and breadcrumb trail.
pub async fn fetch_file(id: &str) -> Result<FileDetail, RequestError> {
    get_json(&format!("/api/files/{id}")).await
}

/// Fetch the live children and aggregate counters of a directory.
pub async fn fetch_children(id: &str) -> Result<Children, RequestError> {
    get_json(&format!("/api/files/{id}/children")).await
}

/// Fetch the top-level entries in the trash.
pub async fn fetch_trash() -> Result<Trash, RequestError> {
    get_json("/api/trash").await
}

/// Sign in, mapping a non-2xx answer to a decoded [`LoginError`].
pub async fn login(request: &LoginRequest) -> Result<Session, LoginError> {
    let sent = Request::post("/api/auth/login")
        .json(request)
        .map_err(|error| transport(error.to_string()))?
        .send()
        .await
        .map_err(|error| transport(error.to_string()))?;
    let status = sent.status();
    if sent.ok() {
        return sent
            .json::<Session>()
            .await
            .map_err(|error| transport(error.to_string()));
    }
    // The envelope's message is the user-facing text for everything except the
    // two codes the form handles itself.
    match sent.json::<ErrorEnvelope>().await {
        Ok(envelope) => Err(LoginError {
            status,
            code: envelope.error.code,
            message: envelope.error.message,
        }),
        Err(_) => Err(LoginError {
            status,
            code: None,
            message: format!("请求失败 ({status})"),
        }),
    }
}

/// End the session. Failures are ignored: the shell clears local state anyway.
pub async fn logout() {
    let _ = Request::post("/api/auth/logout").send().await;
}

/// A transport failure (offline, DNS, malformed body) carries no HTTP status.
fn transport(message: String) -> LoginError {
    LoginError {
        status: 0,
        code: None,
        message,
    }
}

/// Fetch and decode a JSON endpoint using the shared error envelope.
async fn get_json<T>(path: &str) -> Result<T, RequestError>
where
    T: serde::de::DeserializeOwned,
{
    let response = Request::get(path)
        .send()
        .await
        .map_err(|error| request_transport(error.to_string()))?;
    if response.ok() {
        return response
            .json::<T>()
            .await
            .map_err(|error| request_transport(error.to_string()));
    }
    let status = response.status();
    match response.json::<ErrorEnvelope>().await {
        Ok(envelope) => Err(RequestError {
            status,
            code: envelope.error.code,
            message: envelope.error.message,
        }),
        Err(_) => Err(RequestError {
            status,
            code: None,
            message: format!("请求失败 ({status})"),
        }),
    }
}

fn request_transport(message: String) -> RequestError {
    RequestError {
        status: 0,
        code: None,
        message,
    }
}
