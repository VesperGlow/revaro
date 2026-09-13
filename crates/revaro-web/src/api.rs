//! The slice of the HTTP client the shell needs.
//!
//! The Vue app funnelled every request through `web/src/api.ts`, which merged a
//! caller `AbortSignal` with a 60 s timeout, defaulted `Content-Type` to JSON and
//! unwrapped the `{error:{status,code,message}}` envelope into an `ApiError`.
//! This module is the same contract for the authenticated shell: session and
//! login, logout, folder metadata/children, file mutations and trash actions.
//!
//! The request/response bodies are the shared types from
//! [`revaro_core::api::auth`], so a field rename on the server breaks this build
//! rather than failing at runtime. The error envelope is likewise the shared
//! [`ErrorEnvelope`], which is how the login form can branch on the
//! `totp_required` code without string-matching a message.
//!
//! Raw upload bytes intentionally use the browser XHR API in
//! [`crate::components::uploads`], where progress and cancellation are
//! available; this module owns the JSON session and commit calls. Only compiled
//! for wasm; the pure parts of the client live in `crate::logic`.

use gloo_net::http::Request;
use revaro_core::api::auth::{LoginRequest, Session};
use revaro_core::api::book::{Info as BookInfo, Progress as BookProgress, SaveProgressRequest};
use revaro_core::api::files::{
    Children, CopyFileRequest, CreateDirectoryRequest, FileDetail, PatchFileRequest, Trash,
};
use revaro_core::api::media::{AudioMedia, VideoMedia};
use revaro_core::api::tasks::{TaskInputRequest, TaskList};
use revaro_core::api::uploads::{
    CompleteUploadRequest, CreateUpload, CreateUploadRequest, RecordUploadPartRequest,
    UploadPartsRequest, UploadPartsResponse, UploadStatus,
};
use revaro_core::model::MediaProgress;
use revaro_core::reader::FlowManifest;
use revaro_core::{ErrorCode, ErrorEnvelope};

/// A failed request to an authenticated JSON endpoint.
///
/// Keeping the status and optional shared error code alongside the message
/// lets the view distinguish an expired session from a server-side failure
/// without matching translated text. The browser only receives the server's
/// envelope; transport and malformed-response failures are represented with
/// status `0`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestError {
    /// HTTP status, or `0` when no usable HTTP response arrived.
    pub status: u16,
    /// Machine-readable code from the shared error envelope, when present.
    pub code: Option<ErrorCode>,
    /// Human-readable failure detail.
    pub message: String,
}

impl RequestError {
    /// True when the browser should return to the login screen.
    #[must_use]
    pub fn is_unauthorized(&self) -> bool {
        self.status == 401
    }
}

/// A rejected login, decoded from the shared error envelope when possible.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginError {
    /// HTTP status the server answered with.
    pub status: u16,
    /// Machine-readable code, when the server sent one.
    pub code: Option<ErrorCode>,
    /// Human-readable message: the envelope's, or a generic fallback.
    pub message: String,
}

impl LoginError {
    /// True when the server is asking for a TOTP or recovery code.
    ///
    /// `useAuthSession.submitLogin` treated this code as "show the second field
    /// and keep going", not as a final failure, so the distinction has to
    /// survive deserialization.
    #[must_use]
    pub fn requires_second_factor(&self) -> bool {
        self.code.as_ref() == Some(&ErrorCode::TOTP_REQUIRED)
    }

    /// True when a supplied second factor was rejected.
    #[must_use]
    pub fn second_factor_rejected(&self) -> bool {
        self.code.as_ref() == Some(&ErrorCode::INVALID_SECOND_FACTOR)
    }
}

/// Fetch the current session, or `None` when the visitor is not signed in.
///
/// A 401 is the ordinary "logged out" answer, not an error worth surfacing:
/// `checkSession` in the Vue app swallowed every failure and showed the login
/// page, and the shell keeps that behaviour.
pub async fn fetch_session() -> Option<Session> {
    let response = Request::get("/api/auth/me").send().await.ok()?;
    if !response.ok() {
        return None;
    }
    response.json::<Session>().await.ok()
}

