//! Archive extraction routes and the durable task adapter.
//!
//! `revaro-media` owns format decoding and bounded writes to a scratch
//! directory. This module owns authenticated file lookup, task persistence,
//! password prompts, object-store import and restart recovery. Keeping those
//! responsibilities separate makes it possible to test the dangerous archive
//! loop without a database while still exercising the complete HTTP flow here.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll};

use axum::extract::{Path as PathParam, State};
use axum::routing::post;
use axum::{Json, Router};
use revaro_core::ApiError;
use revaro_core::api::archive::{Job as ArchiveJob, JobStatus};
use revaro_core::api::tasks::TaskInputRequest;
use revaro_core::classify::{self, ARCHIVE_SUFFIXES};
use revaro_core::ids::ROOT_ID;
use revaro_core::keys;
use revaro_core::model::{File, Task, TaskStatus};
use revaro_core::time::Timestamp;
use revaro_core::validate::validate_name;
use revaro_media::{
    ArchiveError, ArchivePhase, ArchiveProgress, ArchiveResult, MAX_ARCHIVE_ENTRIES,
    MAX_ARCHIVE_PASSWORD_BYTES, expanded_limit,
};
use rusqlite::{Connection, OptionalExtension, Row};
use serde::Deserialize;
use sha2::{Digest as _, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt as _, ReadBuf};
use tokio_util::sync::CancellationToken;

use crate::auth::extract::AuthUser;
use crate::auth_routes::JsonBody;
use crate::db::DbError;
use crate::file_routes;
use crate::state::AppState;

const CONTENT_HASH_ALGORITHM: &str = "sha256";
const ARCHIVE_TASK_TYPE: &str = "archive_extract";

/// Routes mounted below the authenticated `/api` subtree.
pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/files/{id}/extract", post(extract_archive))
        .route("/tasks/{id}/input", post(task_input))
}

/// Start an archive extraction from a ready archive file.
async fn extract_archive(
    State(state): State<Arc<AppState>>,
    _user: AuthUser,
    PathParam(id): PathParam<String>,
) -> Result<(http::StatusCode, Json<ArchiveJob>), ApiError> {
    let file = load_archive_file(Arc::clone(&state), id).await?;
    let parent_id = file.parent_id.clone().unwrap_or_else(|| ROOT_ID.to_owned());
    let job = create_archive_job(&state, &file, parent_id).await?;
    let handle = state.archive.register(job.clone());
    spawn_attempt(Arc::clone(&state), handle, file, None);
    Ok((http::StatusCode::ACCEPTED, Json(job)))
}

/// Supply a password to an archive task waiting for input.
async fn task_input(
    State(state): State<Arc<AppState>>,
    _user: AuthUser,
    PathParam(id): PathParam<String>,
    JsonBody(request): JsonBody<TaskInputRequest>,
) -> Result<(http::StatusCode, Json<ArchiveJob>), ApiError> {
    let task = find_task(&state, &id)
        .await?
        .filter(|task| task.status == TaskStatus::WaitingInput)
        .ok_or_else(|| ApiError::conflict("task is not waiting for input"))?;
    if task.source_type != "archive" {
        return Err(ApiError::bad_request("task does not accept input"));
    }
    if request.password.is_empty() || request.password.len() > MAX_ARCHIVE_PASSWORD_BYTES {
        return Err(ApiError::bad_request("archive password is required"));
    }

    let handle = state
        .archive
        .resume_for_password(&task.source_id)
        .ok_or_else(|| ApiError::conflict("archive task cannot continue"))?;
    let file = match load_archive_file(Arc::clone(&state), handle.file_id()).await {
        Ok(file) => file,
        Err(error) => {
            let message = "archive source is unavailable";
            handle.update(
                JobStatus::Failed,
                handle.snapshot().progress,
                message.to_owned(),
                message.to_owned(),
            );
            persist_snapshot(&state, &handle.snapshot()).await;
            state.archive.remove_if(&task.source_id, &handle);
            cleanup_output(&state, &task.source_id).await;
            tracing::warn!(%error, task = %id, "archive source disappeared while awaiting password");
            return Err(ApiError::conflict(message));
        }
    };
    spawn_attempt(
        Arc::clone(&state),
        Arc::clone(&handle),
        file,
        Some(request.password),
    );
    Ok((http::StatusCode::ACCEPTED, Json(handle.snapshot())))
}

/// Load a file that can be used as an archive source.
async fn load_archive_file(state: Arc<AppState>, id: String) -> Result<File, ApiError> {
    state
        .db
        .call_api(move |connection| {
            let file = file_routes::lookup_file_any(connection, &id)
                .map_err(|error| not_found_or(error, "ready archive file not found"))?;
            if !classify::is_archive(&file) {
                return Err(ApiError::not_found("ready archive file not found"));
            }
            Ok(file)
        })
        .await
}

async fn create_archive_job(
    state: &Arc<AppState>,
    file: &File,
    parent_id: String,
) -> Result<ArchiveJob, ApiError> {
    let id = crate::ids::new_id();
    let now = Timestamp::now();
    let job = ArchiveJob {
        id: id.clone(),
        file_id: file.id.clone(),
        parent_id: parent_id.clone(),
        name: file.name.clone(),
        status: JobStatus::Queued,
        progress: 0,
        message: "等待解压".to_owned(),
        output_id: String::new(),
        output_name: String::new(),
        error: String::new(),
        created_at: now,
        updated_at: now,
    };
    let payload = serde_json::json!({
        "file_id": file.id,
        "parent_id": parent_id,
    });
    let payload = serde_json::to_string(&payload)
        .map_err(|_| ApiError::internal("could not persist archive task"))?;
    let file_id = file.id.clone();
    state
        .db
        .call_api(move |connection| {
            let transaction = connection
                .transaction()
                .map_err(|error| database_error(DbError::Query(error)))?;
            transaction
                .execute(
                    "INSERT INTO tasks(id,type,status,phase,progress,error,source_type,source_id,payload_json,cancel_requested,created_at,updated_at) \
                     VALUES(?1,?2,'queued','queued',0,'','archive',?1,?3,0,?4,?4)",
                    rusqlite::params![id, ARCHIVE_TASK_TYPE, payload, now.to_rfc3339()],
                )
                .map_err(|error| database_error(DbError::Query(error)))?;
            transaction
                .execute(
                    "INSERT INTO task_files(task_id,file_id,role) VALUES(?1,?2,'input')",
                    rusqlite::params![id, file_id],
                )
                .map_err(|error| database_error(DbError::Query(error)))?;
            transaction
                .commit()
                .map_err(|error| database_error(DbError::Query(error)))?;
            Ok(())
        })
        .await?;
    Ok(job)
}

#[derive(Debug, Deserialize)]
struct ArchiveTaskPayload {
    file_id: String,
    parent_id: String,
}

#[derive(Debug, Clone)]
struct ArchiveTaskRecord {
    id: String,
    status: TaskStatus,
    phase: String,
    progress: f64,
    error: String,
    source_type: String,
    source_id: String,
    payload_json: String,
    created_at: String,
    updated_at: String,
}

async fn find_task(state: &Arc<AppState>, id: &str) -> Result<Option<ArchiveTaskRecord>, ApiError> {
    let id = id.to_owned();
    state
        .db
        .call_api(move |connection| {
            connection
                .query_row(
                    "SELECT id,status,phase,progress,error,source_type,source_id,payload_json,created_at,updated_at \
                     FROM tasks WHERE id = ?1",
                    [&id],
                    task_record,
                )
                .optional()
                .map_err(|error| database_error(DbError::Query(error)))
        })
        .await
}

async fn active_archive_tasks(state: &Arc<AppState>) -> Result<Vec<ArchiveTaskRecord>, ApiError> {
    state
        .db
        .call_api(|connection| {
            let mut statement = connection
                .prepare(
                    "SELECT id,status,phase,progress,error,source_type,source_id,payload_json,created_at,updated_at \
                     FROM tasks WHERE type = 'archive_extract' \
                     AND status IN ('queued','running','retrying','waiting_input') \
                     ORDER BY created_at",
                )
                .map_err(|error| database_error(DbError::Query(error)))?;
            let rows = statement
                .query_map([], task_record)
                .map_err(|error| database_error(DbError::Query(error)))?;
            let mut records = Vec::new();
            for row in rows {
                records.push(row.map_err(|error| database_error(DbError::Query(error)))?);
            }
            Ok(records)
        })
        .await
}

