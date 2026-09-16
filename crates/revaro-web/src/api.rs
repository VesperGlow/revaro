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

use gloo_net::http::{Request, RequestBuilder};
use js_sys::{Object, Reflect};
use revaro_core::api::auth::{
    AvatarRequest, ChangePasswordRequest, ChangeUsernameRequest, LoginRequest, PasswordCodeRequest,
    PasswordRequest, Session, TotpRecovery, TotpSetup, TotpStatus,
};
use revaro_core::api::book::{Info as BookInfo, Progress as BookProgress, SaveProgressRequest};
use revaro_core::api::files::{
    ChildItems, Children, CopyFileRequest, CreateDirectoryRequest, CreateDocumentRequest,
    DocumentContent, FileDetail, PatchFileRequest, Trash, UpdateDocumentRequest,
};
use revaro_core::api::library::LibraryAll;
use revaro_core::api::media::{AudioMedia, VideoMedia};
use revaro_core::api::share::Status as ShareStatus;
use revaro_core::api::tasks::{TaskInputRequest, TaskList};
use revaro_core::api::uploads::{
    CompleteUploadRequest, CreateUpload, CreateUploadRequest, RecordUploadPartRequest,
    UploadPartsRequest, UploadPartsResponse, UploadStatus,
};
use revaro_core::api::{BatchDownloadRequest, BatchDownloadTicket};
use revaro_core::model::MediaProgress;
use revaro_core::reader::FlowManifest;
use revaro_core::{ErrorCode, ErrorEnvelope};
use wasm_bindgen::JsValue;
use web_sys::{AbortSignal, Request as BrowserRequest, RequestCredentials, RequestInit};

const API_TIMEOUT_MS: u32 = 60_000;
const READER_FLOW_TIMEOUT_MS: u32 = 120_000;

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
    let response = api_request(Request::get("/api/auth/me"))
        .send()
        .await
        .ok()?;
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

/// Fetch only the directory entries for callers whose historical contract did
/// not consume aggregate byte/file counters.
pub async fn fetch_child_items(id: &str) -> Result<Vec<revaro_core::model::File>, RequestError> {
    let response: ChildItems = get_json(&format!("/api/files/{id}/children")).await?;
    Ok(response.items)
}

/// Fetch the UTF-8 content and optimistic-concurrency ETag of an editable file.
pub async fn fetch_document(id: &str) -> Result<DocumentContent, RequestError> {
    get_json(&format!("/api/files/{id}/content")).await
}

/// Create a new editable document in a directory.
pub async fn create_document(
    request: &CreateDocumentRequest,
) -> Result<revaro_core::model::File, RequestError> {
    let request = api_request(Request::post("/api/documents"))
        .json(request)
        .map_err(|error| request_transport(error.to_string()))?;
    send_json(request).await
}

/// Save an editable document with its last observed ETag.
pub async fn update_document(
    id: &str,
    request: &UpdateDocumentRequest,
) -> Result<revaro_core::model::File, RequestError> {
    let request = api_request(Request::put(&format!("/api/files/{id}/content")))
        .json(request)
        .map_err(|error| request_transport(error.to_string()))?;
    send_json(request).await
}

/// Fetch the top-level entries in the trash.
pub async fn fetch_trash() -> Result<Trash, RequestError> {
    get_json("/api/trash").await
}

/// Fetch every media bucket and its sidebar counts in one request.
pub async fn fetch_library_all() -> Result<LibraryAll, RequestError> {
    get_json("/api/library/all").await
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
    let request = api_request(Request::put(&format!("/api/files/{id}/book/progress")))
        .json(progress)
        .map_err(|error| request_transport(error.to_string()))?;
    send_empty(request).await
}

/// Fetch the no-cache flow manifest used to lay out the book.
pub async fn fetch_book_flow(id: &str) -> Result<FlowManifest, RequestError> {
    get_json_with_timeout(
        &format!("/api/files/{id}/book/flow"),
        READER_FLOW_TIMEOUT_MS,
    )
    .await
}

