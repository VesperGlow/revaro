//! Shared HTTP representations and transfer admission, independent of file type.
use axum::body::Body;
use bytes::Bytes;
use futures_util::StreamExt as _;
use revaro_core::ApiError;
use std::convert::Infallible;
use tokio::io::{AsyncReadExt as _, AsyncSeekExt as _};
use tokio::sync::OwnedSemaphorePermit;

use crate::file_access::ReadSeek;

pub fn hold_permit(body: Body, permit: OwnedSemaphorePermit) -> Body {
    Body::from_stream(futures_util::stream::unfold(
        (body.into_data_stream(), permit),
        |(mut stream, permit)| async move { stream.next().await.map(|chunk| (chunk, (stream, permit))) },
    ))
}

struct TransferObservation {
    started: std::time::Instant,
    method: http::Method,
    range: Option<http::HeaderValue>,
    headers_ms: u64,
    status: u16,
    expected: u64,
    bytes: u64,
    outcome: &'static str,
}

impl Drop for TransferObservation {
    fn drop(&mut self) {
        tracing::debug!(
            event = "file_transfer",
            method = %self.method,
            range = ?self.range,
            status = self.status,
            headers_ms = self.headers_ms,
            bytes = self.bytes,
            expected_bytes = self.expected,
            elapsed_ms = self.started.elapsed().as_millis() as u64,
            outcome = self.outcome,
            "file response body finished"
        );
    }
}

fn observed_response(
    mut response: axum::response::Response,
    method: http::Method,
    range: Option<http::HeaderValue>,
    started: std::time::Instant,
) -> axum::response::Response {
    let expected = if method == http::Method::HEAD {
        0
    } else {
        response
            .headers()
            .get(http::header::CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok()?.parse().ok())
            .unwrap_or(0)
    };
    let observation = TransferObservation {
        headers_ms: started.elapsed().as_millis() as u64,
        started,
        method,
        range,
        status: response.status().as_u16(),
        expected,
        bytes: 0,
        outcome: if expected == 0 {
            "completed"
        } else {
            "cancelled"
        },
    };
    let body = std::mem::replace(response.body_mut(), Body::empty());
    *response.body_mut() = Body::from_stream(futures_util::stream::unfold(
        (body.into_data_stream(), observation),
        |(mut stream, mut observation)| async move {
            match stream.next().await {
                Some(chunk) => {
                    if let Ok(bytes) = &chunk {
                        observation.bytes += bytes.len() as u64;
                        // HTTP can stop polling once Content-Length is met,
                        // without polling the underlying stream's final None.
                        if observation.bytes == observation.expected
                            && observation.outcome != "failed"
                        {
                            observation.outcome = "completed";
                        }
                    } else {
                        observation.outcome = "failed";
                    }
                    Some((chunk, (stream, observation)))
                }
                None => {
                    observation.outcome = if observation.bytes == observation.expected
                        && observation.outcome != "failed"
                    {
                        "completed"
                    } else {
                        "failed"
                    };
                    drop(observation);
                    None
                }
            }
        },
    ));
    response
}

/// Observe native and managed delivery at the same boundary, after Axum has
/// removed HEAD bodies. Does not pre-read or buffer any response bytes.
pub async fn monitor(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let method = request.method().clone();
    let range = request.headers().get(http::header::RANGE).cloned();
    let path = request.uri().path();
    let applies = matches!(method, http::Method::GET | http::Method::HEAD)
        && (path.starts_with("/api/files/") || path.starts_with("/s/"));
    let started = std::time::Instant::now();
    let response = next.run(request).await;
    if applies
        && (response.headers().contains_key(http::header::ACCEPT_RANGES)
            || response.headers().contains_key(http::header::CONTENT_RANGE)
            || response.status() == http::StatusCode::NOT_MODIFIED)
    {
        observed_response(response, method, range, started)
    } else {
        response
    }
}

/// One range as parsed by Go's `http.ServeContent`.
#[derive(Clone, Copy)]
struct ByteRange {
    start: u64,
    length: u64,
}