fn task_record(row: &Row<'_>) -> rusqlite::Result<ArchiveTaskRecord> {
    let status_raw: String = row.get(1)?;
    let status = status_raw.parse().map_err(|_| {
        rusqlite::Error::InvalidColumnType(
            1,
            format!("status={status_raw:?}"),
            rusqlite::types::Type::Text,
        )
    })?;
    Ok(ArchiveTaskRecord {
        id: row.get(0)?,
        status,
        phase: row.get(2)?,
        progress: row.get(3)?,
        error: row.get(4)?,
        source_type: row.get::<_, Option<String>>(5)?.unwrap_or_default(),
        source_id: row.get::<_, Option<String>>(6)?.unwrap_or_default(),
        payload_json: row.get(7)?,
        created_at: row.get(8)?,
        updated_at: row.get(9)?,
    })
}

fn parse_payload(record: &ArchiveTaskRecord) -> Result<ArchiveTaskPayload, String> {
    serde_json::from_str(&record.payload_json)
        .map_err(|_| "archive task payload is invalid".to_owned())
}

fn recovered_job(
    record: &ArchiveTaskRecord,
    payload: &ArchiveTaskPayload,
    file: &File,
) -> ArchiveJob {
    let status = if record.status == TaskStatus::WaitingInput {
        JobStatus::WaitingPassword
    } else {
        JobStatus::Queued
    };
    let progress = if record.progress.is_finite() {
        (record.progress.round() as i32).clamp(0, 100)
    } else {
        0
    };
    ArchiveJob {
        id: record.id.clone(),
        file_id: payload.file_id.clone(),
        parent_id: payload.parent_id.clone(),
        name: file.name.clone(),
        status,
        progress,
        message: if status == JobStatus::WaitingPassword {
            "压缩包已加密，请重新输入密码".to_owned()
        } else {
            match record.phase.as_str() {
                "extracting" => "服务重启后继续解压".to_owned(),
                "importing" => "服务重启后继续写入网盘".to_owned(),
                _ => "服务重启后恢复".to_owned(),
            }
        },
        output_id: String::new(),
        output_name: String::new(),
        error: if status == JobStatus::WaitingPassword {
            "压缩包已加密，请重新输入密码".to_owned()
        } else {
            record.error.clone()
        },
        created_at: Timestamp::parse(&record.created_at).unwrap_or_else(|_| Timestamp::now()),
        updated_at: Timestamp::parse(&record.updated_at).unwrap_or_else(|_| Timestamp::now()),
    }
}

/// Recover active archive tasks after the database and object store are ready.
pub async fn recover(state: Arc<AppState>) {
    let records = match active_archive_tasks(&state).await {
        Ok(records) => records,
        Err(error) => {
            tracing::error!(%error, "could not recover archive tasks");
            return;
        }
    };
    for record in records {
        if record.source_type != "archive" || record.source_id != record.id {
            fail_recovered_task(&state, &record.id, "archive task source is invalid").await;
            continue;
        }
        let payload = match parse_payload(&record) {
            Ok(payload) if !payload.file_id.is_empty() && !payload.parent_id.is_empty() => payload,
            _ => {
                fail_recovered_task(&state, &record.id, "archive task payload is invalid").await;
                continue;
            }
        };
        let file = match load_archive_file(Arc::clone(&state), payload.file_id.clone()).await {
            Ok(file) => file,
            Err(error) => {
                tracing::warn!(%error, task = %record.id, "archive source unavailable during recovery");
                fail_recovered_task(&state, &record.id, "archive source is unavailable").await;
                continue;
            }
        };
        let handle = state
            .archive
            .register(recovered_job(&record, &payload, &file));
        if record.status == TaskStatus::WaitingInput {
            handle.wait_for_password("压缩包已加密，请重新输入密码".to_owned());
            persist_snapshot(&state, &handle.snapshot()).await;
            schedule_password_expiry(Arc::clone(&state), record.id.clone());
        } else {
            spawn_attempt(Arc::clone(&state), handle, file, None);
        }
    }
}

/// Cancel the in-memory archive attempt associated with a task.
pub async fn cancel_task_runtime(state: &Arc<AppState>, task_id: &str) {
    let Some(task) = find_task(state, task_id).await.ok().flatten() else {
        return;
    };
    if task.source_type != "archive" {
        return;
    }
    let Some(handle) = state.archive.cancel(&task.source_id) else {
        return;
    };
    let waiting = handle.snapshot().status == JobStatus::WaitingPassword;
    if waiting && let Some(snapshot) = state.archive.mark_cancelled(&task.source_id) {
        persist_cancelled_snapshot(state, &snapshot).await;
        cleanup_output(state, &task.source_id).await;
        state.archive.remove_if(&task.source_id, &handle);
    }
}

/// Recreate a failed archive task's runtime and start its next attempt.
pub async fn retry_task_runtime(state: &Arc<AppState>, task_id: &str) {
    let Some(record) = find_task(state, task_id).await.ok().flatten() else {
        return;
    };
    if record.source_type != "archive" || record.status != TaskStatus::Retrying {
        return;
    }
    let payload = match parse_payload(&record) {
        Ok(payload) => payload,
        Err(error) => {
            fail_recovered_task(state, task_id, &error).await;
            return;
        }
    };
    let file = match load_archive_file(Arc::clone(state), payload.file_id.clone()).await {
        Ok(file) => file,
        Err(error) => {
            tracing::warn!(%error, task = %task_id, "archive source unavailable during retry");
            fail_recovered_task(state, task_id, "archive source is unavailable").await;
            return;
        }
    };
    if let Some(old) = state.archive.remove(task_id) {
        old.cancel();
        cleanup_output(state, task_id).await;
    }
    let job = ArchiveJob {
        id: task_id.to_owned(),
        file_id: payload.file_id,
        parent_id: payload.parent_id,
        name: file.name.clone(),
        status: JobStatus::Queued,
        progress: 0,
        message: "重试解压".to_owned(),
        output_id: String::new(),
        output_name: String::new(),
        error: String::new(),
        created_at: Timestamp::parse(&record.created_at).unwrap_or_else(|_| Timestamp::now()),
        updated_at: Timestamp::now(),
    };
    let handle = state.archive.register(job);
    spawn_attempt(Arc::clone(state), handle, file, None);
}

/// Remove a terminal archive job's transient runtime state and scratch files.
pub async fn delete_task_runtime(state: &Arc<AppState>, task: &Task) {
    if task.source_type != "archive" {
        return;
    }
    if let Some(handle) = state.archive.remove(&task.source_id) {
        handle.cancel();
    }
    cleanup_output(state, &task.source_id).await;
}

fn spawn_attempt(
    state: Arc<AppState>,
    handle: Arc<crate::archive_runtime::ArchiveJobHandle>,
    file: File,
    password: Option<String>,
) {
    tokio::spawn(async move {
        run_attempt(state, handle, file, password).await;
    });
}

