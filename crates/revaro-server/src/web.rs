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
use axum::http::{HeaderMap, Method, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use tokio_util::io::ReaderStream;

use crate::state::AppState;

/// Serve a file from the bundle, or `index.html` for a client-side route.
pub async fn serve(
    State(state): State<std::sync::Arc<AppState>>,
    method: Method,
    request_headers: HeaderMap,
    uri: Uri,
) -> Response {
    let Some(relative) = safe_relative_path(uri.path()) else {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    };

    let Some((path, metadata)) = resolve(&state.config.web_dir, &relative).await else {
        // A missing module must never become a successful HTML response.
        if relative
            .extension()
            .is_some_and(|extension| extension == "wasm")
        {
            return (StatusCode::NOT_FOUND, "WASM asset not found").into_response();
        }
        return index_response(
            &state.config.web_dir,
            &request_headers,
            method == Method::HEAD,
        )
        .await;
    };
    file_response(&path, &metadata, &request_headers, method == Method::HEAD).await
}

/// Minimal public-download bootstrap, independent of login or the WASM shell.
pub(crate) async fn download_shell(state: &AppState, headers: &HeaderMap) -> Response {
    let path = state.config.web_dir.join("share-download.html");
    match tokio::fs::metadata(&path).await {
        Ok(metadata) => file_response(&path, &metadata, headers, false).await,
        Err(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            "download client unavailable",
        )
            .into_response(),
    }
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

async fn index_response(dir: &Path, request_headers: &HeaderMap, head_only: bool) -> Response {
    let index = dir.join("index.html");
    match tokio::fs::metadata(&index).await {
        Ok(metadata) if metadata.is_file() => {
            file_response(&index, &metadata, request_headers, head_only).await
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
    head_only: bool,
) -> Response {
    let is_wasm = path
        .extension()
        .is_some_and(|extension| extension == "wasm");
    let mut representation = path.to_owned();
    let mut metadata = metadata.clone();
    let mut content_encoding = None;
    if is_wasm {
        let identity_quality = encoding_quality(request_headers, "identity");
        let mut codings = [
            (
                "br",
                "br",
                encoding_quality(request_headers, "br").unwrap_or(0),
            ),
            (
                "gzip",
                "gz",
                encoding_quality(request_headers, "gzip").unwrap_or(0),
            ),
            ("identity", "", identity_quality.unwrap_or(0)),
        ];
        // Client q-values take priority; prefer Brotli when weights are equal.
        codings.sort_by_key(|coding| std::cmp::Reverse(coding.2));
        for (coding, extension, quality) in codings {
            if quality == 0 {
                continue;
            }
            if coding == "identity" {
                break;
            }
            let candidate = path.with_extension(format!("wasm.{extension}"));
            if let Ok(candidate_metadata) = tokio::fs::metadata(&candidate).await
                && candidate_metadata.is_file()
            {
                representation = candidate;
                metadata = candidate_metadata;
                content_encoding = Some(coding);
                break;
            }
        }
        if content_encoding.is_none() && identity_quality == Some(0) {
            let mut response =
                (StatusCode::NOT_ACCEPTABLE, "no acceptable WASM encoding").into_response();
            response
                .headers_mut()
                .insert(header::VARY, "Accept-Encoding".parse().expect("valid Vary"));
            return response;
        }
    }
    let etag = match content_encoding {
        Some(coding) => format!("\"{coding}-{}\"", etag_for(&metadata).trim_matches('"')),
        None => etag_for(&metadata),
    };
    let cache_control = cache_policy(path);
    let etag_header = etag.parse().expect("quoted etag is a header value");
    let cache_header = cache_control.parse().expect("valid header value");

    let not_modified = request_headers
        .get(header::IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| etag_matches(value, &etag));
    let mut response = if not_modified {
        StatusCode::NOT_MODIFIED.into_response()
    } else if head_only {
        Response::new(Body::empty())
    } else {
        let file = match tokio::fs::File::open(&representation).await {
            Ok(file) => file,
            Err(error) => {
                tracing::warn!(path = %representation.display(), %error, "could not open web asset");
                return (StatusCode::NOT_FOUND, "not found").into_response();
            }
        };
        Response::new(Body::from_stream(ReaderStream::new(file)))
    };
    // MIME and cache policy describe the original URL, not the .br/.gz sidecar.
    let content_type = mime_guess::from_path(path).first_or_octet_stream();
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
    if is_wasm {
        headers.insert(header::VARY, "Accept-Encoding".parse().expect("valid Vary"));
    }
    if let Some(coding) = content_encoding {
        headers.insert(
            header::CONTENT_ENCODING,
            coding.parse().expect("valid content encoding"),
        );
    }
    if !not_modified {
        headers.insert(header::CONTENT_LENGTH, metadata.len().into());
    }
    response
}

/// Explicit exclusions override wildcards. Identity remains acceptable unless
/// it or `*` is excluded; an absent/empty field selects the original file.
fn encoding_quality(headers: &HeaderMap, coding: &str) -> Option<u16> {
    let mut specific = None;
    let mut wildcard = None;
    for value in headers.get_all(header::ACCEPT_ENCODING) {
        let Ok(value) = value.to_str() else { continue };
        for entry in value.split(',') {
            let mut parts = entry.split(';');
            let name = parts.next().unwrap_or_default().trim();
            let quality = parts
                .find_map(|part| {
                    let (name, value) = part.trim().split_once('=')?;
                    name.trim()
                        .eq_ignore_ascii_case("q")
                        .then_some(value.trim())
                })
                .map_or(1000, |value| {
                    value
                        .parse::<f32>()
                        .ok()
                        .filter(|q| (0.0..=1.0).contains(q))
                        .map_or(0, |q| (q * 1000.0) as u16)
                });
            if name.eq_ignore_ascii_case(coding) {
                specific = Some(quality);
            } else if name == "*" {
                wildcard = Some(quality);
            }
        }
    }
    specific.or_else(|| {
        if coding == "identity" {
            wildcard.filter(|quality| *quality == 0)
        } else {
            wildcard
        }
    })
}

/// A strong validator derived from size and modification time.
///
/// Stable shell names still need validators. Encoded variants use their own
/// metadata plus a coding prefix, so strong ETags cannot alias one another.
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
    if is_hashed_asset(path) {
        return "public, max-age=31536000, immutable";
    }
    match path.extension().and_then(|extension| extension.to_str()) {
        Some("html" | "js" | "wasm" | "css") => "no-cache",
        _ => "private, max-age=3600",
    }
}

fn is_hashed_asset(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let hash = name
        .strip_prefix("core.")
        .and_then(|name| name.strip_suffix(".wasm"))
        .or_else(|| {
            name.strip_prefix("revaro_web.")
                .and_then(|name| name.strip_suffix(".js"))
        });
    hash.is_some_and(|hash| {
        hash.len() == 64
            && hash
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
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
    fn immutable_requires_a_full_content_hash() {
        for name in [
            format!("core.{}.wasm", "a".repeat(64)),
            format!("revaro_web.{}.js", "0".repeat(64)),
        ] {
            assert_eq!(
                cache_policy(Path::new(&name)),
                "public, max-age=31536000, immutable"
            );
        }
        for name in ["core.wasm", "core.abc.wasm", "revaro_web.js", "index.html"] {
            assert_eq!(cache_policy(Path::new(name)), "no-cache");
        }
        assert!(!is_hashed_asset(Path::new(&format!(
            "core.{}.wasm",
            "z".repeat(64)
        ))));
    }

    #[tokio::test]
    async fn wasm_negotiates_precompressed_variants_and_revalidates_each_representation() {
        use http_body_util::BodyExt as _;

        let dir = std::env::temp_dir().join(format!("revaro-web-encoding-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("core.{}.wasm", "a".repeat(64)));
        for (suffix, bytes) in [
            ("", b"wasm".as_slice()),
            (".br", b"brotli"),
            (".gz", b"gzip"),
        ] {
            std::fs::write(format!("{}{suffix}", path.display()), bytes).unwrap();
        }
        let metadata = std::fs::metadata(&path).unwrap();
        let mut headers = HeaderMap::new();
        let mut etags = Vec::new();
        for (accept, expected_encoding, expected_body) in [
            ("gzip, br", Some("br"), b"brotli".as_slice()),
            ("br;q=0.4, gzip;q=0.8", Some("gzip"), b"gzip"),
            ("br;q=0, gzip", Some("gzip"), b"gzip"),
            ("*;q=1, br;q=0", Some("gzip"), b"gzip"),
            ("BR", Some("br"), b"brotli"),
            ("br;q=0.4, identity;q=1", None, b"wasm"),
            ("br;q=0, gzip;q=0", None, b"wasm"),
            ("", None, b"wasm"),
        ] {
            headers.insert(header::ACCEPT_ENCODING, accept.parse().unwrap());
            headers.remove(header::IF_NONE_MATCH);
            let response = file_response(&path, &metadata, &headers, false).await;
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(response.headers()[header::CONTENT_TYPE], "application/wasm");
            assert_eq!(response.headers()[header::VARY], "Accept-Encoding");
            assert_eq!(
                response.headers()[header::CACHE_CONTROL],
                "public, max-age=31536000, immutable"
            );
            assert_eq!(
                response.headers()[header::CONTENT_LENGTH],
                expected_body.len().to_string()
            );
            assert_eq!(
                response
                    .headers()
                    .get(header::CONTENT_ENCODING)
                    .map(|v| v.to_str().unwrap()),
                expected_encoding
            );
            let etag = response.headers()[header::ETAG].clone();
            etags.push(etag.clone());
            assert_eq!(
                response.into_body().collect().await.unwrap().to_bytes(),
                expected_body
            );

            headers.insert(header::IF_NONE_MATCH, etag);
            let cached = file_response(&path, &metadata, &headers, false).await;
            assert_eq!(cached.status(), StatusCode::NOT_MODIFIED);
            assert_eq!(cached.headers()[header::VARY], "Accept-Encoding");
            assert_eq!(
                cached
                    .headers()
                    .get(header::CONTENT_ENCODING)
                    .map(|v| v.to_str().unwrap()),
                expected_encoding
            );
            assert!(
                cached
                    .into_body()
                    .collect()
                    .await
                    .unwrap()
                    .to_bytes()
                    .is_empty()
            );
        }
        assert_ne!(etags[0], etags[1]);
        assert_ne!(etags[0], etags[5]);
        headers.insert(header::ACCEPT_ENCODING, "gzip".parse().unwrap());
        headers.insert(header::IF_NONE_MATCH, etags[0].clone());
        assert_eq!(
            file_response(&path, &metadata, &headers, false)
                .await
                .status(),
            StatusCode::OK
        );

        headers.remove(header::IF_NONE_MATCH);
        let head = file_response(&path, &metadata, &headers, true).await;
        assert_eq!(head.headers()[header::CONTENT_ENCODING], "gzip");
        assert_eq!(head.headers()[header::CONTENT_LENGTH], "4");
        assert!(
            head.into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes()
                .is_empty()
        );

        std::fs::remove_file(path.with_extension("wasm.br")).unwrap();
        headers.insert(header::ACCEPT_ENCODING, "br, gzip".parse().unwrap());
        assert_eq!(
            file_response(&path, &metadata, &headers, false)
                .await
                .headers()[header::CONTENT_ENCODING],
            "gzip"
        );
        std::fs::remove_file(path.with_extension("wasm.gz")).unwrap();
        assert!(
            !file_response(&path, &metadata, &headers, false)
                .await
                .headers()
                .contains_key(header::CONTENT_ENCODING)
        );
        headers.insert(
            header::ACCEPT_ENCODING,
            "identity;q=0, *;q=0".parse().unwrap(),
        );
        assert_eq!(
            file_response(&path, &metadata, &headers, false)
                .await
                .status(),
            StatusCode::NOT_ACCEPTABLE
        );
        std::fs::remove_dir_all(dir).unwrap();
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