/// The two errors exposed by `http.ServeContent`'s range parser.
enum RangeError {
    /// The header could not be parsed as a byte-range-set.
    Invalid,
    /// Every syntactically valid range started after the representation.
    NoOverlap,
}

/// Parse byte ranges using RFC 9110. Multi-range responses use multipart;
/// unsupported units are ignored and zero-length suffixes are unsatisfiable.
fn parse_range(header: Option<&str>, size: u64) -> Result<Vec<ByteRange>, RangeError> {
    let Some(value) = header else {
        return Ok(Vec::new());
    };
    if value.is_empty() {
        return Ok(Vec::new());
    }
    let Some((unit, spec)) = value.split_once('=') else {
        return Err(RangeError::Invalid);
    };
    if !unit.eq_ignore_ascii_case("bytes") {
        return Ok(Vec::new());
    }

    let size = i64::try_from(size).unwrap_or(i64::MAX);
    let mut ranges = Vec::new();
    let mut no_overlap = false;
    for raw_range in spec.split(',') {
        let raw_range = raw_range.trim();
        if raw_range.is_empty() {
            continue;
        }
        let Some((start, end)) = raw_range.split_once('-') else {
            return Err(RangeError::Invalid);
        };
        let start = start.trim();
        let end = end.trim();
        let range = if start.is_empty() {
            if end.is_empty() || end.starts_with('-') {
                return Err(RangeError::Invalid);
            }
            let length = end.parse::<i64>().map_err(|_| RangeError::Invalid)?;
            if length < 0 {
                return Err(RangeError::Invalid);
            }
            let length = length.min(size);
            if length == 0 {
                no_overlap = true;
                continue;
            }
            ByteRange {
                start: (size - length) as u64,
                length: length as u64,
            }
        } else {
            let start = start.parse::<i64>().map_err(|_| RangeError::Invalid)?;
            if start < 0 {
                return Err(RangeError::Invalid);
            }
            if start >= size {
                no_overlap = true;
                continue;
            }
            let length = if end.is_empty() {
                size - start
            } else {
                let mut end = end.parse::<i64>().map_err(|_| RangeError::Invalid)?;
                if start > end {
                    return Err(RangeError::Invalid);
                }
                if end >= size {
                    end = size - 1;
                }
                end - start + 1
            };
            ByteRange {
                start: start as u64,
                length: length as u64,
            }
        };
        ranges.push(range);
    }

    if no_overlap && ranges.is_empty() {
        Err(RangeError::NoOverlap)
    } else {
        Ok(ranges)
    }
}

fn range_end(range: ByteRange) -> i128 {
    i128::from(range.start) + i128::from(range.length) - 1
}

fn random_multipart_boundary() -> String {
    // Match Go's mime/multipart default: thirty random bytes rendered as
    // lowercase hexadecimal (the exact value is intentionally per-response).
    hex::encode(rand::random::<[u8; 30]>())
}

fn multipart_part_header(
    boundary: &str,
    range: ByteRange,
    size: u64,
    content_type: &str,
    first: bool,
) -> String {
    let prefix = if first { "--" } else { "\r\n--" };
    format!(
        "{prefix}{boundary}\r\nContent-Range: bytes {}-{}/{}\r\nContent-Type: {content_type}\r\n\r\n",
        range.start,
        range_end(range),
        size,
    )
}

fn multipart_footer(boundary: &str) -> String {
    format!("\r\n--{boundary}--\r\n")
}

fn multipart_length(ranges: &[ByteRange], boundary: &str, size: u64, content_type: &str) -> u64 {
    ranges.iter().enumerate().fold(
        multipart_footer(boundary).len() as u64,
        |total, (index, range)| {
            total
                .saturating_add(
                    multipart_part_header(boundary, *range, size, content_type, index == 0).len()
                        as u64,
                )
                .saturating_add(range.length)
        },
    )
}

struct MultipartRangeState {
    file: Box<dyn ReadSeek>,
    ranges: Vec<ByteRange>,
    boundary: String,
    content_type: String,
    size: u64,
    index: usize,
    remaining: u64,
    phase: MultipartPhase,
}

