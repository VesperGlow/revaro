//! Transport helpers for the shared error model.
//!
//! The error *model* lives in [`revaro_core::error`]; the `IntoResponse`
//! implementation ships with that crate behind its `axum` feature so the
//! browser bundle never links a server framework. This module adds only the
//! pieces that are specific to being a server: a JSON response with an explicit
//! status, and the fixed body the server returns for an unparseable request.

use axum::Json;
use axum::response::{IntoResponse, Response};
use http::StatusCode;
use revaro_core::error::ApiError;

/// Result alias for handlers: any `ApiError` becomes a response automatically.
pub type ApiResult<T> = Result<T, ApiError>;

/// A successful JSON response with an explicit status code.
///
/// Handlers return `impl IntoResponse`, so this is only needed when the status
/// is not `200`.
#[derive(Debug)]
pub struct JsonStatus<T>(pub StatusCode, pub T);

impl<T: serde::Serialize> IntoResponse for JsonStatus<T> {
    fn into_response(self) -> Response {
        (self.0, Json(self.1)).into_response()
    }
}

/// The body for a value that failed to deserialize.
///
/// The Go server answered every malformed body with this same opaque message and
/// deliberately did not echo parser details back to the client; keeping that
/// behaviour avoids leaking schema information.
#[must_use]
pub fn invalid_json() -> ApiError {
    ApiError::bad_request("invalid JSON request")
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;
    use revaro_core::ErrorCode;

    async fn body_of(response: Response) -> (StatusCode, serde_json::Value) {
        let status = response.status();
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        (status, json)
    }

    #[tokio::test]
    async fn plain_errors_use_the_shared_envelope() {
        let (status, body) = body_of(ApiError::not_found("file not found").into_response()).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(
            body,
            serde_json::json!({"error": {"status": 404, "message": "file not found"}})
        );
    }

    #[tokio::test]
    async fn coded_errors_carry_their_code() {
        let error = ApiError::unauthorized("enter your authenticator or recovery code")
            .with_code(ErrorCode::TOTP_REQUIRED);
        let (status, body) = body_of(error.into_response()).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["error"]["code"], "totp_required");
        assert_eq!(body["error"]["status"], 401);
    }

    #[tokio::test]
    async fn out_of_range_statuses_fall_back_to_500() {
        // `http::StatusCode` accepts any three-digit code, so 999 is legal;
        // only a value outside that range is unusable.
        let (status, _) = body_of(ApiError::new(999, "nonsense").into_response()).await;
        assert_eq!(status, StatusCode::from_u16(999).unwrap());

        let (status, _) = body_of(ApiError::new(1000, "nonsense").into_response()).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[tokio::test]
    async fn invalid_json_uses_the_historical_message() {
        let error = invalid_json();
        assert_eq!(error.status, 400);
        assert_eq!(error.message, "invalid JSON request");
    }

    #[tokio::test]
    async fn json_status_preserves_the_status_code() {
        let (status, body) =
            body_of(JsonStatus(StatusCode::CREATED, serde_json::json!({"id": 1})).into_response())
                .await;
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(body["id"], 1);
    }
}
