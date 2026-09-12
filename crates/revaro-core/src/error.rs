//! The one error model shared by the backend, the task system and the web UI.
//!
//! Every failed HTTP response has the same JSON envelope:
//!
//! ```json
//! { "error": { "status": 404, "code": "file_not_found", "message": "file not found" } }
//! ```
//!
//! `code` is optional; the frontend HTTP client only needs `message`, but the
//! code lets the UI branch on specific conditions (for example prompting for a
//! second factor) without string matching.

use std::borrow::Cow;
use std::fmt;

use serde::{Deserialize, Serialize};

/// A stable, machine-readable error identifier.
///
/// This is a transparent newtype rather than a closed enum: a newer server may
/// introduce codes an older client does not know, and such a client must still
/// be able to deserialize the envelope. The associated constants are the shared
/// vocabulary both ends agree on today.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ErrorCode(Cow<'static, str>);

impl ErrorCode {
    /// The request needs a TOTP authenticator or recovery code.
    pub const TOTP_REQUIRED: ErrorCode = ErrorCode::from_static("totp_required");
    /// The supplied TOTP authenticator or recovery code was rejected.
    pub const INVALID_SECOND_FACTOR: ErrorCode = ErrorCode::from_static("invalid_second_factor");

    /// Build a code from a `'static` string without allocating.
    #[must_use]
    pub const fn from_static(value: &'static str) -> Self {
        Self(Cow::Borrowed(value))
    }

    /// Build a code from an owned or borrowed string.
    #[must_use]
    pub fn new(value: impl Into<Cow<'static, str>>) -> Self {
        Self(value.into())
    }

    /// The wire representation of the code.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&'static str> for ErrorCode {
    fn from(value: &'static str) -> Self {
        Self::from_static(value)
    }
}

/// The `error` object inside the response envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiErrorBody {
    /// HTTP status code, repeated in the body so clients can log it uniformly.
    pub status: u16,
    /// Optional machine-readable code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<ErrorCode>,
    /// Human-readable message, already localized for the end user by the caller.
    pub message: String,
}

/// The complete JSON body of every failed API response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorEnvelope {
    pub error: ApiErrorBody,
}

impl ErrorEnvelope {
    /// Wrap a plain message without a code.
    #[must_use]
    pub fn new(status: u16, message: impl Into<String>) -> Self {
        Self {
            error: ApiErrorBody {
                status,
                code: None,
                message: message.into(),
            },
        }
    }

    /// Wrap a coded error.
    #[must_use]
    pub fn coded(status: u16, code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            error: ApiErrorBody {
                status,
                code: Some(code),
                message: message.into(),
            },
        }
    }

    /// The HTTP status carried by this envelope.
    #[must_use]
    pub fn status(&self) -> u16 {
        self.error.status
    }

    /// The human-readable message carried by this envelope.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.error.message
    }
}

impl fmt::Display for ErrorEnvelope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.error.message)
    }
}

impl std::error::Error for ErrorEnvelope {}

/// An API failure carrying everything needed to build a response.
///
/// Handlers return [`Result`]; the HTTP layer turns any `ApiError` into the
/// [`ErrorEnvelope`] above. Errors raised deeper in the stack (storage, media,
/// database) are mapped onto this type at their boundary so the wire contract
/// stays in one place.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct ApiError {
    /// HTTP status to respond with.
    pub status: u16,
    /// Optional stable code.
    pub code: Option<ErrorCode>,
    /// Message shown to the user.
    pub message: String,
}

impl ApiError {
    /// Build an error with an explicit status and no code.
    #[must_use]
    pub fn new(status: u16, message: impl Into<String>) -> Self {
        Self {
            status,
            code: None,
            message: message.into(),
        }
    }

    /// Attach a stable code to this error.
    #[must_use]
    pub fn with_code(mut self, code: ErrorCode) -> Self {
        self.code = Some(code);
        self
    }

    /// Convert to the response envelope.
    #[must_use]
    pub fn envelope(&self) -> ErrorEnvelope {
        ErrorEnvelope {
            error: ApiErrorBody {
                status: self.status,
                code: self.code.clone(),
                message: self.message.clone(),
            },
        }
    }