/// Fetch a directory's metadata and breadcrumb trail.
pub async fn fetch_file(id: &str) -> Result<FileDetail, RequestError> {
    get_json(&format!("/api/files/{id}")).await
}

/// Fetch the live children and aggregate counters of a directory.
pub async fn fetch_children(id: &str) -> Result<Children, RequestError> {
    get_json(&format!("/api/files/{id}/children")).await
}

/// Fetch the top-level entries in the trash.
pub async fn fetch_trash() -> Result<Trash, RequestError> {
    get_json("/api/trash").await
}

/// Fetch the optional chapter and cover metadata for an audio file.
pub async fn fetch_audio_media(id: &str) -> Result<AudioMedia, RequestError> {
    get_json(&format!("/api/files/{id}/audio")).await
}

/// Fetch the subtitle tracks advertised for a video file.
pub async fn fetch_video_media(id: &str) -> Result<VideoMedia, RequestError> {
    get_json(&format!("/api/files/{id}/video")).await
}

/// Fetch the parsed title and table of contents for a book.
pub async fn fetch_book(id: &str) -> Result<BookInfo, RequestError> {
    get_json(&format!("/api/files/{id}/book")).await
}

/// Fetch the durable reading anchor, if one exists.
pub async fn fetch_book_progress(id: &str) -> Result<BookProgress, RequestError> {
    get_json(&format!("/api/files/{id}/book/progress")).await
}

/// Save a durable reading anchor.
pub async fn save_book_progress(
    id: &str,
    progress: &SaveProgressRequest,
) -> Result<(), RequestError> {
    let request = Request::put(&format!("/api/files/{id}/book/progress"))
        .json(progress)
        .map_err(|error| request_transport(error.to_string()))?;
    send_empty(request).await
}

/// Fetch the no-cache flow manifest used to lay out the book.
pub async fn fetch_book_flow(id: &str) -> Result<FlowManifest, RequestError> {
    get_json(&format!("/api/files/{id}/book/flow")).await
}

/// Fetch one sanitized flow chunk as HTML.
pub async fn fetch_book_chunk(id: &str, index: i32) -> Result<String, RequestError> {
    let response = Request::get(&format!("/api/files/{id}/book/flow/chunks/{index}"))
        .send()
        .await
        .map_err(|error| request_transport(error.to_string()))?;
    if response.ok() {
        return response
            .text()
            .await
            .map_err(|error| request_transport(error.to_string()));
    }
    Err(decode_request_error(response).await)
}

/// Fetch a saved playback position. The server returns zeroes for a first play.
pub async fn fetch_media_progress(id: &str) -> Result<MediaProgress, RequestError> {
    get_json(&format!("/api/files/{id}/media/progress")).await
}

/// Save a playback position and the duration known by the browser.
pub async fn save_media_progress(
    id: &str,
    progress: &MediaProgress,
) -> Result<MediaProgress, RequestError> {
    let request = Request::put(&format!("/api/files/{id}/media/progress"))
        .json(progress)
        .map_err(|error| request_transport(error.to_string()))?;
    send_json(request).await
}

/// Fetch the durable tasks shown by the task centre.
pub async fn fetch_tasks() -> Result<TaskList, RequestError> {
    get_json("/api/tasks").await
}

/// Ask a running or waiting task to stop.
pub async fn cancel_task(id: &str) -> Result<(), RequestError> {
    let request = Request::post(&format!("/api/tasks/{id}/cancel"))
        .build()
        .map_err(|error| request_transport(error.to_string()))?;
    send_empty(request).await
}

/// Retry a failed task on the server's next worker pass.
pub async fn retry_task(id: &str) -> Result<(), RequestError> {
    let request = Request::post(&format!("/api/tasks/{id}/retry"))
        .build()
        .map_err(|error| request_transport(error.to_string()))?;
    send_empty(request).await
}