/// Fetch one sanitized flow chunk as HTML.
pub async fn fetch_book_chunk(id: &str, index: i32) -> Result<String, RequestError> {
    // The reference reader deliberately used a raw same-origin fetch for
    // chunks, so a large chapter is not cut off by the generic 60s JSON timer.
    let response = Request::get(&format!("/api/files/{id}/book/flow/chunks/{index}"))
        .credentials(RequestCredentials::SameOrigin)
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
    let request = api_request(Request::put(&format!("/api/files/{id}/media/progress")))
        .json(progress)
        .map_err(|error| request_transport(error.to_string()))?;
    send_json(request).await
}

/// Save the final playback position while a media preview is being removed.
///
/// The reference Vue player deliberately used a fire-and-forget same-origin
/// `fetch` with `keepalive: true` during unmount so the browser can finish the
/// small request while the preview and its ordinary request timers disappear.
/// `web-sys` does not expose that dictionary member in the pinned version, so
/// set it through the DOM dictionary object before constructing the request.
pub fn save_media_progress_keepalive(id: &str, progress: &MediaProgress) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let Ok(body) = serde_json::to_string(progress) else {
        return;
    };

    let init = RequestInit::new();
    init.set_method("PUT");
    init.set_credentials(RequestCredentials::SameOrigin);
    init.set_body(&JsValue::from_str(&body));
    let _ = Reflect::set(
        init.as_ref(),
        &JsValue::from_str("keepalive"),
        &JsValue::TRUE,
    );

    let headers = Object::new();
    let _ = Reflect::set(
        headers.as_ref(),
        &JsValue::from_str("Content-Type"),
        &JsValue::from_str("application/json"),
    );
    init.set_headers(headers.as_ref());

    let Ok(request) =
        BrowserRequest::new_with_str_and_init(&format!("/api/files/{id}/media/progress"), &init)
    else {
        return;
    };
    let _ = window.fetch_with_request(&request);
}

/// Fetch the durable tasks shown by the task centre.
pub async fn fetch_tasks() -> Result<TaskList, RequestError> {
    get_json("/api/tasks").await
}

/// Ask a running or waiting task to stop.
pub async fn cancel_task(id: &str) -> Result<(), RequestError> {
    let request = api_request(Request::post(&format!("/api/tasks/{id}/cancel")))
        .build()
        .map_err(|error| request_transport(error.to_string()))?;
    send_empty(request).await
}

/// Retry a failed task on the server's next worker pass.
pub async fn retry_task(id: &str) -> Result<(), RequestError> {
    let request = api_request(Request::post(&format!("/api/tasks/{id}/retry")))
        .build()
        .map_err(|error| request_transport(error.to_string()))?;
    send_empty(request).await
}

/// Supply input to a task waiting for it, currently an archive password.
pub async fn submit_task_input(id: &str, request: &TaskInputRequest) -> Result<(), RequestError> {
    let request = api_request(Request::post(&format!("/api/tasks/{id}/input")))
        .json(request)
        .map_err(|error| request_transport(error.to_string()))?;
    send_empty(request).await
}

/// Remove one terminal task notification from the durable task history.
pub async fn delete_task(id: &str) -> Result<(), RequestError> {
    let request = api_request(Request::delete(&format!("/api/tasks/{id}")))
        .build()
        .map_err(|error| request_transport(error.to_string()))?;
    send_empty(request).await
}

/// Start or resume a browser upload session.
pub async fn create_upload(request: &CreateUploadRequest) -> Result<CreateUpload, RequestError> {
    let request = api_request(Request::post("/api/uploads"))
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
    let request = api_request(Request::post(&format!("/api/uploads/{id}/parts")))
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
    let request = api_request(Request::put(&format!(
        "/api/uploads/{id}/parts/{part_number}"
    )))
    .json(request)
    .map_err(|error| request_transport(error.to_string()))?;
    send_empty(request).await
}