async fn run_attempt(
    state: Arc<AppState>,
    handle: Arc<crate::archive_runtime::ArchiveJobHandle>,
    file: File,
    password: Option<String>,
) {
    let id = handle.id();
    let Some(cancel) = state.archive.start_token(&id) else {
        return;
    };
    let output = match output_directory(&state, &id) {
        Ok(path) => path,
        Err(error) => {
            settle_failure(&state, &handle, error).await;
            return;
        }
    };
    if let Err(error) = remove_output(&output).await {
        settle_failure(
            &state,
            &handle,
            WorkerError::Message(format!("could not prepare archive workspace: {error}")),
        )
        .await;
        return;
    }
    if cancel.is_cancelled() {
        settle_cancelled(&state, &handle, &output).await;
        return;
    }

    let source = match state.store.path_for(&file.object_key) {
        Ok(path) => path,
        Err(error) => {
            settle_failure(&state, &handle, WorkerError::Message(error.to_string())).await;
            return;
        }
    };
    let head = match state.store.head(&file.object_key).await {
        Ok(head) if head.size == file.size => head,
        Ok(head) => {
            settle_failure(
                &state,
                &handle,
                WorkerError::Message(format!(
                    "archive source size changed: stored {}, database {}",
                    head.size, file.size
                )),
            )
            .await;
            return;
        }
        Err(error) => {
            settle_failure(&state, &handle, WorkerError::Message(error.to_string())).await;
            return;
        }
    };
    let source_metadata = match tokio::fs::symlink_metadata(&source).await {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => metadata,
        _ => {
            settle_failure(
                &state,
                &handle,
                WorkerError::Message("archive source is unavailable".to_owned()),
            )
            .await;
            return;
        }
    };
    if source_metadata.len() != u64::try_from(head.size).unwrap_or_default() {
        settle_failure(
            &state,
            &handle,
            WorkerError::Message("archive source size changed".to_owned()),
        )
        .await;
        return;
    }

    handle.update(
        JobStatus::Checking,
        2,
        "正在准备压缩包".to_owned(),
        String::new(),
    );
    persist_snapshot(&state, &handle.snapshot()).await;
    let permit = tokio::select! {
        _ = cancel.cancelled() => {
            settle_cancelled(&state, &handle, &output).await;
            return;
        }
        permit = state.archive.acquire_slot() => match permit {
            Ok(permit) => permit,
            Err(_) => {
                settle_cancelled(&state, &handle, &output).await;
                return;
            }
        },
    };
    // Cancellation and semaphore release can become ready at the same time.
    // Re-check after acquiring the slot so a queued task cannot start an
    // extraction merely because the select chose the permit branch.
    if cancel.is_cancelled() {
        settle_cancelled(&state, &handle, &output).await;
        return;
    }

    let progress = Arc::new(Mutex::new(ArchiveProgress {
        phase: ArchivePhase::Checking,
        entries: 0,
        expanded_bytes: 0,
    }));
    let poll_stop = CancellationToken::new();
    let poll_task = tokio::spawn(poll_progress(
        Arc::clone(&state),
        Arc::clone(&handle),
        Arc::clone(&progress),
        poll_stop.clone(),
    ));
    let engine = state.archive.engine;
    let source_for_worker = source.clone();
    let output_for_worker = output.clone();
    let cancel_for_worker = cancel.clone();
    let progress_for_worker = Arc::clone(&progress);
    let password_for_worker = password.clone();
    let extraction = tokio::task::spawn_blocking(move || {
        engine.extract(
            &source_for_worker,
            &output_for_worker,
            password_for_worker.as_deref(),
            expanded_limit(file.size),
            cancel_for_worker,
            |value| {
                *progress_for_worker
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner) = value;
            },
        )
    })
    .await;
    poll_stop.cancel();
    let _ = poll_task.await;
    drop(permit);

    let extraction = match extraction {
        Ok(result) => result,
        Err(error) => Err(ArchiveError::Input(format!(
            "archive worker failed: {error}"
        ))),
    };
    match extraction {
        Ok(result) => {
            handle.update(
                JobStatus::Extracting,
                25,
                "正在解压".to_owned(),
                String::new(),
            );
            persist_snapshot(&state, &handle.snapshot()).await;
            match import_output(&state, &handle, &file, &output, &cancel, result).await {
                Ok((output_id, output_name)) => {
                    handle.update(JobStatus::Done, 100, "解压完成".to_owned(), String::new());
                    {
                        // The runtime update method intentionally does not
                        // expose output fields; set them while preserving the
                        // same timestamp and locking discipline.
                        let mut snapshot = handle.snapshot();
                        snapshot.output_id = output_id;
                        snapshot.output_name = output_name;
                        // `replace_output` applies the two fields atomically to
                        // the live job before it is persisted below.
                        handle.replace_output(&snapshot.output_id, &snapshot.output_name);
                    }
                    let snapshot = handle.snapshot();
                    persist_snapshot(&state, &snapshot).await;
                    cleanup_output(&state, &id).await;
                    state.archive.remove_if(&id, &handle);
                }
                Err(WorkerError::Cancelled) => settle_cancelled(&state, &handle, &output).await,
                Err(error) => settle_failure(&state, &handle, error).await,
            }
        }
        Err(ArchiveError::PasswordRequired) => {
            cleanup_output(&state, &id).await;
            handle.wait_for_password("压缩包已加密，请输入密码后继续".to_owned());
            persist_snapshot(&state, &handle.snapshot()).await;
            schedule_password_expiry(Arc::clone(&state), id);
        }
        Err(ArchiveError::WrongPassword) => {
            cleanup_output(&state, &id).await;
            handle.wait_for_password("压缩包密码错误，请重新输入".to_owned());
            persist_snapshot(&state, &handle.snapshot()).await;
            schedule_password_expiry(Arc::clone(&state), id);
        }
        Err(ArchiveError::Cancelled) => settle_cancelled(&state, &handle, &output).await,
        Err(error) => settle_failure(&state, &handle, archive_error(error)).await,
    }
}

async fn poll_progress(
    state: Arc<AppState>,
    handle: Arc<crate::archive_runtime::ArchiveJobHandle>,
    progress: Arc<Mutex<ArchiveProgress>>,
    stop: CancellationToken,
) {
    let mut ticker = tokio::time::interval(std::time::Duration::from_millis(400));
    let mut last = None;
    loop {
        tokio::select! {
            _ = stop.cancelled() => return,
            _ = ticker.tick() => {
                let current = *progress.lock().unwrap_or_else(PoisonError::into_inner);
                if last == Some(current) {
                    continue;
                }
                last = Some(current);
                let (status, percent, message) = match current.phase {
                    ArchivePhase::Checking => (JobStatus::Checking, 2, "正在检查压缩包"),
                    ArchivePhase::Extracting => (JobStatus::Extracting, 25, "正在解压"),
                };
                handle.update(status, percent, message.to_owned(), String::new());
                persist_snapshot(&state, &handle.snapshot()).await;
            }
        }
    }
}

#[derive(Debug, thiserror::Error)]
enum WorkerError {
    #[error("archive task cancelled")]
    Cancelled,
    #[error("{0}")]
    Message(String),
}

fn archive_error(error: ArchiveError) -> WorkerError {
    if matches!(error, ArchiveError::Cancelled) {
        WorkerError::Cancelled
    } else {
        let message = error.to_string();
        // The pre-migration server received every non-password extraction
        // failure from the sidecar as a data-plane HTTP error. Preserve that
        // marker for engine errors that are not libarchive open/read errors
        // (for example a rejected link or special file), so the visible task
        // message keeps the old `data plane: ... (400)` envelope.
        WorkerError::Message(if message.starts_with("archive input error:") {
            message
        } else {
            format!("archive input error: {message}")
        })
    }
}

async fn settle_cancelled(
    state: &Arc<AppState>,
    handle: &Arc<crate::archive_runtime::ArchiveJobHandle>,
    output: &Path,
) {
    let id = handle.id();
    handle.update(
        JobStatus::Cancelled,
        handle.snapshot().progress,
        "解压已取消".to_owned(),
        String::new(),
    );
    // A retry replaces the runtime handle while the old worker may still be
    // unwinding. Only the live attempt may settle the durable task.
    if state.archive.is_current(&id, handle) {
        persist_cancelled_snapshot(state, &handle.snapshot()).await;
    }
    let _ = remove_output(output).await;
    state.archive.remove_if(&id, handle);
}

async fn settle_failure(
    state: &Arc<AppState>,
    handle: &Arc<crate::archive_runtime::ArchiveJobHandle>,
    error: WorkerError,
) {
    if matches!(error, WorkerError::Cancelled) || handle.is_cancelled() {
        let output = output_directory(state, &handle.id()).ok();
        if let Some(output) = output {
            settle_cancelled(state, handle, &output).await;
        }
        return;
    }
    let message = archive_failure_message(error.to_string());
    handle.update(
        JobStatus::Failed,
        handle.snapshot().progress,
        message.clone(),
        message,
    );
    persist_snapshot(state, &handle.snapshot()).await;
    cleanup_output(state, &handle.id()).await;
    state.archive.remove_if(&handle.id(), handle);
}