enum MultipartPhase {
    Header,
    Body,
    Footer,
    Done,
}

fn multipart_stream(
    file: Box<dyn ReadSeek>,
    ranges: Vec<ByteRange>,
    boundary: String,
    content_type: String,
    size: u64,
) -> impl futures_util::Stream<Item = Result<Bytes, std::io::Error>> {
    futures_util::stream::unfold(
        MultipartRangeState {
            file,
            ranges,
            boundary,
            content_type,
            size,
            index: 0,
            remaining: 0,
            phase: MultipartPhase::Header,
        },
        |mut state| async move {
            loop {
                match state.phase {
                    MultipartPhase::Header => {
                        if state.index >= state.ranges.len() {
                            state.phase = MultipartPhase::Footer;
                            continue;
                        }
                        let range = state.ranges[state.index];
                        if let Err(error) =
                            state.file.seek(std::io::SeekFrom::Start(range.start)).await
                        {
                            state.phase = MultipartPhase::Done;
                            return Some((Err(error), state));
                        }
                        state.remaining = range.length;
                        state.phase = MultipartPhase::Body;
                        let header = multipart_part_header(
                            &state.boundary,
                            range,
                            state.size,
                            &state.content_type,
                            state.index == 0,
                        );
                        return Some((Ok(Bytes::from(header)), state));
                    }
                    MultipartPhase::Body => {
                        if state.remaining == 0 {
                            state.index += 1;
                            state.phase = MultipartPhase::Header;
                            continue;
                        }
                        let chunk_size = state.remaining.min(64 * 1024) as usize;
                        let mut buffer = vec![0_u8; chunk_size];
                        match state.file.read(&mut buffer).await {
                            Ok(0) => {
                                state.phase = MultipartPhase::Done;
                                return Some((
                                    Err(std::io::Error::new(
                                        std::io::ErrorKind::UnexpectedEof,
                                        "object ended before the requested range",
                                    )),
                                    state,
                                ));
                            }
                            Ok(read) => {
                                state.remaining -= read as u64;
                                return Some((Ok(Bytes::copy_from_slice(&buffer[..read])), state));
                            }
                            Err(error) => {
                                state.phase = MultipartPhase::Done;
                                return Some((Err(error), state));
                            }
                        }
                    }
                    MultipartPhase::Footer => {
                        state.phase = MultipartPhase::Done;
                        return Some((Ok(Bytes::from(multipart_footer(&state.boundary))), state));
                    }
                    MultipartPhase::Done => return None,
                }
            }
        },
    )
}

fn range_error_response(
    disposition: &str,
    message: &str,
    content_range: Option<String>,
) -> axum::response::Response {
    let body = Bytes::from(format!("{message}\n"));
    let mut response = axum::response::Response::new(axum::body::Body::from(body.clone()));
    *response.status_mut() = http::StatusCode::RANGE_NOT_SATISFIABLE;
    let headers = response.headers_mut();
    headers.insert(
        http::header::CACHE_CONTROL,
        "private, no-cache".parse().unwrap(),
    );
    headers.insert(
        http::header::CONTENT_TYPE,
        "text/plain; charset=utf-8"
            .parse()
            .expect("valid error content type"),
    );
    headers.insert(
        http::header::CONTENT_DISPOSITION,
        disposition.parse().expect("valid content disposition"),
    );
    headers.insert(
        http::header::HeaderName::from_static("x-content-type-options"),
        "nosniff".parse().expect("valid nosniff header"),
    );
    if let Some(content_range) = content_range {
        headers.insert(
            http::header::CONTENT_RANGE,
            content_range.parse().expect("valid content range"),
        );
    }
    headers.insert(
        http::header::CONTENT_LENGTH,
        body.len()
            .to_string()
            .parse()
            .expect("valid content length"),
    );
    response
}

fn if_none_match_matches(value: &str, etag: &str) -> bool {
    value.split(',').any(|candidate| {
        let candidate = candidate.trim();
        if candidate == "*" {
            return true;
        }
        let candidate = candidate.strip_prefix("W/").unwrap_or(candidate).trim();
        let current = etag.strip_prefix("W/").unwrap_or(etag).trim();
        candidate == current && candidate.starts_with('"') && candidate.ends_with('"')
    })
}