/// Commit the upload transaction. The historical client disabled its generic
/// 60-second timeout for this verification call and relied on the caller's
/// abort signal instead, so a large local-disk hash is not cut off early. It
/// also ignored the successful response body, so only the HTTP result is
/// consumed here.
pub async fn complete_upload(
    id: &str,
    request: &CompleteUploadRequest,
    signal: Option<&web_sys::AbortSignal>,
) -> Result<(), RequestError> {
    let request = api_request_timeout(
        Request::post(&format!("/api/uploads/{id}/complete")),
        0,
        signal,
    )
    .json(request)
    .map_err(|error| request_transport(error.to_string()))?;
    send_empty(request).await
}

/// Abandon a session and remove its pending file row and staging bytes.
pub async fn abort_upload(id: &str) -> Result<(), RequestError> {
    let request = api_request(Request::delete(&format!("/api/uploads/{id}")))
        .build()
        .map_err(|error| request_transport(error.to_string()))?;
    send_empty(request).await
}

/// Create a directory below a live directory.
pub async fn create_directory(
    request: &CreateDirectoryRequest,
) -> Result<revaro_core::model::File, RequestError> {
    let request = api_request(Request::post("/api/directories"))
        .json(request)
        .map_err(|error| request_transport(error.to_string()))?;
    send_json(request).await
}

/// Create a directory from the foreground file-browser action.
///
/// The historical action ignored the successful response body. Upload
/// directory creation still uses [`create_directory`] because it needs the
/// returned folder record to build its local path tree.
pub async fn create_directory_action(request: &CreateDirectoryRequest) -> Result<(), RequestError> {
    let request = api_request(Request::post("/api/directories"))
        .json(request)
        .map_err(|error| request_transport(error.to_string()))?;
    send_empty(request).await
}

/// Apply a file-browser rename or move.
///
/// The historical foreground action only consumed the success status, so the
/// response body is intentionally ignored here.
pub async fn patch_file(id: &str, request: &PatchFileRequest) -> Result<(), RequestError> {
    let request = api_request(Request::patch(&format!("/api/files/{id}")))
        .json(request)
        .map_err(|error| request_transport(error.to_string()))?;
    send_empty(request).await
}

/// Copy a file-browser item while consuming only the success status.
pub async fn copy_file(id: &str, request: &CopyFileRequest) -> Result<(), RequestError> {
    let request = api_request(Request::post(&format!("/api/files/{id}/copy")))
        .json(request)
        .map_err(|error| request_transport(error.to_string()))?;
    send_empty(request).await
}

/// Move a live item into the trash.
pub async fn delete_file(id: &str) -> Result<(), RequestError> {
    let request = api_request(Request::delete(&format!("/api/files/{id}")))
        .build()
        .map_err(|error| request_transport(error.to_string()))?;
    send_empty(request).await
}

/// Restore one root item from the trash.
pub async fn restore_trash(id: &str) -> Result<(), RequestError> {
    let request = api_request(Request::post(&format!("/api/trash/{id}/restore")))
        .build()
        .map_err(|error| request_transport(error.to_string()))?;
    send_empty(request).await
}

/// Permanently remove one root item from the trash.
pub async fn purge_trash(id: &str) -> Result<(), RequestError> {
    let request = api_request(Request::delete(&format!("/api/trash/{id}")))
        .build()
        .map_err(|error| request_transport(error.to_string()))?;
    send_empty(request).await
}

/// Permanently remove every item currently in the trash.
pub async fn empty_trash() -> Result<(), RequestError> {
    let request = api_request(Request::delete("/api/trash"))
        .build()
        .map_err(|error| request_transport(error.to_string()))?;
    send_empty(request).await
}