fn archive_failure_message(error: String) -> String {
    let lower = error.to_ascii_lowercase();
    if lower.contains("no space left")
        || lower.contains("disk space is insufficient")
        || lower.contains("disk quota")
        || lower.contains("quota exceeded")
    {
        "解压失败：临时磁盘空间不足，请释放服务器磁盘空间后重试".to_owned()
    } else {
        format!("解压失败：{}", historical_archive_engine_error(&error))
    }
}

/// Preserve the old Go server's visible data-plane error envelope. The
/// pre-migration server called the Rust sidecar over HTTP; extraction failures
/// therefore reached the task as `data plane: <message> (400)`. The migrated
/// in-process engine carries the same libarchive detail in an
/// `archive input error:` wrapper, which must not leak into the user-facing
/// task text.
fn historical_archive_engine_error(error: &str) -> String {
    error.strip_prefix("archive input error:").map_or_else(
        || error.to_owned(),
        |detail| format!("data plane:{} (400)", detail),
    )
}

#[derive(Debug, Clone)]
struct ExtractedEntry {
    relative: String,
    path: PathBuf,
    is_directory: bool,
    size: i64,
}

#[derive(Debug, Clone)]
struct ImportedObject {
    relative: String,
    key: String,
    size: i64,
    etag: String,
    hash: String,
    mime_type: String,
}

async fn import_output(
    state: &Arc<AppState>,
    handle: &Arc<crate::archive_runtime::ArchiveJobHandle>,
    file: &File,
    output: &Path,
    cancel: &CancellationToken,
    result: ArchiveResult,
) -> Result<(String, String), WorkerError> {
    let output_for_scan = output.to_path_buf();
    let cancel_for_scan = cancel.clone();
    let limit = expanded_limit(file.size);
    let entries =
        tokio::task::spawn_blocking(move || scan_output(&output_for_scan, &cancel_for_scan, limit))
            .await
            .map_err(|error| {
                WorkerError::Message(format!("archive scan worker failed: {error}"))
            })??;
    let scanned_bytes = entries
        .iter()
        .filter(|entry| !entry.is_directory)
        .try_fold(0i64, |total, entry| total.checked_add(entry.size))
        .ok_or_else(|| {
            WorkerError::Message("extracted data exceeds the safety limit".to_owned())
        })?;
    if entries.len() < result.entries || scanned_bytes != result.expanded_bytes {
        return Err(WorkerError::Message(
            "archive output changed before import".to_owned(),
        ));
    }
    if cancel.is_cancelled() {
        return Err(WorkerError::Cancelled);
    }
    handle.update(
        JobStatus::Importing,
        35,
        "正在写入网盘".to_owned(),
        String::new(),
    );
    persist_snapshot(state, &handle.snapshot()).await;

    let regular_count = entries.iter().filter(|entry| !entry.is_directory).count();
    let mut stored_keys = Vec::with_capacity(regular_count);
    let mut objects = Vec::with_capacity(regular_count);
    for entry in entries.iter().filter(|entry| !entry.is_directory) {
        if cancel.is_cancelled() {
            cleanup_import_keys(state, &stored_keys).await;
            return Err(WorkerError::Cancelled);
        }
        let key = keys::blob_key(&crate::ids::new_id());
        let input = match tokio::fs::File::open(&entry.path).await {
            Ok(input) => input,
            Err(error) => {
                cleanup_import_keys(state, &stored_keys).await;
                return Err(WorkerError::Message(format!(
                    "open extracted file: {error}"
                )));
            }
        };
        let mut input = CancellableReader {
            reader: input,
            cancel,
        };
        let stored = match state.store.write_stream(&key, &mut input, entry.size).await {
            Ok(stored) => stored,
            Err(_error) if cancel.is_cancelled() => {
                cleanup_import_keys(state, &stored_keys).await;
                return Err(WorkerError::Cancelled);
            }
            Err(error) => {
                cleanup_import_keys(state, &stored_keys).await;
                return Err(WorkerError::Message(format!(
                    "upload extracted file: {error}"
                )));
            }
        };
        stored_keys.push(key.clone());
        if stored.size != entry.size {
            cleanup_import_keys(state, &stored_keys).await;
            return Err(WorkerError::Message(format!(
                "stored extracted file size {} does not match local size {}",
                stored.size, entry.size
            )));
        }
        let hash = match hash_object(&state.store, &key, entry.size, cancel).await {
            Ok(hash) => hash,
            Err(error) => {
                cleanup_import_keys(state, &stored_keys).await;
                return Err(error);
            }
        };
        let mime_type = historical_archive_mime_type(&entry.relative);
        objects.push(ImportedObject {
            relative: entry.relative.clone(),
            key,
            size: entry.size,
            etag: stored.etag,
            hash,
            mime_type,
        });
        let progress = 35
            + i32::try_from(
                objects
                    .len()
                    .saturating_mul(58)
                    .checked_div(regular_count)
                    .unwrap_or(58),
            )
            .unwrap_or(58);
        handle.update(
            JobStatus::Importing,
            progress,
            "正在写入网盘".to_owned(),
            String::new(),
        );
        persist_snapshot(state, &handle.snapshot()).await;
    }
    let preferred = archive_base_name(&file.name);
    match commit_import(
        state,
        &handle.id(),
        &handle.parent_id(),
        &preferred,
        &entries,
        &objects,
    )
    .await
    {
        Ok(Some(result)) => Ok(result),
        Ok(None) => {
            cleanup_import_keys(state, &stored_keys).await;
            Err(WorkerError::Cancelled)
        }
        Err(error) => {
            cleanup_import_keys(state, &stored_keys).await;
            Err(error)
        }
    }
}

async fn cleanup_import_keys(state: &Arc<AppState>, keys_to_delete: &[String]) {
    for key in keys_to_delete {
        if let Err(error) = state.store.delete(key).await {
            tracing::warn!(%error, key, "could not discard failed archive object");
        }
    }
}

async fn hash_object(
    store: &crate::storage::LocalStore,
    key: &str,
    expected_size: i64,
    cancel: &CancellationToken,
) -> Result<String, WorkerError> {
    let mut object = store
        .open_object(key)
        .await
        .map_err(|error| WorkerError::Message(format!("open stored extracted file: {error}")))?;
    if object.size != expected_size {
        return Err(WorkerError::Message(
            "stored extracted file size changed".to_owned(),
        ));
    }
    let mut hasher = Sha256::new();
    let mut bytes = 0i64;
    let mut buffer = [0u8; 256 << 10];
    loop {
        let read = tokio::select! {
            _ = cancel.cancelled() => return Err(WorkerError::Cancelled),
            result = object.file.read(&mut buffer) => result
                .map_err(|error| WorkerError::Message(format!("hash stored extracted file: {error}")))?,
        };
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        bytes = bytes
            .checked_add(i64::try_from(read).map_err(|_| {
                WorkerError::Message("stored extracted file is too large".to_owned())
            })?)
            .ok_or_else(|| WorkerError::Message("stored extracted file is too large".to_owned()))?;
    }
    if bytes != expected_size {
        return Err(WorkerError::Message(
            "stored extracted file size changed".to_owned(),
        ));
    }
    Ok(hex::encode(hasher.finalize()))
}

async fn commit_import(
    state: &Arc<AppState>,
    task_id: &str,
    parent_id: &str,
    preferred_name: &str,
    entries: &[ExtractedEntry],
    objects: &[ImportedObject],
) -> Result<Option<(String, String)>, WorkerError> {
    let task_id = task_id.to_owned();
    let parent_id = parent_id.to_owned();
    let preferred_name = preferred_name.to_owned();
    let entries = entries.to_vec();
    let objects = objects.to_vec();
    state
        .db
        .call_api(move |connection| {
            commit_import_sync(
                connection,
                &task_id,
                &parent_id,
                &preferred_name,
                &entries,
                &objects,
            )
        })
        .await
        .map_err(|error| WorkerError::Message(error.message))
}

