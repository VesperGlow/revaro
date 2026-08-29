use std::{
    env,
    io::{self, Read, Seek, SeekFrom},
    pin::Pin,
    task::{Context, Poll},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use aws_config::BehaviorVersion;
use aws_credential_types::Credentials;
use aws_sdk_s3::{
    Client,
    config::{Builder as S3ConfigBuilder, Region},
    presigning::PresigningConfig,
    primitives::ByteStream,
    types::{CompletedMultipartUpload, CompletedPart, Delete, ObjectIdentifier},
};
use axum::{
    Json,
    body::Body,
    extract::{Query, Request, State},
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use http_body::{Body as HttpBody, Frame};
use http_body_util::BodyExt;
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::runtime::Handle;
use tokio::sync::mpsc;
use tokio_stream::StreamExt;
use tokio_util::io::ReaderStream;
use tokio_util::sync::CancellationToken;

use crate::{AppState, error::ApiError};

const MAX_DELETE_KEYS: usize = 1000;

#[derive(Clone)]
pub struct S3State {
    pub(crate) client: Client,
    presign: Client,
    pub(crate) bucket: String,
    pub(crate) slots: std::sync::Arc<tokio::sync::Semaphore>,
}

struct MultipartAbortGuard {
    client: Client,
    bucket: String,
    key: String,
    upload_id: String,
    armed: bool,
}

impl MultipartAbortGuard {
    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for MultipartAbortGuard {
    fn drop(&mut self) {
        if !self.armed || self.upload_id.is_empty() {
            return;
        }
        let (client, bucket, key, upload_id) = (
            self.client.clone(),
            self.bucket.clone(),
            self.key.clone(),
            self.upload_id.clone(),
        );
        if let Ok(runtime) = Handle::try_current() {
            runtime.spawn(async move {
                let _ = client
                    .abort_multipart_upload()
                    .bucket(bucket)
                    .key(key)
                    .upload_id(upload_id)
                    .send()
                    .await;
            });
        }
    }
}

#[derive(Deserialize)]
pub struct KeyQuery {
    key: String,
}

#[derive(Deserialize)]
pub struct RangeQuery {
    key: String,
    start: Option<u64>,
    end: Option<u64>,
}

#[derive(Deserialize)]
pub struct PutQuery {
    key: String,
    mime: Option<String>,
    size: Option<u64>,
    immutable: Option<bool>,
}

#[derive(Deserialize)]
pub struct PresignPutQuery {
    key: String,
    mime: Option<String>,
    expires_seconds: u64,
}

#[derive(Deserialize)]
pub struct PresignGetQuery {
    key: String,
    filename: String,
    mime: Option<String>,
    inline: Option<bool>,
    expires_seconds: u64,
}

#[derive(Deserialize)]
pub struct MultipartCreate {
    key: String,
    mime: Option<String>,
}

#[derive(Deserialize)]
pub struct MultipartRef {
    key: String,
    upload_id: String,
}

#[derive(Deserialize)]
pub struct PartQuery {
    key: String,
    upload_id: String,
    part_number: i32,
    expires_seconds: u64,
}

#[derive(Deserialize)]
pub struct UploadPartQuery {
    key: String,
    upload_id: String,
    part_number: i32,
    size: u64,
}

#[derive(Deserialize)]
pub struct CompleteRequest {
    key: String,
    upload_id: String,
    parts: Vec<Part>,
}

#[derive(Deserialize)]
pub struct Part {
    part_number: i32,
    etag: String,
}

#[derive(Deserialize)]
pub struct ListQuery {
    prefix: String,
    continuation: Option<String>,
}

#[derive(Deserialize)]
pub struct DeleteRequest {
    keys: Vec<String>,
}

#[derive(Serialize)]
pub struct ObjectInfo {
    pub(crate) size: i64,
    pub(crate) etag: String,
}

#[derive(Serialize)]
pub struct UrlResponse {
    url: String,
}

#[derive(Serialize)]
pub struct UploadResponse {
    upload_id: String,
}

#[derive(Serialize)]
pub struct ObjectRef {
    key: String,
    size: i64,
    last_modified_unix_ms: i64,
}

#[derive(Serialize)]
pub struct ListResponse {
    objects: Vec<ObjectRef>,
    continuation: Option<String>,
}

impl S3State {
    pub async fn from_env() -> Result<Self, Box<dyn std::error::Error>> {
        let region = env::var("S3_REGION").unwrap_or_else(|_| "us-east-1".into());
        let access = env::var("S3_ACCESS_KEY")?;
        let secret = env::var("S3_SECRET_KEY")?;
        let bucket = env::var("S3_BUCKET")?;
        let endpoint = env::var("S3_ENDPOINT").ok().filter(|v| !v.is_empty());
        let public_endpoint = env::var("S3_PUBLIC_ENDPOINT")
            .ok()
            .filter(|v| !v.is_empty())
            .or_else(|| endpoint.clone());
        let path_style = env::var("S3_PATH_STYLE")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(false);
        let credentials = Credentials::new(access, secret, None, None, "revaro-static");
        let shared = aws_config::defaults(BehaviorVersion::latest())
            .region(Region::new(region.clone()))
            .credentials_provider(credentials.clone())
            .load()
            .await;
        let build = |endpoint: Option<&String>| {
            let mut b = S3ConfigBuilder::from(&shared)
                .region(Region::new(region.clone()))
                .credentials_provider(credentials.clone())
                .force_path_style(path_style);
            if let Some(endpoint) = endpoint {
                b = b.endpoint_url(endpoint);
            }
            b.build()
        };
        Ok(Self {
            client: Client::from_conf(build(endpoint.as_ref())),
            presign: Client::from_conf(build(public_endpoint.as_ref())),
            bucket,
            slots: std::sync::Arc::new(tokio::sync::Semaphore::new(4)),
        })
    }

    pub async fn ping(&self) -> Result<(), ApiError> {
        self.client
            .head_bucket()
            .bucket(&self.bucket)
            .send()
            .await
            .map_err(ApiError::upstream)?;
        Ok(())
    }

    pub async fn abort_stale_multipart_uploads(&self, max_age: Duration) -> Result<u64, ApiError> {
        let cutoff = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64
            - max_age.as_secs() as i64;
        let mut key_marker = None;
        let mut upload_marker = None;
        let mut aborted = 0;
        loop {
            let result = self
                .client
                .list_multipart_uploads()
                .bucket(&self.bucket)
                .set_key_marker(key_marker)
                .set_upload_id_marker(upload_marker)
                .send()
                .await
                .map_err(ApiError::upstream)?;
            for upload in result.uploads() {
                if upload.initiated().is_some_and(|time| time.secs() <= cutoff)
                    && let (Some(key), Some(upload_id)) = (upload.key(), upload.upload_id())
                {
                    self.client
                        .abort_multipart_upload()
                        .bucket(&self.bucket)
                        .key(key)
                        .upload_id(upload_id)
                        .send()
                        .await
                        .map_err(ApiError::upstream)?;
                    aborted += 1;
                }
            }
            if !result.is_truncated().unwrap_or(false) {
                break;
            }
            key_marker = result.next_key_marker().map(ToOwned::to_owned);
            upload_marker = result.next_upload_id_marker().map(ToOwned::to_owned);
        }
        Ok(aborted)
    }

    pub async fn range_reader(
        &self,
        key: String,
        cancel: CancellationToken,
    ) -> Result<S3RangeReader, ApiError> {
        let head = self
            .client
            .head_object()
            .bucket(&self.bucket)
            .key(&key)
            .send()
            .await
            .map_err(ApiError::upstream)?;
        Ok(S3RangeReader {
            client: self.client.clone(),
            bucket: self.bucket.clone(),
            key,
            size: head.content_length().unwrap_or_default(),
            position: 0,
            window: bytes::Bytes::new(),
            window_start: -1,
            runtime: Handle::current(),
            cancel,
        })
    }

    pub async fn store_reader<R>(
        &self,
        key: &str,
        mime: Option<&str>,
        mut reader: R,
        expected_size: Option<u64>,
    ) -> Result<ObjectInfo, ApiError>
    where
        R: AsyncRead + Unpin,
    {
        let _permit = self
            .slots
            .acquire()
            .await
            .map_err(|_| ApiError::cancelled("S3 transfer shutdown"))?;
        const PART: usize = 16 << 20;
        if expected_size == Some(0) {
            let mut put = self
                .client
                .put_object()
                .bucket(&self.bucket)
                .key(key)
                .content_length(0)
                .body(ByteStream::from_static(&[]));
            if let Some(value) = mime {
                put = put.content_type(value);
            }
            let out = put.send().await.map_err(ApiError::upstream)?;
            return Ok(ObjectInfo {
                size: 0,
                etag: clean_etag(out.e_tag()),
            });
        }
        let mut create = self
            .client
            .create_multipart_upload()
            .bucket(&self.bucket)
            .key(key);
        if let Some(value) = mime {
            create = create.content_type(value);
        }
        let created = create.send().await.map_err(ApiError::upstream)?;
        let upload_id = created.upload_id().unwrap_or_default().to_string();
        let mut abort_guard = MultipartAbortGuard {
            client: self.client.clone(),
            bucket: self.bucket.clone(),
            key: key.to_owned(),
            upload_id: upload_id.clone(),
            armed: true,
        };
        let result: Result<u64, ApiError> = async {
            let mut parts = Vec::new();
            let mut total = 0u64;
            loop {
                let mut pending = bytes::BytesMut::zeroed(PART);
                let mut filled = 0usize;
                while filled < PART {
                    let read = reader
                        .read(&mut pending[filled..])
                        .await
                        .map_err(ApiError::upstream)?;
                    if read == 0 {
                        break;
                    }
                    filled += read;
                }
                if filled == 0 {
                    break;
                }
                pending.truncate(filled);
                total += upload_buffer(self, key, &upload_id, &mut parts, pending.freeze()).await?;
                if filled < PART {
                    break;
                }
            }
            if expected_size.is_some_and(|size| size != total) {
                return Err(ApiError::bad_request(format!(
                    "blob stream size {total}, expected {}",
                    expected_size.unwrap()
                )));
            }
            if parts.is_empty() {
                return Ok(0);
            }
            self.client
                .complete_multipart_upload()
                .bucket(&self.bucket)
                .key(key)
                .upload_id(&upload_id)
                .multipart_upload(
                    CompletedMultipartUpload::builder()
                        .set_parts(Some(parts))
                        .build(),
                )
                .send()
                .await
                .map_err(ApiError::upstream)?;
            Ok(total)
        }
        .await;
        let total = match result {
            Ok(total) => total,
            Err(error) => {
                let _ = self
                    .client
                    .abort_multipart_upload()
                    .bucket(&self.bucket)
                    .key(key)
                    .upload_id(&upload_id)
                    .send()
                    .await;
                return Err(error);
            }
        };
        if total == 0 {
            let _ = self
                .client
                .abort_multipart_upload()
                .bucket(&self.bucket)
                .key(key)
                .upload_id(&upload_id)
                .send()
                .await;
            let mut put = self
                .client
                .put_object()
                .bucket(&self.bucket)
                .key(key)
                .content_length(0)
                .body(ByteStream::from_static(&[]));
            if let Some(value) = mime {
                put = put.content_type(value);
            }
            let out = put.send().await.map_err(ApiError::upstream)?;
            return Ok(ObjectInfo {
                size: 0,
                etag: clean_etag(out.e_tag()),
            });
        }
        abort_guard.disarm();
        let head = self
            .client
            .head_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
            .map_err(ApiError::upstream)?;
        Ok(ObjectInfo {
            size: total as i64,
            etag: clean_etag(head.e_tag()),
        })
    }
}

pub struct S3RangeReader {
    client: Client,
    bucket: String,
    key: String,
    size: i64,
    position: i64,
    window: bytes::Bytes,
    window_start: i64,
    runtime: Handle,
    cancel: CancellationToken,
}

impl Read for S3RangeReader {
    fn read(&mut self, target: &mut [u8]) -> io::Result<usize> {
        if self.cancel.is_cancelled() {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "S3 Range read cancelled",
            ));
        }
        if target.is_empty() || self.position >= self.size {
            return Ok(0);
        }
        let cached_end = self.window_start + self.window.len() as i64;
        if self.position < self.window_start || self.position >= cached_end {
            let fetch = target.len().clamp(1 << 20, 4 << 20) as i64;
            let start = self.position;
            let end = (start + fetch - 1).min(self.size - 1);
            let client = self.client.clone();
            let bucket = self.bucket.clone();
            let key = self.key.clone();
            let cancel = self.cancel.clone();
            let bytes = self.runtime.block_on(async move {
                tokio::select! {
                    _ = cancel.cancelled() => Err(io::Error::new(io::ErrorKind::Interrupted, "S3 Range read cancelled")),
                    result = async {
                        let out = client.get_object().bucket(bucket).key(key).range(format!("bytes={start}-{end}")).send().await.map_err(io::Error::other)?;
                        out.body.collect().await.map(|v| v.into_bytes()).map_err(io::Error::other)
                    } => result,
                }
            })?;
            self.window = bytes;
            self.window_start = start;
        }
        let offset = (self.position - self.window_start) as usize;
        let count = target.len().min(self.window.len().saturating_sub(offset));
        target[..count].copy_from_slice(&self.window[offset..offset + count]);
        self.position += count as i64;
        Ok(count)
    }
}

impl Seek for S3RangeReader {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        if self.cancel.is_cancelled() {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "S3 seek cancelled",
            ));
        }
        let next = match from {
            SeekFrom::Start(v) => Some(i64::try_from(v).map_err(io::Error::other)?),
            SeekFrom::Current(v) => self.position.checked_add(v),
            SeekFrom::End(v) => self.size.checked_add(v),
        }
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "seek overflow"))?;
        if next < 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "negative seek"));
        }
        self.position = next;
        Ok(next as u64)
    }
}