/// Start the background extraction job for a ready archive.
pub async fn extract_archive(id: &str) -> Result<(), RequestError> {
    let request = api_request(Request::post(&format!("/api/files/{id}/extract")))
        .build()
        .map_err(|error| request_transport(error.to_string()))?;
    send_empty(request).await
}

/// Read the current public-share state of a file.
pub async fn fetch_share(id: &str) -> Result<ShareStatus, RequestError> {
    get_json(&format!("/api/files/{id}/share")).await
}

/// Create or replace a public-share link.
pub async fn create_share(id: &str) -> Result<ShareStatus, RequestError> {
    let request = api_request(Request::post(&format!("/api/files/{id}/share")))
        .build()
        .map_err(|error| request_transport(error.to_string()))?;
    send_json(request).await
}

/// Revoke a public-share link.
pub async fn revoke_share(id: &str) -> Result<(), RequestError> {
    let request = api_request(Request::delete(&format!("/api/files/{id}/share")))
        .build()
        .map_err(|error| request_transport(error.to_string()))?;
    send_empty(request).await
}

/// Reserve the one-use ZIP download ticket for multiple files.
pub async fn prepare_batch_download(ids: Vec<String>) -> Result<BatchDownloadTicket, RequestError> {
    let request = api_request(Request::post("/api/files/batch-download/prepare"))
        .json(&BatchDownloadRequest { ids })
        .map_err(|error| request_transport(error.to_string()))?;
    // The reference client validates the token after decoding the response:
    // a successful `{}`/`{"token":null}` response becomes the explicit
    // "batch download preparation failed" feedback instead of a JSON parser
    // error. Keep the wire type strict for callers while preserving that
    // observable branch.
    #[derive(serde::Deserialize)]
    struct Response {
        token: Option<String>,
    }
    let response: Response = send_json(request).await?;
    let Some(token) = response.token.filter(|token| !token.is_empty()) else {
        return Err(RequestError {
            status: 0,
            code: None,
            message: "批量下载准备失败".to_owned(),
        });
    };
    Ok(BatchDownloadTicket { token })
}

/// Sign in, mapping a non-2xx answer to a decoded [`LoginError`].
pub async fn login(request: &LoginRequest) -> Result<Session, LoginError> {
    let sent = api_request(Request::post("/api/auth/login"))
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
    let _ = api_request(Request::post("/api/auth/logout")).send().await;
}

/// Change the signed-in user's login name.
pub async fn change_username(request: &ChangeUsernameRequest) -> Result<(), RequestError> {
    let request = api_request(Request::patch("/api/profile/username"))
        .json(request)
        .map_err(|error| request_transport(error.to_string()))?;
    send_empty(request).await
}

/// Change the signed-in user's password.
pub async fn change_password(request: &ChangePasswordRequest) -> Result<(), RequestError> {
    let request = api_request(Request::patch("/api/auth/password"))
        .json(request)
        .map_err(|error| request_transport(error.to_string()))?;
    send_empty(request).await
}

/// Read the administrator's TOTP state.
pub async fn fetch_totp_status() -> Result<TotpStatus, RequestError> {
    get_json("/api/auth/totp").await
}

/// Begin TOTP enrollment and return the secret and QR data URL.
pub async fn begin_totp_setup(request: &PasswordRequest) -> Result<TotpSetup, RequestError> {
    let request = api_request(Request::post("/api/auth/totp/setup"))
        .json(request)
        .map_err(|error| request_transport(error.to_string()))?;
    send_json(request).await
}

/// Confirm TOTP enrollment and receive one-time recovery codes.
pub async fn enable_totp(request: &PasswordCodeRequest) -> Result<TotpRecovery, RequestError> {
    let request = api_request(Request::post("/api/auth/totp/enable"))
        .json(request)
        .map_err(|error| request_transport(error.to_string()))?;
    send_json(request).await
}

