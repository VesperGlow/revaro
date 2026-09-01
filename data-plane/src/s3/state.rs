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
                let abort = client
                    .abort_multipart_upload()
                    .bucket(bucket)
                    .key(key)
                    .upload_id(upload_id)
                    .send();
                let _ = tokio::time::timeout(Duration::from_secs(30), abort).await;
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


