//! Static delivery of the Leptos browser bundle.
//!
//! The Go server embedded the built SPA with `go:embed`, which forced the client
//! bundle to exist before the server could compile. The Rust server reads the
//! bundle from `APP_WEB_DIR` instead, so `cargo build -p revaro-server` never
//! depends on a wasm build having run first, and the client can be rebuilt and
//! swapped without touching the binary.
//!
//! Unknown paths fall back to `index.html` so client-side routes survive a page
//! reload, exactly like the SPA handler this replaces.

use std::path::{Component, Path, PathBuf};

use axum::body::Body;
use axum::extract::State;
use axum::http::{StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use tokio::io::AsyncReadExt as _;
use tokio_util::io::ReaderStream;

use crate::state::AppState;

/// Serve a file from the bundle, or `index.html` for a client-side route.
pub async fn serve(State(state): State<std::sync::Arc<AppState>>, uri: Uri) -> Response {
    let Some(relative) = safe_relative_path(uri.path()) else {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    };

    let Some(path) = resolve(&state.config.web_dir, &relative).await else {
        return index_response(&state.config.web_dir).await;
    };
    file_response(&path).await
}

/// Reject anything that could escape the bundle directory.
///
/// Percent-encoded separators and traversal segments are refused rather than
/// normalized: the bundle only ever contains plain relative paths, so anything
/// else is an attack or a bug.
fn safe_relative_path(uri_path: &str) -> Option<PathBuf> {
    let decoded = percent_decode(uri_path)?;
    // A NUL byte cannot appear in a legitimate bundle path and is rejected by
    // the OS in some contexts; refuse it rather than passing it through.
    if decoded.contains('\0') {
        return None;
    }
    let mut out = PathBuf::new();
    for component in Path::new(decoded.trim_start_matches('/')).components() {
        match component {
            Component::Normal(part) => out.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(out)
}

/// Minimal percent-decoding for URI paths.
///
/// Returns `None` on malformed escapes or invalid UTF-8, both of which would
/// otherwise let an encoded separator through to the filesystem.
fn percent_decode(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let high = *bytes.get(index + 1)?;
            let low = *bytes.get(index + 2)?;
            out.push(hex_value(high)? << 4 | hex_value(low)?);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(out).ok()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Resolve a relative path to a regular file inside `dir`, if one exists.
async fn resolve(dir: &Path, relative: &Path) -> Option<PathBuf> {
    if relative.as_os_str().is_empty() {
        return None;
    }
    let candidate = dir.join(relative);
    let metadata = tokio::fs::metadata(&candidate).await.ok()?;
    if metadata.is_file() {
        Some(candidate)
    } else {
        None
    }
}

async fn index_response(dir: &Path) -> Response {
    let index = dir.join("index.html");
    match tokio::fs::metadata(&index).await {
        Ok(metadata) if metadata.is_file() => file_response(&index).await,
        _ => (
            StatusCode::NOT_FOUND,
            "Revaro client bundle not found. Run `cargo xtask web-build` and set APP_WEB_DIR.",
        )
            .into_response(),
    }
}

async fn file_response(path: &Path) -> Response {
    let file = match tokio::fs::File::open(path).await {
        Ok(file) => file,
        Err(error) => {
            tracing::warn!(path = %path.display(), %error, "could not open web asset");
            return (StatusCode::NOT_FOUND, "not found").into_response();
        }
    };
    let stream = ReaderStream::new(file);
    let content_type = mime_guess::from_path(path).first_or_octet_stream();
    let cache_control = cache_policy(path);
    let mut response = Response::new(Body::from_stream(stream));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        content_type
            .as_ref()
            .parse()
            .expect("mime types are valid header values"),
    );
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        cache_control.parse().expect("valid header value"),
    );
    response
}

/// Cache policy for a bundle file.
///
/// The HTML shell must never be cached stale, because it names the module the
/// browser will load. Everything else is served with a short lifetime: the
/// bundle is not content-hashed, so `immutable` would be wrong.
fn cache_policy(path: &Path) -> &'static str {
    match path.extension().and_then(|extension| extension.to_str()) {
        Some("html") => "no-cache",
        _ => "private, max-age=3600",
    }
}

/// Read a bundle file fully, used by tests and small assets.
pub async fn read_asset(path: &Path) -> std::io::Result<Vec<u8>> {
    let mut file = tokio::fs::File::open(path).await?;
    let mut buffer = Vec::new();
    file.read_to_end(&mut buffer).await?;
    Ok(buffer)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_plain_relative_paths() {
        assert_eq!(
            safe_relative_path("/revaro_web.js").unwrap(),
            PathBuf::from("revaro_web.js")
        );
        assert_eq!(
            safe_relative_path("/pkg/app.wasm").unwrap(),
            PathBuf::from("pkg/app.wasm")
        );
        assert_eq!(safe_relative_path("/").unwrap(), PathBuf::new());
        assert_eq!(
            safe_relative_path("/./a/./b").unwrap(),
            PathBuf::from("a/b")
        );
    }

    #[test]
    fn rejects_traversal_and_absolute_paths() {
        assert!(safe_relative_path("/../secrets").is_none());
        assert!(safe_relative_path("/a/../../secrets").is_none());
        // Percent-encoded traversal must be decoded before the check.
        assert!(safe_relative_path("/%2e%2e/secrets").is_none());
        assert!(safe_relative_path("/%2E%2E%2Fsecrets").is_none());
        // A NUL byte or invalid escape is malformed.
        assert!(safe_relative_path("/%00").is_none());
        assert!(safe_relative_path("/%zz").is_none());
        assert!(safe_relative_path("/%2").is_none());
    }

    #[test]
    fn decodes_utf8_paths() {
        assert_eq!(percent_decode("/%E4%B8%AD.txt").unwrap(), "/中.txt");
    }

    #[test]
    fn html_is_never_cached_stale() {
        assert_eq!(cache_policy(Path::new("index.html")), "no-cache");
        assert_ne!(cache_policy(Path::new("revaro_web.wasm")), "no-cache");
    }
}