fn s3(state: &AppState) -> &S3State {
    &state.s3
}

fn expiry(seconds: u64) -> Result<PresigningConfig, ApiError> {
    if seconds == 0 || seconds > 7 * 24 * 3600 {
        return Err(ApiError::bad_request("invalid presign expiry"));
    }
    PresigningConfig::expires_in(Duration::from_secs(seconds))
        .map_err(|e| ApiError::bad_request(e.to_string()))
}

fn clean_etag(value: Option<&str>) -> String {
    value.unwrap_or_default().trim_matches('"').to_string()
}

pub async fn ping(State(state): State<AppState>) -> Result<StatusCode, ApiError> {
    s3(&state).ping().await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn head(
    State(state): State<AppState>,
    Query(q): Query<KeyQuery>,
) -> Result<Json<ObjectInfo>, ApiError> {
    let out = s3(&state)
        .client
        .head_object()
        .bucket(&s3(&state).bucket)
        .key(q.key)
        .send()
        .await
        .map_err(ApiError::upstream)?;
    Ok(Json(ObjectInfo {
        size: out.content_length().unwrap_or_default(),
        etag: clean_etag(out.e_tag()),
    }))
}

pub async fn get_range(
    State(state): State<AppState>,
    Query(q): Query<RangeQuery>,
) -> Result<Response, ApiError> {
    if q.end.is_some_and(|end| end < q.start.unwrap_or(0)) {
        return Err(ApiError::bad_request("invalid byte range"));
    }
    let mut request = s3(&state)
        .client
        .get_object()
        .bucket(&s3(&state).bucket)
        .key(q.key);
    if let Some(start) = q.start {
        request = request.range(match q.end {
            Some(end) => format!("bytes={start}-{end}"),
            None => format!("bytes={start}-"),
        });
    }
    let out = request.send().await.map_err(ApiError::upstream)?;
    let length = out.content_length();
    let content_range = out.content_range().map(str::to_string);
    let etag = out.e_tag().map(str::to_string);
    let mut response =
        Body::from_stream(ReaderStream::new(out.body.into_async_read())).into_response();
    *response.status_mut() = if q.start.is_some() {
        StatusCode::PARTIAL_CONTENT
    } else {
        StatusCode::OK
    };
    if let Some(length) = length {
        response.headers_mut().insert(
            header::CONTENT_LENGTH,
            HeaderValue::from_str(&length.to_string()).unwrap(),
        );
    }
    if let Some(range) = content_range
        .as_deref()
        .and_then(|v| HeaderValue::from_str(v).ok())
    {
        response.headers_mut().insert(header::CONTENT_RANGE, range);
    }
    if let Some(etag) = etag.as_deref().and_then(|v| HeaderValue::from_str(v).ok()) {
        response.headers_mut().insert(header::ETAG, etag);
    }
    Ok(response)
}

pub async fn put_stream(
    State(state): State<AppState>,
    Query(q): Query<PutQuery>,
    request: Request,
) -> Result<Json<ObjectInfo>, ApiError> {
    let size = q
        .size
        .ok_or_else(|| ApiError::bad_request("size is required"))?;
    let body = request_body_stream(request);
    let mut put = s3(&state)
        .client
        .put_object()
        .bucket(&s3(&state).bucket)
        .key(&q.key)
        .content_length(size as i64)
        .body(body);
    if let Some(mime) = q.mime {
        put = put.content_type(mime);
    }
    if q.immutable.unwrap_or(false) {
        put = put.if_none_match("*");
    }
    let out = put.send().await.map_err(ApiError::upstream)?;
    Ok(Json(ObjectInfo {
        size: size as i64,
        etag: clean_etag(out.e_tag()),
    }))
}

pub async fn put_blob(
    State(state): State<AppState>,
    Query(q): Query<PutQuery>,
    request: Request,
) -> Result<Json<ObjectInfo>, ApiError> {
    if q.size.is_some_and(|size| size < 8 << 20) {
        return put_stream(State(state), Query(q), request).await;
    }
    let reader = tokio_util::io::StreamReader::new(
        request
            .into_body()
            .into_data_stream()
            .map(|result| result.map_err(io::Error::other)),
    );
    Ok(Json(
        state
            .s3
            .store_reader(&q.key, q.mime.as_deref(), reader, q.size)
            .await?,
    ))
}

async fn upload_buffer(
    state: &S3State,
    key: &str,
    upload_id: &str,
    parts: &mut Vec<CompletedPart>,
    data: bytes::Bytes,
) -> Result<u64, ApiError> {
    if parts.len() >= 10_000 {
        return Err(ApiError::payload_too_large(
            "S3 multipart part limit exceeded",
        ));
    }
    let number = parts.len() as i32 + 1;
    let size = data.len() as i64;
    let out = state
        .client
        .upload_part()
        .bucket(&state.bucket)
        .key(key)
        .upload_id(upload_id)
        .part_number(number)
        .content_length(size)
        .body(ByteStream::from(data))
        .send()
        .await
        .map_err(ApiError::upstream)?;
    parts.push(
        CompletedPart::builder()
            .part_number(number)
            .e_tag(clean_etag(out.e_tag()))
            .build(),
    );
    Ok(size as u64)
}

pub async fn delete_one(
    State(state): State<AppState>,
    Query(q): Query<KeyQuery>,
) -> Result<StatusCode, ApiError> {
    s3(&state)
        .client
        .delete_object()
        .bucket(&s3(&state).bucket)
        .key(q.key)
        .send()
        .await
        .map_err(ApiError::upstream)?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn delete_many(
    State(state): State<AppState>,
    Json(q): Json<DeleteRequest>,
) -> Result<StatusCode, ApiError> {
    if q.keys.len() > MAX_DELETE_KEYS {
        return Err(ApiError::payload_too_large(
            "delete batch exceeds 1000 keys",
        ));
    }
    if q.keys.is_empty() {
        return Ok(StatusCode::NO_CONTENT);
    }
    let objects = q
        .keys
        .into_iter()
        .map(|key| ObjectIdentifier::builder().key(key).build())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    let delete = Delete::builder()
        .set_objects(Some(objects))
        .quiet(true)
        .build()
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    let out = s3(&state)
        .client
        .delete_objects()
        .bucket(&s3(&state).bucket)
        .delete(delete)
        .send()
        .await
        .map_err(ApiError::upstream)?;
    if let Some(error) = out.errors().first() {
        return Err(ApiError::upstream(format!(
            "{}: {}",
            error.key().unwrap_or_default(),
            error.message().unwrap_or_default()
        )));
    }
    Ok(StatusCode::NO_CONTENT)
}

pub async fn list(
    State(state): State<AppState>,
    Query(q): Query<ListQuery>,
) -> Result<Json<ListResponse>, ApiError> {
    let out = s3(&state)
        .client
        .list_objects_v2()
        .bucket(&s3(&state).bucket)
        .prefix(q.prefix)
        .set_continuation_token(q.continuation)
        .send()
        .await
        .map_err(ApiError::upstream)?;
    let objects = out
        .contents()
        .iter()
        .map(|o| ObjectRef {
            key: o.key().unwrap_or_default().to_string(),
            size: o.size().unwrap_or_default(),
            last_modified_unix_ms: o
                .last_modified()
                .map(|d| d.to_millis().unwrap_or_default())
                .unwrap_or_default(),
        })
        .collect();
    Ok(Json(ListResponse {
        objects,
        continuation: out.next_continuation_token().map(str::to_string),
    }))
}

pub async fn presign_put(
    State(state): State<AppState>,
    Query(q): Query<PresignPutQuery>,
) -> Result<Json<UrlResponse>, ApiError> {
    let mut request = s3(&state)
        .presign
        .put_object()
        .bucket(&s3(&state).bucket)
        .key(q.key);
    if let Some(mime) = q.mime {
        request = request.content_type(mime);
    }
    let out = request
        .presigned(expiry(q.expires_seconds)?)
        .await
        .map_err(ApiError::upstream)?;
    Ok(Json(UrlResponse {
        url: out.uri().to_string(),
    }))
}

pub async fn presign_get(
    State(state): State<AppState>,
    Query(q): Query<PresignGetQuery>,
) -> Result<Json<UrlResponse>, ApiError> {
    let disposition = format!(
        "{}; filename*=UTF-8''{}",
        if q.inline.unwrap_or(false) {
            "inline"
        } else {
            "attachment"
        },
        utf8_percent_encode(&q.filename, NON_ALPHANUMERIC)
    );
    let mut request = s3(&state)
        .presign
        .get_object()
        .bucket(&s3(&state).bucket)
        .key(q.key)
        .response_content_disposition(disposition);
    if let Some(mime) = q.mime {
        request = request.response_content_type(mime);
    }
    let out = request
        .presigned(expiry(q.expires_seconds)?)
        .await
        .map_err(ApiError::upstream)?;
    Ok(Json(UrlResponse {
        url: out.uri().to_string(),
    }))
}

pub async fn multipart_create(
    State(state): State<AppState>,
    Json(q): Json<MultipartCreate>,
) -> Result<Json<UploadResponse>, ApiError> {
    let mut request = s3(&state)
        .client
        .create_multipart_upload()
        .bucket(&s3(&state).bucket)
        .key(q.key);
    if let Some(mime) = q.mime {
        request = request.content_type(mime);
    }
    let out = request.send().await.map_err(ApiError::upstream)?;
    Ok(Json(UploadResponse {
        upload_id: out.upload_id().unwrap_or_default().to_string(),
    }))
}

pub async fn multipart_part(
    State(state): State<AppState>,
    Query(q): Query<PartQuery>,
) -> Result<Json<UrlResponse>, ApiError> {
    if !(1..=10000).contains(&q.part_number) {
        return Err(ApiError::bad_request(
            "multipart part number is out of range",
        ));
    }
    let out = s3(&state)
        .presign
        .upload_part()
        .bucket(&s3(&state).bucket)
        .key(q.key)
        .upload_id(q.upload_id)
        .part_number(q.part_number)
        .presigned(expiry(q.expires_seconds)?)
        .await
        .map_err(ApiError::upstream)?;
    Ok(Json(UrlResponse {
        url: out.uri().to_string(),
    }))
}

pub async fn multipart_upload(
    State(state): State<AppState>,
    Query(q): Query<UploadPartQuery>,
    request: Request,
) -> Result<Json<ObjectInfo>, ApiError> {
    if !(1..=10000).contains(&q.part_number) || q.size == 0 {
        return Err(ApiError::bad_request("invalid multipart part"));
    }
    let _permit = s3(&state)
        .slots
        .acquire()
        .await
        .map_err(|_| ApiError::cancelled("S3 transfer shutdown"))?;
    let body = request_body_stream(request);
    let out = s3(&state)
        .client
        .upload_part()
        .bucket(&s3(&state).bucket)
        .key(q.key)
        .upload_id(q.upload_id)
        .part_number(q.part_number)
        .content_length(q.size as i64)
        .body(body)
        .send()
        .await
        .map_err(ApiError::upstream)?;
    Ok(Json(ObjectInfo {
        size: q.size as i64,
        etag: clean_etag(out.e_tag()),
    }))
}

pub async fn multipart_abort(
    State(state): State<AppState>,
    Json(q): Json<MultipartRef>,
) -> Result<StatusCode, ApiError> {
    if !q.upload_id.is_empty() {
        s3(&state)
            .client
            .abort_multipart_upload()
            .bucket(&s3(&state).bucket)
            .key(q.key)
            .upload_id(q.upload_id)
            .send()
            .await
            .map_err(ApiError::upstream)?;
    }
    Ok(StatusCode::NO_CONTENT)
}

pub async fn multipart_complete(
    State(state): State<AppState>,
    Json(q): Json<CompleteRequest>,
) -> Result<Json<ObjectInfo>, ApiError> {
    if q.parts.is_empty() || q.parts.len() > 10000 {
        return Err(ApiError::bad_request("invalid multipart part count"));
    }
    let parts = q
        .parts
        .into_iter()
        .map(|p| {
            CompletedPart::builder()
                .part_number(p.part_number)
                .e_tag(p.etag)
                .build()
        })
        .collect();
    let upload = CompletedMultipartUpload::builder()
        .set_parts(Some(parts))
        .build();
    s3(&state)
        .client
        .complete_multipart_upload()
        .bucket(&s3(&state).bucket)
        .key(&q.key)
        .upload_id(q.upload_id)
        .multipart_upload(upload)
        .send()
        .await
        .map_err(ApiError::upstream)?;
    head(State(state), Query(KeyQuery { key: q.key })).await
}

struct ChannelBody {
    rx: mpsc::Receiver<Result<bytes::Bytes, std::io::Error>>,
}

fn request_body_stream(request: Request) -> ByteStream {
    let (tx, rx) = mpsc::channel(2);
    tokio::spawn(async move {
        let mut body = request.into_body();
        while let Some(frame) = body.frame().await {
            match frame {
                Ok(frame) => {
                    if let Ok(data) = frame.into_data()
                        && tx.send(Ok(data)).await.is_err()
                    {
                        break;
                    }
                }
                Err(error) => {
                    let _ = tx.send(Err(std::io::Error::other(error))).await;
                    break;
                }
            }
        }
    });
    ByteStream::from_body_1_x(ChannelBody { rx })
}

impl HttpBody for ChannelBody {
    type Data = bytes::Bytes;
    type Error = std::io::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        match self.rx.poll_recv(cx) {
            Poll::Ready(Some(Ok(bytes))) => Poll::Ready(Some(Ok(Frame::data(bytes)))),
            Poll::Ready(Some(Err(error))) => Poll::Ready(Some(Err(error))),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }

    fn size_hint(&self) -> http_body::SizeHint {
        http_body::SizeHint::default()
    }
}