    /// `400 Bad Request`
    #[must_use]
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(400, message)
    }

    /// `401 Unauthorized`
    #[must_use]
    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self::new(401, message)
    }

    /// `403 Forbidden`
    #[must_use]
    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::new(403, message)
    }

    /// `404 Not Found`
    #[must_use]
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(404, message)
    }

    /// `405 Method Not Allowed`
    #[must_use]
    pub fn method_not_allowed(message: impl Into<String>) -> Self {
        Self::new(405, message)
    }

    /// `409 Conflict`
    #[must_use]
    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(409, message)
    }

    /// `413 Payload Too Large`
    #[must_use]
    pub fn payload_too_large(message: impl Into<String>) -> Self {
        Self::new(413, message)
    }

    /// `415 Unsupported Media Type`
    #[must_use]
    pub fn unsupported_media_type(message: impl Into<String>) -> Self {
        Self::new(415, message)
    }

    /// `422 Unprocessable Entity`
    #[must_use]
    pub fn unprocessable(message: impl Into<String>) -> Self {
        Self::new(422, message)
    }

    /// `429 Too Many Requests`
    #[must_use]
    pub fn too_many_requests(message: impl Into<String>) -> Self {
        Self::new(429, message)
    }

    /// `500 Internal Server Error`
    #[must_use]
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(500, message)
    }

    /// `503 Service Unavailable`
    #[must_use]
    pub fn unavailable(message: impl Into<String>) -> Self {
        Self::new(503, message)
    }

    /// True when the failure is the server's fault, used to decide whether the
    /// details may be logged but hidden from the response body.
    #[must_use]
    pub fn is_server_error(&self) -> bool {
        self.status >= 500
    }
}

impl From<ApiError> for ErrorEnvelope {
    fn from(value: ApiError) -> Self {
        value.envelope()
    }
}

impl From<ErrorEnvelope> for ApiError {
    fn from(value: ErrorEnvelope) -> Self {
        Self {
            status: value.error.status,
            code: value.error.code,
            message: value.error.message,
        }
    }
}

/// HTTP transport glue for the shared error model.
///
/// Behind the `axum` feature so the browser bundle never links a server
/// framework, while the server gets `?`-friendly error propagation into Axum
/// handlers for free. Request logging lives in the server's middleware, not
/// here, so this crate stays free of a logging dependency.
#[cfg(feature = "axum")]
impl axum::response::IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        let status = axum::http::StatusCode::from_u16(self.status)
            .unwrap_or(axum::http::StatusCode::INTERNAL_SERVER_ERROR);
        (status, axum::Json(self.envelope())).into_response()
    }
}

/// Conventional result alias for anything that can fail with an [`ApiError`].
pub type Result<T, E = ApiError> = std::result::Result<T, E>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_matches_the_wire_contract_without_a_code() {
        let json = serde_json::to_string(&ErrorEnvelope::new(404, "file not found")).unwrap();
        assert_eq!(
            json,
            r#"{"error":{"status":404,"message":"file not found"}}"#
        );
    }

    #[test]
    fn envelope_matches_the_wire_contract_with_a_code() {
        let envelope = ErrorEnvelope::coded(401, ErrorCode::TOTP_REQUIRED, "enter your code");
        let json = serde_json::to_string(&envelope).unwrap();
        assert_eq!(
            json,
            r#"{"error":{"status":401,"code":"totp_required","message":"enter your code"}}"#
        );
    }

    #[test]
    fn unknown_codes_still_deserialize() {
        let raw = r#"{"error":{"status":400,"code":"something_new","message":"x"}}"#;
        let envelope: ErrorEnvelope = serde_json::from_str(raw).unwrap();
        assert_eq!(envelope.error.code.unwrap().as_str(), "something_new");
    }

    #[test]
    fn api_error_round_trips_through_its_envelope() {
        let error = ApiError::conflict("an item with that name already exists");
        let envelope = error.envelope();
        assert_eq!(envelope.status(), 409);
        assert_eq!(ApiError::from(envelope.clone()), error);
    }
}
