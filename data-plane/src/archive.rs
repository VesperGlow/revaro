use std::{
    collections::HashMap,
    env,
    fs::{self, OpenOptions},
    io::Write,
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex},
};

use axum::{
    Json,
    extract::{Path as AxumPath, State},
    http::StatusCode,
};
use libarchive2::{FileType, ReadArchive};
use serde::{Deserialize, Serialize};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    task,
};
use tokio_util::sync::CancellationToken;

use crate::{AppState, error::ApiError};

const MAX_ENTRIES: usize = 100_000;
const ABSOLUTE_EXPANDED_LIMIT: i64 = 64 << 30;

#[derive(Deserialize)]
pub struct ExtractQuery {
    key: String,
    job_id: String,
    archive_size: i64,
    password: Option<String>,
}

#[derive(Serialize)]
pub struct ExtractResponse {
    output_dir: String,
    entries: usize,
    expanded_bytes: i64,
}

struct CancelOnDrop(CancellationToken);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

#[derive(Clone, Default)]
pub struct ArchiveState {
    jobs: Arc<Mutex<HashMap<String, ArchiveJobHandle>>>,
}

#[derive(Clone)]
struct ArchiveJobHandle {
    cancel: CancellationToken,
    progress: Arc<Mutex<Progress>>,
}

#[derive(Clone, Default, Serialize)]
pub struct Progress {
    phase: String,
    entries: usize,
    expanded_bytes: i64,
    downloaded_bytes: i64,
}

impl ArchiveState {
    fn register(&self, job_id: &str, cancel: CancellationToken, progress: Arc<Mutex<Progress>>) {
        self.jobs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(job_id.to_string(), ArchiveJobHandle { cancel, progress });
    }

    fn remove(&self, job_id: &str) {
        self.jobs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(job_id);
    }

    fn get(&self, job_id: &str) -> Option<ArchiveJobHandle> {
        self.jobs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(job_id)
            .cloned()
    }
}

// JobRegistration removes the transient job entry when the extract request
// settles (success, error, or client disconnect).
struct JobRegistration {
    state: ArchiveState,
    job_id: String,
}
impl Drop for JobRegistration {
    fn drop(&mut self) {
        self.state.remove(&self.job_id);
    }
}

pub async fn cancel(
    State(state): State<AppState>,
    AxumPath(job_id): AxumPath<String>,
) -> Result<StatusCode, ApiError> {
    validate_job_id(&job_id)?;
    if let Some(handle) = state.archive.get(&job_id) {
        handle.cancel.cancel();
    }
    Ok(StatusCode::NO_CONTENT)
}

pub async fn progress(
    State(state): State<AppState>,
    AxumPath(job_id): AxumPath<String>,
) -> Result<Json<Progress>, ApiError> {
    validate_job_id(&job_id)?;
    let value = state
        .archive
        .get(&job_id)
        .map(|handle| {
            handle
                .progress
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone()
        })
        .unwrap_or_default();
    Ok(Json(value))
}

pub async fn extract(
    State(state): State<AppState>,
    Json(q): Json<ExtractQuery>,
) -> Result<Json<ExtractResponse>, ApiError> {
    validate_job_id(&q.job_id)?;
    if q.archive_size < 0 {
        return Err(ApiError::bad_request("invalid archive size"));
    }
    if q.password.as_ref().is_some_and(|p| p.len() > 1024) {
        return Err(ApiError::bad_request("archive password is too long"));
    }
    let work = PathBuf::from(env::var("APP_WORK_DIR").unwrap_or_else(|_| "/work".into()));
    let root = work.join(format!("revaro-extract-{}", q.job_id));
    let source = root.join("source.archive");
    let output = root.join("output");
    tokio::fs::create_dir_all(&root)
        .await
        .map_err(ApiError::internal)?;
    let _permit = state
        .archive_slots
        .clone()
        .acquire_owned()
        .await
        .map_err(ApiError::internal)?;
    let cancel = state.shutdown.child_token();
    let progress = Arc::new(Mutex::new(Progress::default()));
    state
        .archive
        .register(&q.job_id, cancel.clone(), progress.clone());
    let _registration = JobRegistration {
        state: state.archive.clone(),
        job_id: q.job_id.clone(),
    };
    let mut guard = CancelOnDrop(cancel.clone());
    if tokio::fs::metadata(&source).await.is_err() {
        progress
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .phase = "downloading".to_string();
        let object = state
            .s3
            .client
            .get_object()
            .bucket(&state.s3.bucket)
            .key(&q.key)
            .send()
            .await
            .map_err(ApiError::upstream)?;
        let mut reader = object.body.into_async_read();
        let partial = root.join("source.partial");
        let mut file = tokio::fs::File::create(&partial)
            .await
            .map_err(ApiError::internal)?;
        let mut copied = 0u64;
        let mut buffer = [0u8; 256 << 10];
        let download_result: Result<u64, ApiError> = async {
        loop {
            let n = tokio::select! {
                _ = cancel.cancelled() => return Err(ApiError::cancelled("archive download cancelled")),
                result = reader.read(&mut buffer) => result.map_err(ApiError::upstream)?,
            };
            if n == 0 {
                break;
            }
            tokio::select! {
                _ = cancel.cancelled() => return Err(ApiError::cancelled("archive download cancelled")),
                result = file.write_all(&buffer[..n]) => result.map_err(ApiError::internal)?,
            }
            copied += n as u64;
            progress
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .downloaded_bytes = copied as i64;
        }
        file.flush().await.map_err(ApiError::internal)?;
        Ok(copied)
        }.await;
        let copied = match download_result {
            Ok(value) => value,
            Err(error) => {
                let _ = tokio::fs::remove_file(&partial).await;
                return Err(error);
            }
        };
        if copied != q.archive_size as u64 {
            let _ = tokio::fs::remove_file(&partial).await;
            return Err(ApiError::upstream("archive source size changed"));
        }
        tokio::fs::rename(partial, &source)
            .await
            .map_err(ApiError::internal)?;
    }
    if tokio::fs::metadata(&output).await.is_ok() {
        tokio::fs::remove_dir_all(&output)
            .await
            .map_err(ApiError::internal)?;
    }
    tokio::fs::create_dir(&output)
        .await
        .map_err(ApiError::internal)?;

    progress
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .phase = "extracting".to_string();
    let password = q.password.unwrap_or_default();
    let password_empty = password.is_empty();
    let limit = expanded_limit(q.archive_size);
    let output_for_task = output.clone();
    let result = task::spawn_blocking(move || {
        extract_blocking(
            &source,
            &output_for_task,
            &password,
            limit,
            cancel,
            progress,
        )
    })
    .await
    .map_err(ApiError::internal)?;
    guard.0 = CancellationToken::new();
    match result {
        Ok((entries, expanded_bytes)) => Ok(Json(ExtractResponse {
            output_dir: output.to_string_lossy().into_owned(),
            entries,
            expanded_bytes,
        })),
        Err(error) => {
            let message = error.to_string();
            let lower = message.to_lowercase();
            if lower.contains("passphrase")
                || lower.contains("password")
                || lower.contains("encrypted")
            {
                let code = if password_empty {
                    "archive_password_required"
                } else {
                    "archive_wrong_password"
                };
                return Err(ApiError::conflict(code, message));
            }
            Err(ApiError::bad_request(message))
        }
    }
}