fn commit_import_sync(
    connection: &mut Connection,
    task_id: &str,
    parent_id: &str,
    preferred_name: &str,
    entries: &[ExtractedEntry],
    objects: &[ImportedObject],
) -> Result<Option<(String, String)>, ApiError> {
    let transaction = connection
        .transaction()
        .map_err(|error| database_error(DbError::Query(error)))?;
    let task_state: Option<(String, bool)> = transaction
        .query_row(
            "SELECT status,cancel_requested FROM tasks WHERE id = ?1",
            [task_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(|error| database_error(DbError::Query(error)))?;
    if !matches!(task_state.as_ref(), Some((status, false)) if status == "running") {
        return Ok(None);
    }
    let valid_parent: bool = transaction
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM files WHERE id = ?1 AND kind = 'directory' AND status = 'ready' AND deleted_at IS NULL)",
            [parent_id],
            |row| row.get(0),
        )
        .map_err(|error| database_error(DbError::Query(error)))?;
    if !valid_parent {
        return Err(ApiError::conflict(
            "destination directory is no longer available",
        ));
    }
    let root_name = available_archive_name(&transaction, parent_id, preferred_name)?;
    let now = Timestamp::now().to_rfc3339();
    let root_id = crate::ids::new_id();
    transaction
        .execute(
            "INSERT INTO files(id,parent_id,name,kind,status,created_at,updated_at) \
             VALUES(?1,?2,?3,'directory','ready',?4,?4)",
            rusqlite::params![root_id, parent_id, root_name, now],
        )
        .map_err(|error| conflict_or(DbError::Query(error)))?;

    let mut directories = BTreeMap::new();
    directories.insert(String::new(), root_id.clone());
    let mut directory_paths: Vec<String> = entries
        .iter()
        .filter(|entry| entry.is_directory)
        .map(|entry| entry.relative.clone())
        .collect();
    directory_paths.sort_by(|left, right| {
        let left_depth = left.bytes().filter(|byte| *byte == b'/').count();
        let right_depth = right.bytes().filter(|byte| *byte == b'/').count();
        left_depth.cmp(&right_depth).then_with(|| left.cmp(right))
    });
    for relative in directory_paths {
        let parent_relative = parent_relative(&relative);
        let Some(directory_parent) = directories.get(parent_relative).cloned() else {
            return Err(ApiError::internal("archive directory tree is invalid"));
        };
        let name = basename(&relative);
        let id = crate::ids::new_id();
        transaction
            .execute(
                "INSERT INTO files(id,parent_id,name,kind,status,created_at,updated_at) \
                 VALUES(?1,?2,?3,'directory','ready',?4,?4)",
                rusqlite::params![id, directory_parent, name, now],
            )
            .map_err(|error| conflict_or(DbError::Query(error)))?;
        directories.insert(relative, id);
    }

    for object in objects {
        let parent_relative = parent_relative(&object.relative);
        let Some(directory_parent) = directories.get(parent_relative).cloned() else {
            return Err(ApiError::internal("archive file tree is invalid"));
        };
        transaction
            .execute(
                "INSERT INTO files(id,parent_id,name,kind,object_key,size,mime_type,etag,content_hash,hash_algorithm,status,created_at,updated_at) \
                 VALUES(?1,?2,?3,'file',?4,?5,?6,?7,?8,?9,?10,?11,?11)",
                rusqlite::params![
                    crate::ids::new_id(),
                    directory_parent,
                    basename(&object.relative),
                    object.key,
                    object.size,
                    object.mime_type,
                    object.etag,
                    object.hash,
                    CONTENT_HASH_ALGORITHM,
                    "ready",
                    now,
                ],
            )
            .map_err(|error| conflict_or(DbError::Query(error)))?;
    }
    let changed = transaction
        .execute(
            "UPDATE tasks SET status='completed',phase='done',progress=100,error='',finished_at=COALESCE(finished_at,?1),updated_at=?1 WHERE id=?2 AND status='running' AND cancel_requested=0",
            rusqlite::params![now, task_id],
        )
        .map_err(|error| database_error(DbError::Query(error)))?;
    if changed != 1 {
        // A cancellation can win immediately before this transaction. The
        // inserts above are still uncommitted, so returning rolls them back.
        return Ok(None);
    }
    transaction
        .commit()
        .map_err(|error| database_error(DbError::Query(error)))?;
    Ok(Some((root_id, root_name)))
}

fn available_archive_name(
    connection: &Connection,
    parent_id: &str,
    preferred_name: &str,
) -> Result<String, ApiError> {
    let preferred = if validate_name(preferred_name).is_ok() {
        preferred_name.to_owned()
    } else {
        "解压文件".to_owned()
    };
    for index in 0..10_000 {
        let candidate = if index == 0 {
            preferred.clone()
        } else {
            format!("{preferred} ({})", index + 1)
        };
        if validate_name(&candidate).is_err() {
            return Err(ApiError::internal(
                "could not choose extraction folder name",
            ));
        }
        let taken: bool = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM files WHERE parent_id = ?1 AND name = ?2 AND deleted_at IS NULL)",
                rusqlite::params![parent_id, candidate],
                |row| row.get(0),
            )
            .map_err(|error| database_error(DbError::Query(error)))?;
        if !taken {
            return Ok(candidate);
        }
    }
    Err(ApiError::internal(
        "could not choose extraction folder name",
    ))
}

fn archive_base_name(name: &str) -> String {
    let lower = name.to_ascii_lowercase();
    for suffix in ARCHIVE_SUFFIXES {
        let marker = format!(".{suffix}");
        if lower.ends_with(&marker) {
            return name[..name.len() - marker.len()].to_owned();
        }
    }
    let extension = classify::extension(name);
    if extension.is_empty() {
        name.to_owned()
    } else {
        name[..name.len() - extension.len() - 1].to_owned()
    }
}

/// Match the pre-migration `mime.TypeByExtension` value used for extracted
/// files. Go appends an UTF-8 charset to text types returned from its MIME
/// table, while `mime_guess` intentionally returns the bare registered type.
fn historical_archive_mime_type(path: &str) -> String {
    let mime = mime_guess::from_path(path)
        .first_raw()
        .unwrap_or("application/octet-stream");
    if mime.starts_with("text/") && !mime.contains(';') {
        format!("{mime}; charset=utf-8")
    } else {
        mime.to_owned()
    }
}

fn scan_output(
    output: &Path,
    cancel: &CancellationToken,
    limit: i64,
) -> Result<Vec<ExtractedEntry>, WorkerError> {
    let metadata = fs::symlink_metadata(output)
        .map_err(|error| WorkerError::Message(format!("inspect archive output: {error}")))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(WorkerError::Message(
            "archive output is not a directory".to_owned(),
        ));
    }
    let mut pending = vec![output.to_path_buf()];
    let mut entries = Vec::new();
    let mut expanded = 0i64;
    while let Some(directory) = pending.pop() {
        let children = fs::read_dir(&directory)
            .map_err(|error| WorkerError::Message(format!("read archive output: {error}")))?;
        for child in children {
            if cancel.is_cancelled() {
                return Err(WorkerError::Cancelled);
            }
            let child = child.map_err(|error| {
                WorkerError::Message(format!("read archive output entry: {error}"))
            })?;
            let path = child.path();
            let file_type = child.file_type().map_err(|error| {
                WorkerError::Message(format!("inspect archive output entry: {error}"))
            })?;
            let raw_relative = path
                .strip_prefix(output)
                .ok()
                .and_then(|relative| relative.to_str())
                .ok_or_else(|| {
                    WorkerError::Message("archive entry path is not valid UTF-8".to_owned())
                })?;
            let relative = revaro_core::validate::normalize_archive_path(raw_relative)
                .map_err(|error| WorkerError::Message(error.message))?;
            if file_type.is_symlink() {
                return Err(WorkerError::Message(
                    "archive contains unsupported links or special files".to_owned(),
                ));
            }
            if file_type.is_dir() {
                entries.push(ExtractedEntry {
                    relative,
                    path,
                    is_directory: true,
                    size: 0,
                });
                pending.push(child.path());
            } else if file_type.is_file() {
                let size = i64::try_from(
                    child
                        .metadata()
                        .map_err(|error| {
                            WorkerError::Message(format!("inspect extracted file: {error}"))
                        })?
                        .len(),
                )
                .map_err(|_| WorkerError::Message("extracted file is too large".to_owned()))?;
                expanded = expanded.checked_add(size).ok_or_else(|| {
                    WorkerError::Message("extracted data exceeds the safety limit".to_owned())
                })?;
                if expanded > limit {
                    return Err(WorkerError::Message(
                        "extracted data exceeds the safety limit".to_owned(),
                    ));
                }
                entries.push(ExtractedEntry {
                    relative,
                    path,
                    is_directory: false,
                    size,
                });
            } else {
                return Err(WorkerError::Message(
                    "archive contains unsupported links or special files".to_owned(),
                ));
            }
            if entries.len() > MAX_ARCHIVE_ENTRIES {
                return Err(WorkerError::Message(format!(
                    "archive contains more than {MAX_ARCHIVE_ENTRIES} entries"
                )));
            }
        }
    }
    entries.sort_by(|left, right| left.relative.cmp(&right.relative));
    Ok(entries)
}

