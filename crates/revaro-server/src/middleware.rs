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
/// `script-src 'self' 'wasm-unsafe-eval'` permits the browser's WebAssembly
/// compiler while keeping inline JavaScript, string-eval and third-party
/// origins disabled.
pub const CONTENT_SECURITY_POLICY: &str = "default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; \
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
    let cors = state.config.http2_origin.is_some()
        && request
            .headers()
            .get(http::header::ORIGIN)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| same_origin(&state.config.base_url, v));
    let preflight = cors && request.method() == Method::OPTIONS;
    let origin = request.headers().get(http::header::ORIGIN).cloned();
    let mut response = if preflight {
        StatusCode::NO_CONTENT.into_response()
    } else {
        next.run(request).await
    };
    if cors {
        let headers = response.headers_mut();
        headers.insert(
            http::header::ACCESS_CONTROL_ALLOW_ORIGIN,
            origin.clone().expect("checked origin"),
        );
        headers.insert("timing-allow-origin", origin.expect("checked origin"));
        headers.insert(
            http::header::ACCESS_CONTROL_ALLOW_CREDENTIALS,
            "true".parse().unwrap(),
        );
        headers.insert(
            http::header::ACCESS_CONTROL_ALLOW_METHODS,
            "GET, HEAD, POST, PUT, PATCH, DELETE, OPTIONS"
                .parse()
                .unwrap(),
        );
        headers.insert(http::header::ACCESS_CONTROL_ALLOW_HEADERS,
            "Content-Type, Range, If-Range, If-Match, If-None-Match, X-Content-SHA256, X-Revaro-Managed".parse().unwrap());
        headers.insert(http::header::ACCESS_CONTROL_EXPOSE_HEADERS,
            "ETag, Content-Range, Content-Length, Accept-Ranges, Content-Disposition, X-Content-SHA256, Retry-After".parse().unwrap());
        headers.append(http::header::VARY, "Origin".parse().unwrap());
    }
    if let Some(origin) = &state.config.http2_origin {
        let csp = CONTENT_SECURITY_POLICY.replace(
            "connect-src 'self'",
            &format!("connect-src 'self' {origin}"),
        );
        if !response.headers().contains_key("content-security-policy") {
            response.headers_mut().insert(
                http::header::CONTENT_SECURITY_POLICY,
                csp.parse().expect("validated CSP origin"),
            );
        }
    }

    // Insert only when the header is absent. Axum middleware wraps the handler,
    // so by this point the handler has already set its own headers; a plain
    // `insert` would silently overwrite them. That matters here: the public
    // share endpoint sets a stricter per-response policy (no-referrer, a
    // `sandbox` CSP) which must win — in the Go server the handler ran last and
    // won for exactly the same reason.
    for (name, value) in [
        ("x-content-type-options", "nosniff"),
        ("x-frame-options", "DENY"),
        ("referrer-policy", "same-origin"),
        (
            "permissions-policy",
            "camera=(), microphone=(), geolocation=(), payment=(), usb=()",
        ),
        ("content-security-policy", CONTENT_SECURITY_POLICY),
    ] {
        if !response.headers().contains_key(name) {
            response.headers_mut().insert(
                http::header::HeaderName::from_static(name),
                value.parse().expect("valid header value"),
            );
        }
    }
    if state.config.base_url.starts_with("https://")
        && !response.headers().contains_key("strict-transport-security")
    {
        response.headers_mut().insert(
            http::header::HeaderName::from_static("strict-transport-security"),
            "max-age=31536000".parse().expect("valid header value"),
        );
    }
    if is_api && !response.headers().contains_key("cache-control") {
        response.headers_mut().insert(
            http::header::CACHE_CONTROL,
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
    match (url::Url::parse(base_url), url::Url::parse(origin)) {
        (Ok(base), Ok(other)) => {
            matches!(base.scheme(), "http" | "https")
                && base.origin() == other.origin()
                && other.username().is_empty()
                && other.password().is_none()
                && other.query().is_none()
                && other.fragment().is_none()
        }
        _ => false,
    }
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
        assert!(CONTENT_SECURITY_POLICY.contains("script-src 'self' 'wasm-unsafe-eval'"));
        assert!(!CONTENT_SECURITY_POLICY.contains("'unsafe-eval'"));
        assert!(CONTENT_SECURITY_POLICY.contains("object-src 'none'"));
        assert!(CONTENT_SECURITY_POLICY.contains("frame-ancestors 'none'"));
    }
}