/// Generate a replacement set of recovery codes.
pub async fn regenerate_totp_recovery_codes(
    request: &PasswordCodeRequest,
) -> Result<TotpRecovery, RequestError> {
    let request = api_request(Request::post("/api/auth/totp/recovery-codes"))
        .json(request)
        .map_err(|error| request_transport(error.to_string()))?;
    send_json(request).await
}

/// Disable TOTP after re-verifying the password and second factor.
pub async fn disable_totp(request: &PasswordCodeRequest) -> Result<(), RequestError> {
    let request = api_request(Request::delete("/api/auth/totp"))
        .json(request)
        .map_err(|error| request_transport(error.to_string()))?;
    send_empty(request).await
}

/// Store a validated avatar data URL.
pub async fn update_avatar(request: &AvatarRequest) -> Result<(), RequestError> {
    let request = api_request(Request::put("/api/profile/avatar"))
        .json(request)
        .map_err(|error| request_transport(error.to_string()))?;
    send_empty(request).await
}

/// Remove the stored avatar.
pub async fn delete_avatar() -> Result<(), RequestError> {
    let request = api_request(Request::delete("/api/profile/avatar"))
        .build()
        .map_err(|error| request_transport(error.to_string()))?;
    send_empty(request).await
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
    let request = api_request(Request::get(path))
        .build()
        .map_err(|error| request_transport(error.to_string()))?;
    get_json_with_request(request).await
}

async fn get_json_with_timeout<T>(path: &str, timeout_ms: u32) -> Result<T, RequestError>
where
    T: serde::de::DeserializeOwned,
{
    let request = api_request_timeout(Request::get(path), timeout_ms, None)
        .build()
        .map_err(|error| request_transport(error.to_string()))?;
    get_json_with_request(request).await
}

async fn get_json_with_request<T>(request: Request) -> Result<T, RequestError>
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
        // `fetch()` rejects with an Error whose browser-facing message is
        // `Failed to fetch`; gloo's Display implementation includes the JS
        // constructor name (`TypeError: ...`). The Vue client surfaced only
        // `error.message`, so remove that runtime prefix before it reaches a
        // user-facing toast.
        message: message
            .strip_prefix("TypeError: ")
            .unwrap_or(&message)
            .to_owned(),
    }
}

/// Apply the old Vue client's same-origin cookie policy and cancellation
/// boundary to every JSON request. A zero timeout intentionally means
/// "caller-controlled/no timeout", as used by upload verification.
///
/// `AbortSignal::any` preserves the upload controller's explicit cancellation
/// while still preventing a hung commit from remaining pending forever. The
/// timeout signal is owned by the browser request after this builder is
/// consumed, so it is safe for the local Rust value to be dropped here.
fn api_request(builder: RequestBuilder) -> RequestBuilder {
    api_request_with_timeout(builder, API_TIMEOUT_MS, None)
}

fn api_request_timeout(
    builder: RequestBuilder,
    timeout_ms: u32,
    caller_signal: Option<&AbortSignal>,
) -> RequestBuilder {
    api_request_with_timeout(builder, timeout_ms, caller_signal)
}

fn api_request_with_timeout(
    builder: RequestBuilder,
    timeout_ms: u32,
    caller_signal: Option<&AbortSignal>,
) -> RequestBuilder {
    let builder = builder.credentials(RequestCredentials::SameOrigin);
    if timeout_ms == 0 {
        return if let Some(signal) = caller_signal {
            builder.abort_signal(Some(signal))
        } else {
            builder
        };
    }
    let timeout_signal = AbortSignal::timeout_with_u32(timeout_ms);
    let signal = if let Some(caller_signal) = caller_signal {
        let signals = js_sys::Array::new();
        signals.push(caller_signal);
        signals.push(&timeout_signal);
        AbortSignal::any(&signals.into())
    } else {
        timeout_signal
    };
    builder.abort_signal(Some(&signal))
}
