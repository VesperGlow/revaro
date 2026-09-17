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
use axum::http::{HeaderMap, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use tokio::io::AsyncReadExt as _;
use tokio_util::io::ReaderStream;

use crate::state::AppState;

/// Serve a file from the bundle, or `index.html` for a client-side route.
pub async fn serve(
    State(state): State<std::sync::Arc<AppState>>,
    request_headers: HeaderMap,
    uri: Uri,
) -> Response {
    let Some(relative) = safe_relative_path(uri.path()) else {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    };

    let Some((path, metadata)) = resolve(&state.config.web_dir, &relative).await else {
        return index_response(&state.config.web_dir, &request_headers).await;
    };
    file_response(&path, &metadata, &request_headers).await
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
///
/// Returns the file metadata alongside the path so the response can build a
/// validator without a second `stat`.
async fn resolve(dir: &Path, relative: &Path) -> Option<(PathBuf, std::fs::Metadata)> {
    if relative.as_os_str().is_empty() {
        return None;
    }
    let candidate = dir.join(relative);
    let metadata = tokio::fs::metadata(&candidate).await.ok()?;
    if metadata.is_file() {
        Some((candidate, metadata))
    } else {
        None
    }
}

async fn index_response(dir: &Path, request_headers: &HeaderMap) -> Response {
    let index = dir.join("index.html");
    match tokio::fs::metadata(&index).await {
        Ok(metadata) if metadata.is_file() => {
            file_response(&index, &metadata, request_headers).await
        }
        _ => (
            StatusCode::NOT_FOUND,
            "Revaro client bundle not found. Run `cargo xtask web-build` and set APP_WEB_DIR.",
        )
            .into_response(),
    }
}

async fn file_response(
    path: &Path,
    metadata: &std::fs::Metadata,
    request_headers: &HeaderMap,
) -> Response {
    let etag = etag_for(metadata);
    let cache_control = cache_policy(path);
    let etag_header = etag.parse().expect("quoted etag is a header value");
    let cache_header = cache_control.parse().expect("valid header value");

    if request_headers
        .get(header::IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| etag_matches(value, &etag))
    {
        let mut response = StatusCode::NOT_MODIFIED.into_response();
        let headers = response.headers_mut();
        headers.insert(header::ETAG, etag_header);
        headers.insert(header::CACHE_CONTROL, cache_header);
        return response;
    }

    let file = match tokio::fs::File::open(path).await {
        Ok(file) => file,
        Err(error) => {
            tracing::warn!(path = %path.display(), %error, "could not open web asset");
            return (StatusCode::NOT_FOUND, "not found").into_response();
        }
    };
    let stream = ReaderStream::new(file);
    let content_type = mime_guess::from_path(path).first_or_octet_stream();
    let mut response = Response::new(Body::from_stream(stream));
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        content_type
            .as_ref()
            .parse()
            .expect("mime types are valid header values"),
    );
    headers.insert(header::CACHE_CONTROL, cache_header);
    headers.insert(header::ETAG, etag_header);
    response
}

/// A strong validator derived from size and modification time.
///
/// The bundle is not content-hashed, so the file name alone cannot tell a
/// browser whether a rebuilt asset changed; the validator can.
fn etag_for(metadata: &std::fs::Metadata) -> String {
    let size = metadata.len();
    let modified = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    format!("\"{size:x}-{modified:x}\"")
}

/// Match an `If-None-Match` list against our validator.
fn etag_matches(if_none_match: &str, etag: &str) -> bool {
    let current = etag.trim();
    if_none_match.split(',').any(|candidate| {
        let candidate = candidate.trim();
        candidate == "*"
            || candidate == current
            || candidate.strip_prefix("W/").map(str::trim) == Some(current)
    })
}

/// Cache policy for a bundle file.
///
/// The bundle files keep stable names across rebuilds, so anything the shell
/// loads (`html`, `js`, `wasm`, `css`) is stored but always revalidated with
/// its ETag: a rebuilt client is picked up on the next load instead of being
/// masked by a stale `max-age`. Images, icons and fonts keep a short lifetime.
fn cache_policy(path: &Path) -> &'static str {
    match path.extension().and_then(|extension| extension.to_str()) {
        Some("html" | "js" | "wasm" | "css") => "no-cache",
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
    fn shell_assets_are_revalidated_not_cached_stale() {
        // The bundle names are stable across rebuilds, so every asset the shell
        // loads must revalidate; otherwise a rebuilt client stays hidden behind
        // a stale `max-age`.
        for name in [
            "index.html",
            "revaro_boot.js",
            "revaro_web.js",
            "revaro_web_bg.wasm",
            "styles.css",
            "styles/shell.css",
        ] {
            assert_eq!(cache_policy(Path::new(name)), "no-cache", "{name}");
        }
        // Icons and images still get a short client cache.
        assert_eq!(
            cache_policy(Path::new("favicon.png")),
            "private, max-age=3600"
        );
    }

    #[test]
    fn etag_matching_handles_lists_and_weak_tags() {
        let etag = "\"1f-8\"";
        assert!(etag_matches("*", etag));
        assert!(etag_matches("\"1f-8\"", etag));
        assert!(etag_matches("W/\"1f-8\"", etag));
        assert!(etag_matches("\"other\", \"1f-8\"", etag));
        assert!(!etag_matches("\"other\"", etag));
        assert!(!etag_matches("", etag));
    }

    #[test]
    fn etag_changes_when_the_asset_changes() {
        let dir = std::env::temp_dir().join(format!("revaro-web-etag-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let path = dir.join("asset.bin");
        std::fs::write(&path, b"one").expect("write asset");
        let first = etag_for(&std::fs::metadata(&path).expect("stat asset"));
        std::fs::write(&path, b"a longer body").expect("rewrite asset");
        let second = etag_for(&std::fs::metadata(&path).expect("stat asset"));
        assert_ne!(first, second);
        assert!(first.starts_with('"') && first.ends_with('"'));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