fn parent_relative(relative: &str) -> &str {
    relative.rsplit_once('/').map_or("", |(parent, _)| parent)
}

fn basename(relative: &str) -> &str {
    relative.rsplit('/').next().unwrap_or(relative)
}

fn output_directory(state: &Arc<AppState>, id: &str) -> Result<PathBuf, WorkerError> {
    if !revaro_core::ids::is_uuid(id) {
        return Err(WorkerError::Message("invalid archive job id".to_owned()));
    }
    Ok(state.config.work_dir.join(format!("revaro-extract-{id}")))
}

async fn cleanup_output(state: &Arc<AppState>, id: &str) {
    if let Ok(path) = output_directory(state, id)
        && let Err(error) = remove_output(&path).await
    {
        tracing::warn!(%error, task = %id, "archive workspace cleanup failed");
    }
}

async fn remove_output(path: &Path) -> Result<(), std::io::Error> {
    let metadata = match tokio::fs::symlink_metadata(path).await {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        tokio::fs::remove_file(path).await
    } else {
        tokio::fs::remove_dir_all(path).await
    }
}

struct CancellableReader<'a, R> {
    reader: R,
    cancel: &'a CancellationToken,
}

impl<R: AsyncRead + Unpin> AsyncRead for CancellableReader<'_, R> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        if self.cancel.is_cancelled() {
            return Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "archive task cancelled",
            )));
        }
        Pin::new(&mut self.reader).poll_read(cx, buffer)
    }
}

fn schedule_password_expiry(state: Arc<AppState>, id: String) {
    tokio::spawn(async move {
        tokio::time::sleep(crate::archive_runtime::PASSWORD_WAIT_TTL).await;
        if let Some(snapshot) = state.archive.expire_password_wait(&id) {
            persist_snapshot(&state, &snapshot).await;
            state.archive.remove(&id);
            cleanup_output(&state, &id).await;
        }
    });
}

async fn persist_snapshot(state: &Arc<AppState>, snapshot: &ArchiveJob) {
    let task_status = task_status_for(snapshot.status);
    let task_status = task_status.as_str().to_owned();
    let phase = archive_status_name(snapshot.status).to_owned();
    let progress = f64::from(snapshot.progress);
    let error = snapshot.error.clone();
    let id = snapshot.id.clone();
    let now = Timestamp::now().to_rfc3339();
    // A user cancellation is committed before the runtime token is notified.
    // Ordinary progress writes must therefore never resurrect that task. The
    // cancellation snapshot itself is allowed to finish the transition.
    let result = state
        .db
        .call_api(move |connection| {
            let changed = match task_status.as_str() {
                "running" => connection
                    .execute(
                        "UPDATE tasks SET status=?1,phase=?2,progress=?3,error=?4,started_at=COALESCE(started_at,?5),heartbeat_at=?5,updated_at=?5 WHERE id=?6 AND cancel_requested=0 AND status NOT IN ('completed','failed','cancelled')",
                        rusqlite::params![task_status, phase, progress, error, now, id],
                    )
                    .map_err(|error| database_error(DbError::Query(error)))?,
                "completed" | "failed" => connection
                    .execute(
                        "UPDATE tasks SET status=?1,phase=?2,progress=?3,error=?4,finished_at=COALESCE(finished_at,?5),updated_at=?5 WHERE id=?6 AND cancel_requested=0 AND status NOT IN ('completed','failed','cancelled')",
                        rusqlite::params![task_status, phase, progress, error, now, id],
                    )
                    .map_err(|error| database_error(DbError::Query(error)))?,
                "cancelled" => connection
                    .execute(
                        "UPDATE tasks SET status=?1,phase=?2,progress=?3,error=?4,finished_at=COALESCE(finished_at,?5),updated_at=?5 WHERE id=?6 AND cancel_requested=1 AND status NOT IN ('completed','failed','cancelled')",
                        rusqlite::params![task_status, phase, progress, error, now, id],
                    )
                    .map_err(|error| database_error(DbError::Query(error)))?,
                _ => connection.execute(
                    "UPDATE tasks SET status=?1,phase=?2,progress=?3,error=?4,finished_at=NULL,updated_at=?5 WHERE id=?6 AND cancel_requested=0 AND status NOT IN ('completed','failed','cancelled')",
                    rusqlite::params![task_status, phase, progress, error, now, id],
                ).map_err(|error| database_error(DbError::Query(error)))?,
            };
            Ok(changed)
        })
        .await;
    if let Err(error) = result {
        tracing::warn!(%error, task = %snapshot.id, "could not persist archive task state");
    }
    state.jobs.changed();
}

/// Persist cancellation during process shutdown or after a worker has observed
/// its token. The runtime identity check keeps an old retry attempt from
/// cancelling the replacement attempt that reuses the same durable task id.
async fn persist_cancelled_snapshot(state: &Arc<AppState>, snapshot: &ArchiveJob) {
    let task_status = task_status_for(snapshot.status);
    let task_status = task_status.as_str().to_owned();
    let phase = archive_status_name(snapshot.status).to_owned();
    let progress = f64::from(snapshot.progress);
    let error = snapshot.error.clone();
    let id = snapshot.id.clone();
    let now = Timestamp::now().to_rfc3339();
    let result = state
        .db
        .call_api(move |connection| {
            connection
                .execute(
                    "UPDATE tasks SET status=?1,phase=?2,progress=?3,error=?4,finished_at=COALESCE(finished_at,?5),updated_at=?5 WHERE id=?6 AND status IN ('queued','running','retrying','waiting_input')",
                    rusqlite::params![task_status, phase, progress, error, now, id],
                )
                .map_err(|error| database_error(DbError::Query(error)))
        })
        .await;
    if let Err(error) = result {
        tracing::warn!(%error, task = %snapshot.id, "could not persist cancelled archive task state");
    }
    state.jobs.changed();
}

fn task_status_for(status: JobStatus) -> TaskStatus {
    match status {
        JobStatus::Queued => TaskStatus::Queued,
        JobStatus::WaitingPassword => TaskStatus::WaitingInput,
        JobStatus::Done => TaskStatus::Completed,
        JobStatus::Failed => TaskStatus::Failed,
        JobStatus::Cancelled => TaskStatus::Cancelled,
        JobStatus::Downloading
        | JobStatus::Checking
        | JobStatus::Extracting
        | JobStatus::Importing => TaskStatus::Running,
    }
}

fn archive_status_name(status: JobStatus) -> &'static str {
    match status {
        JobStatus::Queued => "queued",
        JobStatus::Downloading => "downloading",
        JobStatus::Checking => "checking",
        JobStatus::Extracting => "extracting",
        JobStatus::Importing => "importing",
        JobStatus::WaitingPassword => "waiting_password",
        JobStatus::Done => "done",
        JobStatus::Failed => "failed",
        JobStatus::Cancelled => "cancelled",
    }
}