/// Supply input to a task waiting for it, currently an archive password.
pub async fn submit_task_input(id: &str, request: &TaskInputRequest) -> Result<(), RequestError> {
    let request = Request::post(&format!("/api/tasks/{id}/input"))
        .json(request)
        .map_err(|error| request_transport(error.to_string()))?;
    send_empty(request).await
}

/// Remove one terminal task notification from the durable task history.
pub async fn delete_task(id: &str) -> Result<(), RequestError> {
    let request = Request::delete(&format!("/api/tasks/{id}"))
        .build()
        .map_err(|error| request_transport(error.to_string()))?;
    send_empty(request).await
}

/// Start or resume a browser upload session.
pub async fn create_upload(request: &CreateUploadRequest) -> Result<CreateUpload, RequestError> {
    let request = Request::post("/api/uploads")
        .json(request)
        .map_err(|error| request_transport(error.to_string()))?;
    send_json(request).await
}

/// Read the server-side state of an upload session for local resume.
pub async fn fetch_upload(id: &str) -> Result<UploadStatus, RequestError> {
    get_json(&format!("/api/uploads/{id}")).await
}

/// Request one-use URLs for a batch of multipart parts.
pub async fn fetch_upload_parts(
    id: &str,
    request: &UploadPartsRequest,
) -> Result<UploadPartsResponse, RequestError> {
    let request = Request::post(&format!("/api/uploads/{id}/parts"))
        .json(request)
        .map_err(|error| request_transport(error.to_string()))?;
    send_json(request).await
}

/// Acknowledge the ETag and exact size of one stored multipart part.
pub async fn record_upload_part(
    id: &str,
    part_number: i32,
    request: &RecordUploadPartRequest,
) -> Result<(), RequestError> {
    let request = Request::put(&format!("/api/uploads/{id}/parts/{part_number}"))
        .json(request)
        .map_err(|error| request_transport(error.to_string()))?;
    send_empty(request).await
}

/// Commit the upload transaction. The signal is deliberately supplied by the
/// caller so cancelling a slow commit does not leave a request running after
/// the task has already been removed from the queue.
pub async fn complete_upload(
    id: &str,
    request: &CompleteUploadRequest,
    signal: Option<&web_sys::AbortSignal>,
) -> Result<revaro_core::model::File, RequestError> {
    let request = Request::post(&format!("/api/uploads/{id}/complete"))
        .abort_signal(signal)
        .json(request)
        .map_err(|error| request_transport(error.to_string()))?;
    send_json(request).await
}

/// Abandon a session and remove its pending file row and staging bytes.
pub async fn abort_upload(id: &str) -> Result<(), RequestError> {
    let request = Request::delete(&format!("/api/uploads/{id}"))
        .build()
        .map_err(|error| request_transport(error.to_string()))?;
    send_empty(request).await
}

/// Create a directory below a live directory.
pub async fn create_directory(
    request: &CreateDirectoryRequest,
) -> Result<revaro_core::model::File, RequestError> {
    let request = Request::post("/api/directories")
        .json(request)
        .map_err(|error| request_transport(error.to_string()))?;
    send_json(request).await
}

/// Rename or move one live file or directory.
pub async fn patch_file(
    id: &str,
    request: &PatchFileRequest,
) -> Result<revaro_core::model::File, RequestError> {
    let request = Request::patch(&format!("/api/files/{id}"))
        .json(request)
        .map_err(|error| request_transport(error.to_string()))?;
    send_json(request).await
}

/// Copy a live file or directory below another live directory.
pub async fn copy_file(
    id: &str,
    request: &CopyFileRequest,
) -> Result<revaro_core::model::File, RequestError> {
    let request = Request::post(&format!("/api/files/{id}/copy"))
        .json(request)
        .map_err(|error| request_transport(error.to_string()))?;
    send_json(request).await
}

/// Move a live item into the trash.
pub async fn delete_file(id: &str) -> Result<(), RequestError> {
    let request = Request::delete(&format!("/api/files/{id}"))
        .build()
        .map_err(|error| request_transport(error.to_string()))?;
    send_empty(request).await
}

