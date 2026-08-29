use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Serialize;

#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    message: String,
    code: Option<String>,
}

#[derive(Serialize)]
struct ErrorBody<'a> {
    error: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    code: Option<&'a str>,
}

impl ApiError {
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: message.into(),
            code: None,
        }
    }

    pub fn payload_too_large(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::PAYLOAD_TOO_LARGE,
            message: message.into(),
            code: None,
        }
    }

    pub fn upstream(error: impl std::fmt::Display) -> Self {
        Self::upstream_domain("s3", error)
    }

    pub fn upstream_domain(domain: &str, error: impl std::fmt::Display) -> Self {
        let message = error.to_string();
        let status = if message.contains("NoSuchKey") || message.contains("NotFound") {
            StatusCode::NOT_FOUND
        } else if message.contains("PreconditionFailed") {
            StatusCode::PRECONDITION_FAILED
        } else {
            StatusCode::BAD_GATEWAY
        };
        Self {
            status,
            message,
            code: Some(domain.into()),
        }
    }

    pub fn cancelled(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::REQUEST_TIMEOUT,
            message: message.into(),
            code: Some("cancelled".into()),
        }
    }

    pub fn range_not_satisfiable(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::RANGE_NOT_SATISFIABLE,
            message: message.into(),
            code: Some("range".into()),
        }
    }

    pub fn conflict(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            message: message.into(),
            code: Some(code.into()),
        }
    }

    pub fn internal(message: impl std::fmt::Display) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: message.to_string(),
            code: None,
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = ErrorBody {
            error: &self.message,
            code: self.code.as_deref(),
        };
        (self.status, Json(body)).into_response()
    }
}