async fn fail_recovered_task(state: &Arc<AppState>, id: &str, message: &str) {
    let id = id.to_owned();
    let id_for_query = id.clone();
    let message = message.to_owned();
    let now = Timestamp::now().to_rfc3339();
    let result = state
        .db
        .call_api(move |connection| {
            connection
                .execute(
                    "UPDATE tasks SET status='failed',phase='recovery',progress=0,error=?1,finished_at=?2,updated_at=?2 WHERE id=?3",
                    rusqlite::params![message, now, id_for_query],
                )
                .map_err(|error| database_error(DbError::Query(error)))
        })
        .await;
    if let Err(error) = result {
        tracing::warn!(%error, task = %id, "could not mark recovered archive task failed");
    }
    state.jobs.changed();
}

fn database_error(error: DbError) -> ApiError {
    tracing::error!(%error, "archive database query failed");
    ApiError::internal("database error")
}

fn not_found_or(error: DbError, message: &'static str) -> ApiError {
    if error.is_not_found() {
        ApiError::not_found(message)
    } else {
        database_error(error)
    }
}

fn conflict_or(error: DbError) -> ApiError {
    if error.is_constraint_violation() {
        ApiError::conflict("an item with that name already exists")
    } else {
        database_error(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use http::{Request, StatusCode};
    use http_body_util::BodyExt as _;
    use revaro_core::ids::ROOT_ID;
    use tower::ServiceExt as _;
    use zip::ZipWriter;
    use zip::unstable::write::FileOptionsExt as _;
    use zip::write::SimpleFileOptions;

    const SESSION: &str = "archive-route-session";

    async fn state() -> Arc<AppState> {
        let config = crate::config::Config::from_lookup(&|name| match name {
            "APP_BASE_URL" => Some("http://localhost:8080".to_owned()),
            "APP_WEB_DIR" => Some("/nonexistent-web-dir".to_owned()),
            _ => None,
        })
        .unwrap();
        let store_root = std::env::temp_dir().join(format!(
            "revaro-archive-route-store-{}",
            uuid::Uuid::new_v4()
        ));
        let work_dir = std::env::temp_dir().join(format!(
            "revaro-archive-route-work-{}",
            uuid::Uuid::new_v4()
        ));
        let mut config = config;
        config.work_dir = work_dir;
        let store = crate::storage::LocalStore::open(store_root).await.unwrap();
        let database = crate::db::Database::open_in_memory().unwrap();
        let auth = crate::auth::AuthService::new(database.clone());
        AppState::new(Arc::new(config), database, store, auth)
    }

    async fn authenticate(state: &Arc<AppState>) {
        let token_hash = crate::auth::token_hash(SESSION);
        state
            .db
            .call(move |connection| {
                connection
                    .execute(
                        "INSERT OR REPLACE INTO settings(key,value,updated_at) VALUES('admin_username','admin','2024-01-01T00:00:00Z')",
                        [],
                    )
                    .map_err(DbError::Query)?;
                connection
                    .execute(
                        "INSERT OR REPLACE INTO sessions(id,token_hash,created_at,expires_at) VALUES('archive-session',?1,'2024-01-01T00:00:00Z','2999-01-01T00:00:00Z')",
                        [&token_hash],
                    )
                    .map_err(DbError::Query)?;
                Ok(())
            })
            .await
            .unwrap();
    }

    async fn request(
        state: &Arc<AppState>,
        method: &str,
        uri: &str,
        body: Option<Vec<u8>>,
    ) -> (StatusCode, serde_json::Value) {
        authenticate(state).await;
        let mut builder = Request::builder().method(method).uri(uri).header(
            "cookie",
            format!("{}={SESSION}", crate::auth::SESSION_COOKIE),
        );
        if body.is_some() {
            builder = builder
                .header("content-type", "application/json")
                .header("origin", "http://localhost:8080");
        } else if method != "GET" {
            builder = builder.header("origin", "http://localhost:8080");
        }
        let response = crate::router::build(state.clone())
            .oneshot(builder.body(Body::from(body.unwrap_or_default())).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
        )
    }

    fn zip_bytes() -> Vec<u8> {
        let mut writer = ZipWriter::new(std::io::Cursor::new(Vec::new()));
        writer
            .start_file("nested/hello.txt", SimpleFileOptions::default())
            .unwrap();
        std::io::Write::write_all(&mut writer, b"hello from archive").unwrap();
        writer.finish().unwrap().into_inner()
    }

    fn encrypted_zip_bytes() -> Vec<u8> {
        let mut writer = ZipWriter::new(std::io::Cursor::new(Vec::new()));
        writer
            .start_file(
                "secret.txt",
                SimpleFileOptions::default().with_deprecated_encryption(b"secret"),
            )
            .unwrap();
        std::io::Write::write_all(&mut writer, b"encrypted archive").unwrap();
        writer.finish().unwrap().into_inner()
    }

    async fn seed_archive(state: &Arc<AppState>, name: &str) -> String {
        let name = name.to_owned();
        let file_id = uuid::Uuid::new_v4().to_string();
        let object_key = keys::blob_key(&file_id);
        let bytes = zip_bytes();
        seed_archive_with_bytes(state, name, file_id, object_key, bytes).await
    }

    async fn seed_archive_with_bytes(
        state: &Arc<AppState>,
        name: String,
        file_id: String,
        object_key: String,
        bytes: Vec<u8>,
    ) -> String {
        let size = i64::try_from(bytes.len()).unwrap();
        let mut reader: &[u8] = &bytes;
        let info = state
            .store
            .write_stream(&object_key, &mut reader, size)
            .await
            .unwrap();
        let now = Timestamp::now().to_rfc3339();
        let file_id_for_db = file_id.clone();
        let object_key_for_db = object_key.clone();
        state
            .db
            .call(move |connection| {
                connection
                    .execute(
                    "INSERT INTO files(id,parent_id,name,kind,object_key,size,mime_type,etag,status,created_at,updated_at) VALUES(?1,?2,?3,'file',?4,?5,'application/zip',?6,'ready',?7,?7)",
                        rusqlite::params![file_id_for_db, ROOT_ID, name, object_key_for_db, size, info.etag, now],
                    )
                    .map_err(DbError::Query)?;
                Ok(())
            })
            .await
            .unwrap();
        file_id
    }

    async fn wait_for_task_status(state: &Arc<AppState>, task_id: &str, expected: &str) {
        for _ in 0..50 {
            let status: String = state
                .db
                .call({
                    let task_id = task_id.to_owned();
                    move |connection| {
                        connection
                            .query_row(
                                "SELECT status FROM tasks WHERE id = ?1",
                                [&task_id],
                                |row| row.get(0),
                            )
                            .map_err(DbError::Query)
                    }
                })
                .await
                .unwrap();
            if status == expected {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        panic!("task {task_id} did not reach {expected}");
    }

    async fn wait_for_password_prompt(state: &Arc<AppState>, task_id: &str) {
        for _ in 0..50 {
            let durable_waiting = state
                .db
                .call({
                    let task_id = task_id.to_owned();
                    move |connection| {
                        connection
                            .query_row(
                                "SELECT status = 'waiting_input' FROM tasks WHERE id = ?1",
                                [&task_id],
                                |row| row.get(0),
                            )
                            .map_err(DbError::Query)
                    }
                })
                .await
                .unwrap();
            let runtime_waiting = state
                .archive
                .get(task_id)
                .is_some_and(|handle| handle.snapshot().status == JobStatus::WaitingPassword);
            if durable_waiting && runtime_waiting {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        panic!("archive task {task_id} did not reach a password prompt");
    }

    #[tokio::test]
    async fn extract_route_creates_a_task_and_imports_real_zip_entries() {
        let state = state().await;
        let file_id = seed_archive(&state, "books.zip").await;
        let (status, body) = request(
            &state,
            "POST",
            &format!("/api/files/{file_id}/extract"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED);
        assert_eq!(body["status"], "queued");
        let task_id = body["id"].as_str().unwrap().to_owned();

        for _ in 0..50 {
            let completed: bool = state
                .db
                .call({
                    let task_id = task_id.clone();
                    move |connection| {
                        connection
                            .query_row(
                                "SELECT status = 'completed' FROM tasks WHERE id = ?1",
                                [&task_id],
                                |row| row.get(0),
                            )
                            .map_err(DbError::Query)
                    }
                })
                .await
                .unwrap();
            if completed {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let (status, task) = request(&state, "GET", &format!("/api/tasks/{task_id}"), None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(task["status"], "completed");
        let imported: (i64, String, String, String) = state
            .db
            .call({
                move |connection| {
                    connection
                        .query_row(
                            "SELECT size,mime_type,content_hash,hash_algorithm FROM files WHERE name='hello.txt'",
                            [],
                            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                        )
                        .map_err(DbError::Query)
                }
            })
            .await
            .unwrap();
        assert_eq!(imported.0, 18);
        assert_eq!(imported.1, "text/plain; charset=utf-8");
        assert_eq!(imported.2, keys::sha256_hex(b"hello from archive"));
        assert_eq!(imported.3, "sha256");
        let (status, children) = request(
            &state,
            "GET",
            &format!("/api/files/{ROOT_ID}/children"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            children["items"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["name"] == "books")
        );
        let _ = state.archive.remove(&task_id);
    }

    #[tokio::test]
    async fn cancellation_while_queued_reaches_the_durable_task() {
        let state = state().await;
        let file_id = seed_archive(&state, "queued.zip").await;
        let permit = state.archive.acquire_slot().await.unwrap();
        let (status, body) = request(
            &state,
            "POST",
            &format!("/api/files/{file_id}/extract"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED);
        let task_id = body["id"].as_str().unwrap().to_owned();
        let (status, _) = request(
            &state,
            "POST",
            &format!("/api/tasks/{task_id}/cancel"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        drop(permit);
        // The full workspace runs this beside media and reader integration
        // tests. Keep the durable-state poll long enough that scheduler and
        // SQLite pool contention cannot make this lifecycle assertion flaky.
        for _ in 0..250 {
            let cancelled: bool = state
                .db
                .call({
                    let task_id = task_id.clone();
                    move |connection| {
                        connection
                            .query_row(
                                "SELECT status = 'cancelled' FROM tasks WHERE id = ?1",
                                [&task_id],
                                |row| row.get(0),
                            )
                            .map_err(DbError::Query)
                    }
                })
                .await
                .unwrap();
            if cancelled {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let cancelled: bool = state
            .db
            .call({
                let task_id = task_id.clone();
                move |connection| {
                    connection
                        .query_row(
                            "SELECT status = 'cancelled' FROM tasks WHERE id = ?1",
                            [&task_id],
                            |row| row.get(0),
                        )
                        .map_err(DbError::Query)
                }
            })
            .await
            .unwrap();
        assert!(cancelled);
        assert!(
            !state
                .config
                .work_dir
                .join(format!("revaro-extract-{task_id}"))
                .exists()
        );
    }

    #[tokio::test]
    async fn encrypted_archive_waits_for_input_and_resumes_with_password() {
        let state = state().await;
        let file_id = uuid::Uuid::new_v4().to_string();
        let file_id = seed_archive_with_bytes(
            &state,
            "secret.zip".to_owned(),
            file_id.clone(),
            keys::blob_key(&file_id),
            encrypted_zip_bytes(),
        )
        .await;
        let (status, body) = request(
            &state,
            "POST",
            &format!("/api/files/{file_id}/extract"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED);
        let task_id = body["id"].as_str().unwrap().to_owned();

        wait_for_password_prompt(&state, &task_id).await;
        let (status, body) = request(
            &state,
            "POST",
            &format!("/api/tasks/{task_id}/input"),
            Some(br#"{"password":"wrong"}"#.to_vec()),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED, "{body}");
        assert_eq!(body["status"], "checking");

        wait_for_password_prompt(&state, &task_id).await;
        let (status, body) = request(
            &state,
            "POST",
            &format!("/api/tasks/{task_id}/input"),
            Some(br#"{"password":"secret"}"#.to_vec()),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED, "{body}");
        assert_eq!(body["status"], "checking");

        wait_for_task_status(&state, &task_id, "completed").await;
        let imported: (i64, String) = state
            .db
            .call(|connection| {
                connection
                    .query_row(
                        "SELECT size,content_hash FROM files WHERE name='secret.txt'",
                        [],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .map_err(DbError::Query)
            })
            .await
            .unwrap();
        assert_eq!(imported.0, 17);
        assert_eq!(imported.1, keys::sha256_hex(b"encrypted archive"));
    }

    #[tokio::test]
    async fn cancelled_task_is_not_completed_by_import_commit() {
        let state = state().await;
        let task_id = uuid::Uuid::new_v4().to_string();
        let now = Timestamp::now().to_rfc3339();
        state
            .db
            .call({
                let task_id = task_id.clone();
                move |connection| {
                    connection
                        .execute(
                            "INSERT INTO tasks(id,type,status,phase,progress,error,payload_json,created_at,updated_at) VALUES(?1,'archive_extract','cancelled','cancelled',0,'','{}',?2,?2)",
                            rusqlite::params![task_id, now],
                        )
                        .map_err(DbError::Query)?;
                    Ok(())
                }
            })
            .await
            .unwrap();

        let result = state
            .db
            .call_api(move |connection| {
                commit_import_sync(connection, &task_id, ROOT_ID, "cancelled-output", &[], &[])
            })
            .await
            .unwrap();
        assert_eq!(result, None);
        let output_count: i64 = state
            .db
            .call(|connection| {
                connection
                    .query_row(
                        "SELECT COUNT(*) FROM files WHERE name='cancelled-output'",
                        [],
                        |row| row.get(0),
                    )
                    .map_err(DbError::Query)
            })
            .await
            .unwrap();
        assert_eq!(output_count, 0);
    }

    #[tokio::test]
    async fn cancel_requested_running_task_is_not_completed_by_import_commit() {
        let state = state().await;
        let task_id = uuid::Uuid::new_v4().to_string();
        let now = Timestamp::now().to_rfc3339();
        state
            .db
            .call({
                let task_id = task_id.clone();
                move |connection| {
                    connection
                        .execute(
                            "INSERT INTO tasks(id,type,status,phase,progress,error,cancel_requested,payload_json,created_at,updated_at) VALUES(?1,'archive_extract','running','importing',35,'',1,'{}',?2,?2)",
                            rusqlite::params![task_id, now],
                        )
                        .map_err(DbError::Query)?;
                    Ok(())
                }
            })
            .await
            .unwrap();

        let task_id_for_status = task_id.clone();
        let result = state
            .db
            .call_api(move |connection| {
                commit_import_sync(connection, &task_id, ROOT_ID, "cancelled-output", &[], &[])
            })
            .await
            .unwrap();
        assert_eq!(result, None);

        let status: String = state
            .db
            .call(move |connection| {
                connection
                    .query_row(
                        "SELECT status FROM tasks WHERE id = ?1",
                        [&task_id_for_status],
                        |row| row.get(0),
                    )
                    .map_err(DbError::Query)
            })
            .await
            .unwrap();
        assert_eq!(status, "running");
    }

    #[tokio::test]
    async fn task_input_rejects_non_waiting_tasks_with_the_historical_conflict() {
        let state = state().await;
        let (status, body) = request(
            &state,
            "POST",
            "/api/tasks/missing/input",
            Some(br#"{"password":"secret"}"#.to_vec()),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["error"]["message"], "task is not waiting for input");
    }

    #[test]
    fn archive_name_and_limit_helpers_match_the_product_contract() {
        assert_eq!(archive_base_name("book.tar.gz"), "book");
        assert_eq!(archive_base_name("book.ZIP"), "book");
        assert_eq!(archive_base_name(".zip"), "");
        assert_eq!(
            historical_archive_mime_type("entry.txt"),
            "text/plain; charset=utf-8"
        );
        assert_eq!(historical_archive_mime_type("entry.png"), "image/png");
        assert_eq!(
            historical_archive_mime_type("entry.unknown"),
            "application/octet-stream"
        );
        assert_eq!(parent_relative("a/b/c.txt"), "a/b");
        assert_eq!(basename("a/b/c.txt"), "c.txt");
        assert_eq!(expanded_limit(1), 4 << 30);
        assert_eq!(
            historical_archive_engine_error("archive input error: invalid archive"),
            "data plane: invalid archive (400)"
        );
        assert_eq!(
            historical_archive_engine_error("invalid archive"),
            "invalid archive"
        );
    }
}