/// Restore one root item from the trash.
pub async fn restore_trash(id: &str) -> Result<(), RequestError> {
    let request = Request::post(&format!("/api/trash/{id}/restore"))
        .build()
        .map_err(|error| request_transport(error.to_string()))?;
    send_empty(request).await
}

/// Permanently remove one root item from the trash.
pub async fn purge_trash(id: &str) -> Result<(), RequestError> {
    let request = Request::delete(&format!("/api/trash/{id}"))
        .build()
        .map_err(|error| request_transport(error.to_string()))?;
    send_empty(request).await
}

/// Permanently remove every item currently in the trash.
pub async fn empty_trash() -> Result<(), RequestError> {
    let request = Request::delete("/api/trash")
        .build()
        .map_err(|error| request_transport(error.to_string()))?;
    send_empty(request).await
}

/// Sign in, mapping a non-2xx answer to a decoded [`LoginError`].
pub async fn login(request: &LoginRequest) -> Result<Session, LoginError> {
    let sent = Request::post("/api/auth/login")
        .json(request)
        .map_err(|error| transport(error.to_string()))?
        .send()
        .await
        .map_err(|error| transport(error.to_string()))?;
    let status = sent.status();
    if sent.ok() {
        return sent
            .json::<Session>()
            .await
            .map_err(|error| transport(error.to_string()));
    }
    // The envelope's message is the user-facing text for everything except the
    // two codes the form handles itself.
    match sent.json::<ErrorEnvelope>().await {
        Ok(envelope) => Err(LoginError {
            status,
            code: envelope.error.code,
            message: envelope.error.message,
        }),
        Err(_) => Err(LoginError {
            status,
            code: None,
            message: format!("请求失败 ({status})"),
        }),
    }
}

/// End the session. Failures are ignored: the shell clears local state anyway.
pub async fn logout() {
    let _ = Request::post("/api/auth/logout").send().await;
}

/// A transport failure (offline, DNS, malformed body) carries no HTTP status.
fn transport(message: String) -> LoginError {
    LoginError {
        status: 0,
        code: None,
        message,
    }
}

/// Fetch and decode a JSON endpoint using the shared error envelope.
async fn get_json<T>(path: &str) -> Result<T, RequestError>
where
    T: serde::de::DeserializeOwned,
{
    let response = Request::get(path)
        .send()
        .await
        .map_err(|error| request_transport(error.to_string()))?;
    if response.ok() {
        return response
            .json::<T>()
            .await
            .map_err(|error| request_transport(error.to_string()));
    }
    let status = response.status();
    match response.json::<ErrorEnvelope>().await {
        Ok(envelope) => Err(RequestError {
            status,
            code: envelope.error.code,
            message: envelope.error.message,
        }),
        Err(_) => Err(RequestError {
            status,
            code: None,
            message: format!("请求失败 ({status})"),
        }),
    }
}

/// Send an already encoded JSON request and decode its success body.
async fn send_json<T>(request: Request) -> Result<T, RequestError>
where
    T: serde::de::DeserializeOwned,
{
    let response = request
        .send()
        .await
        .map_err(|error| request_transport(error.to_string()))?;
    if response.ok() {
        return response
            .json::<T>()
            .await
            .map_err(|error| request_transport(error.to_string()));
    }
    Err(decode_request_error(response).await)
}

/// Send a request whose successful response has no body.
async fn send_empty(request: Request) -> Result<(), RequestError> {
    let response = request
        .send()
        .await
        .map_err(|error| request_transport(error.to_string()))?;
    if response.ok() {
        return Ok(());
    }
    Err(decode_request_error(response).await)
}

/// Decode the shared error envelope from a non-successful response.
async fn decode_request_error(response: gloo_net::http::Response) -> RequestError {
    let status = response.status();
    match response.json::<ErrorEnvelope>().await {
        Ok(envelope) => RequestError {
            status,
            code: envelope.error.code,
            message: envelope.error.message,
        },
        Err(_) => RequestError {
            status,
            code: None,
            message: format!("请求失败 ({status})"),
        },
    }
}

fn request_transport(message: String) -> RequestError {
    RequestError {
        status: 0,
        code: None,
        message,
    }
}