fn not_modified_response(disposition: &str, etag: &str) -> axum::response::Response {
    let body =
        axum::body::Body::from_stream(futures_util::stream::empty::<Result<Bytes, Infallible>>());
    let mut response = axum::response::Response::new(body);
    *response.status_mut() = http::StatusCode::NOT_MODIFIED;
    let headers = response.headers_mut();
    headers.insert(
        http::header::CACHE_CONTROL,
        "private, no-cache".parse().unwrap(),
    );
    headers.insert(
        http::header::CONTENT_DISPOSITION,
        disposition.parse().expect("valid content disposition"),
    );
    headers.insert(
        http::header::ETAG,
        etag.parse().expect("quoted etag is a header value"),
    );
    response
}

/// Deliver any seekable representation with the same validators and Range rules.
pub async fn serve_reader(
    reader: Box<dyn ReadSeek>,
    size: u64,
    validator: &str,
    mime: &str,
    disposition: &str,
    request_headers: http::HeaderMap,
) -> Result<axum::response::Response, ApiError> {
    // A quoted validator, as the product has always sent it.
    let etag = format!("\"{}\"", validator.replace('"', ""));

    // `If-Range` with a non-matching validator means the client's partial copy is
    // stale, so the range is ignored and the whole file is sent.
    if request_headers
        .get(http::header::IF_MATCH)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|value| value != "*" && !value.split(',').any(|v| v.trim() == etag))
    {
        return Err(ApiError::new(
            412,
            "representation changed; restart the transfer",
        ));
    }

    // Go's ServeContent evaluates If-None-Match before Range and emits a 304
    // with the validator and disposition retained, but without representation
    // headers such as Content-Type, Content-Length or Accept-Ranges.
    if request_headers
        .get(http::header::IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| if_none_match_matches(value, &etag))
    {
        return Ok(not_modified_response(disposition, &etag));
    }

    let range_header = match request_headers
        .get(http::header::IF_RANGE)
        .and_then(|value| value.to_str().ok())
    {
        // Go treats an empty If-Range as absent. A non-matching validator
        // suppresses Range and sends the complete representation.
        Some(if_range) if !if_range.trim().is_empty() && if_range.trim() != etag => None,
        _ => request_headers
            .get(http::header::RANGE)
            .and_then(|value| value.to_str().ok()),
    };
    let ranges = match parse_range(range_header, size) {
        Ok(ranges) => ranges,
        Err(RangeError::NoOverlap) => {
            let mut response = range_error_response(
                disposition,
                "invalid range: failed to overlap",
                Some(format!("bytes */{size}")),
            );
            response
                .headers_mut()
                .insert(http::header::ETAG, etag.parse().expect("valid ETag"));
            return Ok(response);
        }
        Err(RangeError::Invalid) => {
            let mut response = range_error_response(
                disposition,
                "invalid range",
                Some(format!("bytes */{size}")),
            );
            response
                .headers_mut()
                .insert(http::header::ETAG, etag.parse().expect("valid ETag"));
            return Ok(response);
        }
    };
    // Go deliberately ignores a range-set whose encoded bytes would be larger
    // than the representation (an inexpensive guard against range bombs).
    let range_bytes = ranges
        .iter()
        .fold(0_u64, |total, range| total.saturating_add(range.length));
    let ranges = if range_bytes > size {
        Vec::new()
    } else {
        ranges
    };

    // `Body::from_stream` cannot set a length, so it is tracked here and added
    // below; a `206` must advertise the range's length, not the object's.
    let mut response = match ranges.as_slice() {
        [] => {
            let stream = tokio_util::io::ReaderStream::with_capacity(reader.take(size), 64 * 1024);
            axum::response::Response::new(axum::body::Body::from_stream(stream))
        }
        [range] => {
            let mut file_handle = reader;
            file_handle
                .seek(std::io::SeekFrom::Start(range.start))
                .await
                .map_err(|error| {
                    tracing::error!(%error, "object seek failed");
                    ApiError::new(502, "object storage read failed")
                })?;
            let stream = tokio_util::io::ReaderStream::with_capacity(
                file_handle.take(range.length),
                64 * 1024,
            );
            let mut response = axum::response::Response::new(axum::body::Body::from_stream(stream));
            *response.status_mut() = http::StatusCode::PARTIAL_CONTENT;
            response.headers_mut().insert(
                http::header::CONTENT_RANGE,
                format!("bytes {}-{}/{size}", range.start, range_end(*range))
                    .parse()
                    .expect("valid content range"),
            );
            response
        }
        _ => {
            let boundary = random_multipart_boundary();
            let content_length = multipart_length(&ranges, &boundary, size, mime);
            let stream = multipart_stream(
                reader,
                ranges.clone(),
                boundary.clone(),
                mime.to_owned(),
                size,
            );
            let mut response = axum::response::Response::new(axum::body::Body::from_stream(stream));
            *response.status_mut() = http::StatusCode::PARTIAL_CONTENT;
            let headers = response.headers_mut();
            headers.insert(
                http::header::CONTENT_TYPE,
                format!("multipart/byteranges; boundary={boundary}")
                    .parse()
                    .expect("valid multipart content type"),
            );
            headers.insert(
                http::header::CONTENT_LENGTH,
                content_length
                    .to_string()
                    .parse()
                    .expect("valid content length"),
            );
            response
        }
    };

    let headers = response.headers_mut();
    // Browser HTTP caching and application block caching both retain bytes
    // while requiring authorization/version validation before every reuse.
    // Public-share delivery overrides this with its stricter no-store policy.
    headers.insert(
        http::header::CACHE_CONTROL,
        "private, no-cache".parse().unwrap(),
    );
    if ranges.len() <= 1 {
        headers.insert(
            http::header::CONTENT_TYPE,
            mime.parse().unwrap_or_else(|_| {
                "application/octet-stream"
                    .parse()
                    .expect("valid fallback content type")
            }),
        );
    }
    headers.insert(
        http::header::CONTENT_DISPOSITION,
        disposition
            .parse()
            .map_err(|_| ApiError::internal("could not build the download header"))?,
    );
    headers.insert(
        http::header::ETAG,
        etag.parse().expect("quoted etag is a header value"),
    );
    headers.insert(
        http::header::ACCEPT_RANGES,
        "bytes".parse().expect("valid accept ranges"),
    );
    if ranges.len() <= 1 {
        let length = ranges.first().map_or(size, |range| range.length);
        headers.insert(
            http::header::CONTENT_LENGTH,
            length.to_string().parse().expect("valid content length"),
        );
    }
    Ok(response)
}

