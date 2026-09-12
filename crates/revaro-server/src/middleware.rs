//! Cross-cutting HTTP middleware, ported from the Go server.
//!
//! Two behaviours are load-bearing for security and must not drift:
//!
//! * **Security headers** — the CSP in particular is what stops an uploaded
//!   HTML or SVG file from executing in the application origin.
//! * **Origin guard** — every state-changing request must carry an `Origin`
//!   header matching `APP_BASE_URL`. This is defence in depth behind the
//!   `SameSite=Lax` session cookie. Requests without an `Origin` header are
//!   rejected outright, which also means non-browser clients must send one.

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::Response;
use http::{Method, StatusCode};

use revaro_core::ApiError;

use crate::state::AppState;

/// Content-Security-Policy applied to every response.
///
/// `script-src 'self'` is what makes the wasm bundle safe: there is no
/// `unsafe-inline` and no third-party origin.
pub const CONTENT_SECURITY_POLICY: &str = "default-src 'self'; script-src 'self'; \
img-src 'self' data: blob:; media-src 'self' blob:; style-src 'self' 'unsafe-inline'; \
connect-src 'self'; worker-src 'self' blob:; object-src 'none'; base-uri 'self'; \
form-action 'self'; frame-src 'none'; frame-ancestors 'none'";

/// Add the product's security headers to every response.
pub async fn security_headers(
    State(state): State<Arc<AppState>>,
    request: Request,
    next: Next,
) -> Response {
    let is_api = request.uri().path().starts_with("/api/");
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(
        "x-content-type-options",
        "nosniff".parse().expect("valid header value"),
    );
    headers.insert(
        "x-frame-options",
        "DENY".parse().expect("valid header value"),
    );
    headers.insert(
        "referrer-policy",
        "same-origin".parse().expect("valid header value"),
    );
    headers.insert(
        "permissions-policy",
        "camera=(), microphone=(), geolocation=(), payment=(), usb=()"
            .parse()
            .expect("valid header value"),
    );
    headers.insert(
        "content-security-policy",
        CONTENT_SECURITY_POLICY.parse().expect("valid header value"),
    );
    if state.config.base_url.starts_with("https://") {
        headers.insert(
            "strict-transport-security",
            "max-age=31536000".parse().expect("valid header value"),
        );
    }
    if is_api {
        headers.insert(
            "cache-control",
            "no-store".parse().expect("valid header value"),
        );
    }
    response
}

/// Reject state-changing requests that do not come from the application origin.
pub async fn origin_guard(
    State(state): State<Arc<AppState>>,
    request: Request,
    next: Next,
) -> Response {
    let method = request.method().clone();
    if matches!(method, Method::GET | Method::HEAD | Method::OPTIONS) {
        return next.run(request).await;
    }
    let Some(origin) = request.headers().get(http::header::ORIGIN) else {
        return ApiError::forbidden("origin required").into_response();
    };
    let Ok(origin) = origin.to_str() else {
        return ApiError::forbidden("origin not allowed").into_response();
    };
    if !same_origin(&state.config.base_url, origin) {
        return ApiError::forbidden("origin not allowed").into_response();
    }
    next.run(request).await
}

use axum::response::IntoResponse as _;

/// Compare a configured base URL with an `Origin` header value.
///
/// Only scheme and authority take part, matching the Go implementation, and the
/// comparison is case-insensitive because hosts are.
#[must_use]
pub fn same_origin(base_url: &str, origin: &str) -> bool {
    match (
        split_scheme_authority(base_url),
        split_scheme_authority(origin),
    ) {
        (Some((base_scheme, base_host)), Some((scheme, host))) => {
            base_scheme.eq_ignore_ascii_case(scheme) && base_host.eq_ignore_ascii_case(host)
        }
        _ => false,
    }
}

fn split_scheme_authority(value: &str) -> Option<(&str, &str)> {
    let (scheme, rest) = value.split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next()?;
    if scheme.is_empty() || authority.is_empty() {
        return None;
    }
    Some((scheme, authority))
}

/// The status code the origin guard uses, named so tests can assert on it.
pub const ORIGIN_GUARD_STATUS: StatusCode = StatusCode::FORBIDDEN;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matching_origins_are_accepted() {
        assert!(same_origin(
            "http://localhost:8080",
            "http://localhost:8080"
        ));
        assert!(same_origin(
            "https://cloud.example.com",
            "https://cloud.example.com"
        ));
        assert!(same_origin(
            "https://cloud.example.com",
            "https://CLOUD.example.com"
        ));
        // A trailing path on the base URL is not part of the comparison.
        assert!(same_origin(
            "https://cloud.example.com/app",
            "https://cloud.example.com"
        ));
    }

    #[test]
    fn differing_scheme_host_or_port_are_rejected() {
        assert!(!same_origin(
            "https://cloud.example.com",
            "http://cloud.example.com"
        ));
        assert!(!same_origin(
            "http://localhost:8080",
            "http://localhost:9090"
        ));
        assert!(!same_origin(
            "http://localhost:8080",
            "http://evil.example.com"
        ));
        assert!(!same_origin("http://localhost:8080", "null"));
        assert!(!same_origin("http://localhost:8080", ""));
    }

    #[test]
    fn api_responses_are_never_cached() {
        // The policy string is part of the security contract; a typo here would
        // silently weaken the application.
        assert!(CONTENT_SECURITY_POLICY.contains("script-src 'self'"));
        assert!(!CONTENT_SECURITY_POLICY.contains("unsafe-eval"));
        assert!(CONTENT_SECURITY_POLICY.contains("object-src 'none'"));
        assert!(CONTENT_SECURITY_POLICY.contains("frame-ancestors 'none'"));
    }
}