fn validate_job_id(id: &str) -> Result<(), ApiError> {
    if id.is_empty() || id.len() > 64 || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
        return Err(ApiError::bad_request("invalid archive job id"));
    }
    Ok(())
}

fn expanded_limit(size: i64) -> i64 {
    if size > 0 && size < ABSOLUTE_EXPANDED_LIMIT / 100 {
        (size * 100).max(4 << 30)
    } else {
        ABSOLUTE_EXPANDED_LIMIT
    }
}

fn safe_relative(raw: &str) -> Result<PathBuf, Box<dyn std::error::Error + Send + Sync>> {
    let path = Path::new(raw);
    let mut clean = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(name) if !name.is_empty() => clean.push(name),
            _ => return Err(format!("unsafe archive path: {raw:?}").into()),
        }
    }
    if clean.as_os_str().is_empty() {
        return Err("empty archive path".into());
    }
    Ok(clean)
}

// `drop(entry)` below ends the libarchive entry's borrow of the archive before
// `read_data` takes it mutably; the entry type itself has no Drop impl.
#[allow(clippy::drop_non_drop)]
fn extract_blocking(
    source: &Path,
    output: &Path,
    password: &str,
    limit: i64,
    cancel: CancellationToken,
    progress: Arc<Mutex<Progress>>,
) -> Result<(usize, i64), Box<dyn std::error::Error + Send + Sync>> {
    let mut archive = if password.is_empty() {
        ReadArchive::open(source)?
    } else {
        ReadArchive::open_with_passphrase(source, password)?
    };
    let mut count = 0usize;
    let mut total = 0i64;
    let mut buffer = vec![0u8; 256 << 10];
    while let Some(entry) = archive.next_entry()? {
        if cancel.is_cancelled() {
            return Err("archive extraction cancelled".into());
        }
        count += 1;
        if count > MAX_ENTRIES {
            return Err(format!("archive contains more than {MAX_ENTRIES} entries").into());
        }
        let raw = entry.pathname().ok_or("archive entry has no pathname")?;
        let relative = safe_relative(&raw)?;
        let linked = entry.symlink().is_some() || entry.hardlink().is_some();
        let kind = entry.file_type();
        let declared_size = entry.size();
        drop(entry);
        if linked {
            return Err("archive links are not allowed".into());
        }
        let target = output.join(relative);
        match kind {
            FileType::Directory => {
                fs::create_dir_all(&target)?;
                archive.skip_data()?;
            }
            FileType::RegularFile => {
                if declared_size < 0 || total.checked_add(declared_size).is_none_or(|n| n > limit) {
                    return Err("archive exceeds expanded-size limit".into());
                }
                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent)?;
                }
                let mut file = OpenOptions::new()
                    .create_new(true)
                    .write(true)
                    .open(&target)?;
                let mut written = 0i64;
                loop {
                    if cancel.is_cancelled() {
                        return Err("archive extraction cancelled".into());
                    }
                    let n = archive.read_data(&mut buffer)?;
                    if n == 0 {
                        break;
                    }
                    written = written
                        .checked_add(n as i64)
                        .ok_or("archive size overflow")?;
                    if total + written > limit {
                        return Err("archive exceeds expanded-size limit".into());
                    }
                    file.write_all(&buffer[..n])?;
                    {
                        let mut progress = progress
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                        progress.entries = count;
                        progress.expanded_bytes = total + written;
                    }
                }
                if declared_size != 0 && written != declared_size {
                    return Err(format!("archive entry size mismatch for {raw:?}").into());
                }
                total += written;
            }
            _ => return Err("archive contains unsupported special files".into()),
        }
    }
    if count == 0 {
        return Err("archive is empty".into());
    }
    Ok((count, total))
}