/// Derived resources and JSON file contents share the disk-file transport.
pub async fn serve_bytes(
    data: Bytes,
    mime: &str,
    disposition: &str,
    headers: http::HeaderMap,
) -> Result<axum::response::Response, ApiError> {
    let etag = revaro_core::keys::sha256_hex(&data);
    let size = data.len() as u64;
    serve_reader(
        Box::new(std::io::Cursor::new(data)),
        size,
        &etag,
        mime,
        disposition,
        headers,
    )
    .await
}

/// Give bounded, generated file resources the same protocol as original files.
/// Original-file streams already carry Accept-Ranges and are never buffered.
pub async fn file_resources(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use axum::response::IntoResponse as _;
    // Only the two bounded JSON content adapters need conversion. Lists,
    // metadata and progress are mutable control APIs; buffering and hashing
    // those here invents file semantics and delays their response headers.
    let segments: Vec<_> = request
        .uri()
        .path()
        .trim_start_matches('/')
        .split('/')
        .collect();
    let content = matches!(
        segments.as_slice(),
        ["api", "files", _, "content"] | ["api", "files", _, "versions", _, "content"]
    );
    let applies = request.method() == http::Method::GET && content;
    let headers = request.headers().clone();
    let response = next.run(request).await;
    if !applies
        || response.status() != http::StatusCode::OK
        || response.headers().contains_key(http::header::ACCEPT_RANGES)
    {
        return response;
    }
    let (parts, body) = response.into_parts();
    let data = match axum::body::to_bytes(body, 64 << 20).await {
        Ok(data) => data,
        Err(error) => {
            tracing::warn!(%error, "file resource body failed");
            return ApiError::new(502, "file resource read failed").into_response();
        }
    };
    let mime = parts
        .headers
        .get(http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/octet-stream");
    let disposition = parts
        .headers
        .get(http::header::CONTENT_DISPOSITION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("inline");
    match serve_bytes(data, mime, disposition, headers).await {
        Ok(mut response) => {
            for (name, value) in parts.headers.iter() {
                if !matches!(
                    name.as_str(),
                    "content-length"
                        | "content-range"
                        | "etag"
                        | "accept-ranges"
                        | "content-type"
                        | "content-disposition"
                ) {
                    response.headers_mut().insert(name, value.clone());
                }
            }
            response
        }
        Err(error) => error.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt as _;
    use std::{
        pin::Pin,
        sync::{
            Arc,
            atomic::{AtomicU64, Ordering},
        },
        task::{Context, Poll},
    };
    use tokio::io::{AsyncRead, AsyncSeek};

    #[tokio::test]
    async fn range_edges_follow_http_semantics_and_control_apis_remain_streamed() {
        for (bytes, range, status, content_range) in [
            (b"".as_slice(), "bytes=0-", 416, Some("bytes */0")),
            (b"".as_slice(), "bytes=-1", 416, Some("bytes */0")),
            (b"0123".as_slice(), "bytes=-0", 416, Some("bytes */4")),
            (b"0123".as_slice(), "items=0-1", 200, None),
            (b"0123".as_slice(), "Bytes=0-1", 206, Some("bytes 0-1/4")),
        ] {
            let mut headers = http::HeaderMap::new();
            headers.insert(http::header::RANGE, range.parse().unwrap());
            let response = serve_bytes(
                Bytes::copy_from_slice(bytes),
                "text/plain",
                "inline",
                headers,
            )
            .await
            .unwrap();
            assert_eq!(response.status().as_u16(), status);
            assert_eq!(
                response
                    .headers()
                    .get(http::header::CONTENT_RANGE)
                    .map(|v| v.to_str().unwrap()),
                content_range
            );
            assert_eq!(
                response.headers()[http::header::CACHE_CONTROL],
                "private, no-cache"
            );
            assert!(response.headers().contains_key(http::header::ETAG));
        }
        use axum::{Router, routing::get};
        use tower::ServiceExt as _;
        let app = Router::new()
            .route(
                "/api/files/test/media/progress",
                get(|| async {
                    Body::from_stream(
                        futures_util::stream::pending::<Result<Bytes, std::io::Error>>(),
                    )
                }),
            )
            .layer(axum::middleware::from_fn(file_resources));
        let response = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            app.oneshot(
                http::Request::builder()
                    .uri("/api/files/test/media/progress")
                    .body(Body::empty())
                    .unwrap(),
            ),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(response.status(), http::StatusCode::OK);
        assert!(!response.headers().contains_key(http::header::ACCEPT_RANGES));
    }

    /// A seekable 8 GiB object that records actual reads without allocating it.
    struct CountedAudio {
        position: u64,
        size: u64,
        read: Arc<AtomicU64>,
    }
    impl AsyncRead for CountedAudio {
        fn poll_read(
            mut self: Pin<&mut Self>,
            _: &mut Context<'_>,
            buffer: &mut tokio::io::ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            let length = (self.size - self.position).min(buffer.remaining() as u64) as usize;
            for offset in 0..length {
                buffer.put_slice(&[((self.position + offset as u64) % 251) as u8]);
            }
            self.position += length as u64;
            self.read.fetch_add(length as u64, Ordering::SeqCst);
            Poll::Ready(Ok(()))
        }
    }
    impl AsyncSeek for CountedAudio {
        fn start_seek(
            mut self: Pin<&mut Self>,
            position: std::io::SeekFrom,
        ) -> std::io::Result<()> {
            self.position = match position {
                std::io::SeekFrom::Start(offset) => offset,
                std::io::SeekFrom::End(offset) => self.size.checked_add_signed(offset).unwrap(),
                std::io::SeekFrom::Current(offset) => {
                    self.position.checked_add_signed(offset).unwrap()
                }
            };
            Ok(())
        }
        fn poll_complete(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<u64>> {
            Poll::Ready(Ok(self.position))
        }
    }

    #[tokio::test]
    async fn long_audio_ranges_seek_without_reading_the_prefix_or_whole_object() {
        let size = 8_u64 * 1024 * 1024 * 1024;
        for mime in ["audio/flac", "audio/wav"] {
            for (header, start, length) in [
                (
                    "bytes=5368709120-5368713215",
                    5_u64 * 1024 * 1024 * 1024,
                    4096_u64,
                ),
                ("bytes=-4096", size - 4096, 4096),
                ("bytes=8192-", 8192, size - 8192),
            ] {
                let read = Arc::new(AtomicU64::new(0));
                let reader = CountedAudio {
                    position: 0,
                    size,
                    read: read.clone(),
                };
                let mut headers = http::HeaderMap::new();
                headers.insert(http::header::RANGE, header.parse().unwrap());
                let response = serve_reader(
                    Box::new(reader),
                    size,
                    "audio-version",
                    mime,
                    "inline",
                    headers,
                )
                .await
                .unwrap();
                assert_eq!(response.status(), http::StatusCode::PARTIAL_CONTENT);
                assert_eq!(response.headers()[http::header::CONTENT_TYPE], mime);
                assert_eq!(response.headers()[http::header::ACCEPT_RANGES], "bytes");
                assert_eq!(
                    response.headers()[http::header::CONTENT_RANGE],
                    format!("bytes {start}-{}/{size}", start + length - 1)
                );
                assert_eq!(
                    response.headers()[http::header::CONTENT_LENGTH],
                    length.to_string()
                );
                assert_eq!(
                    read.load(Ordering::SeqCst),
                    0,
                    "headers do not require a full download"
                );
                let mut body = response.into_body();
                let frame = body.frame().await.unwrap().unwrap().into_data().unwrap();
                assert!(frame.len() <= 64 * 1024);
                for (index, byte) in frame.iter().enumerate() {
                    assert_eq!(*byte, ((start + index as u64) % 251) as u8);
                }
                assert_eq!(read.load(Ordering::SeqCst), frame.len() as u64);
                if length == 4096 {
                    assert!(body.frame().await.is_none());
                }
                drop(body);
                assert!(
                    read.load(Ordering::SeqCst) <= 64 * 1024,
                    "cancellation stops streaming"
                );
            }
        }
    }

    #[tokio::test]
    async fn generated_resources_share_ranges_and_strong_preconditions() {
        for mime in [
            "application/pdf",
            "application/zip",
            "application/json",
            "text/html",
            "image/jpeg",
        ] {
            let data = Bytes::from_static(b"0123456789");
            let full = serve_bytes(data.clone(), mime, "inline", http::HeaderMap::new())
                .await
                .unwrap();
            let etag = full.headers()[http::header::ETAG].clone();
            let mut headers = http::HeaderMap::new();
            headers.insert(http::header::RANGE, "bytes=3-7".parse().unwrap());
            headers.insert(http::header::IF_RANGE, etag.clone());
            headers.insert(http::header::IF_MATCH, etag);
            let response = serve_bytes(data.clone(), mime, "inline", headers.clone())
                .await
                .unwrap();
            assert_eq!(response.status(), http::StatusCode::PARTIAL_CONTENT);
            assert_eq!(
                response.headers()[http::header::CONTENT_RANGE],
                "bytes 3-7/10"
            );
            assert_eq!(
                response.into_body().collect().await.unwrap().to_bytes(),
                "34567"
            );
            headers.insert(http::header::IF_MATCH, "\"other-version\"".parse().unwrap());
            let error = serve_bytes(data, mime, "inline", headers)
                .await
                .unwrap_err();
            assert_eq!(error.status, 412);
        }
    }
}
