//! Typed API operations. The shared document/worker transport owns request
//! progress deadlines, safe retries and recovery for every file consumer.
//! Mutations without an idempotency contract are never blindly replayed.

use gloo_net::http::{Request, RequestBuilder};
use js_sys::{Object, Reflect};
use revaro_core::api::auth::{
    AvatarRequest, ChangePasswordRequest, ChangeUsernameRequest, LoginRequest, PasswordCodeRequest,
    PasswordRequest, Session, TotpRecovery, TotpSetup, TotpStatus,
};
use revaro_core::api::book::{Info as BookInfo, Progress as BookProgress, SaveProgressRequest};
use revaro_core::api::files::{
    ChildItems, CopyFileRequest, CreateDirectoryRequest, CreateDocumentRequest, DocumentContent,
    FileDetail, PatchFileRequest, Trash, UpdateDocumentRequest,
};
use revaro_core::api::media::AudioMedia;
use revaro_core::api::share::Status as ShareStatus;
use revaro_core::api::uploads::{
    CompleteUploadRequest, CreateUpload, CreateUploadRequest, UploadStatus,
};
use revaro_core::api::{BatchDownloadRequest, BatchDownloadTicket};
use revaro_core::model::MediaProgress;
use revaro_core::reader::FlowManifest;
use revaro_core::{ErrorCode, ErrorEnvelope};
use wasm_bindgen::JsValue;
use web_sys::{AbortSignal, Request as BrowserRequest, RequestCredentials, RequestInit};

// The shared transport owns stall deadlines and retries; a second timer
// would prematurely abort a healthy reconnecting transfer.
const API_TIMEOUT_MS: u32 = 0;
const READER_FLOW_TIMEOUT_MS: u32 = 0;

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
/// Only 401 means signed out. A temporary network/server error must preserve
/// the cookie and offer a retry instead of showing an unrelated login form.
pub async fn fetch_session() -> Result<Option<Session>, RequestError> {
    let response = api_request(Request::get("/api/auth/me"))
        .send()
        .await
        .map_err(|error| request_transport(error.to_string()))?;
    if response.status() == 401 {
        return Ok(None);
    }
    if !response.ok() {
        return Err(decode_request_error(response).await);
    }
    response
        .json::<Session>()
        .await
        .map(Some)
        .map_err(|error| request_transport(error.to_string()))
}

/// Fetch a directory's metadata and breadcrumb trail.
pub async fn fetch_file(id: &str) -> Result<FileDetail, RequestError> {
    get_json(&format!("/api/files/{id}")).await
}

/// Fetch the live children and aggregate counters of a directory.
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

/// Fetch the optional chapter and cover metadata for an audio file.
pub async fn fetch_audio_media(id: &str) -> Result<AudioMedia, RequestError> {
    get_json(&format!("/api/files/{id}/audio")).await
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
    // The shared transport intercepts this request, including body stalls.
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

/// Fetch the cached server-side video probe for library duration labels.
pub async fn fetch_video_media(id: &str) -> Result<revaro_core::media::MediaProbe, RequestError> {
    get_json(&format!("/api/files/{id}/video")).await
}

/// Fetch a saved playback position. The server returns zeroes for a first play.
pub async fn fetch_media_progress(id: &str) -> Result<MediaProgress, RequestError> {
    get_json(&format!("/api/files/{id}/media/progress")).await
}

pub async fn fetch_listening_session()
-> Result<revaro_core::progress::ListeningSession, RequestError> {
    get_json("/api/listening/session").await
}

/// Save the final playback position while a media preview is being removed.
///
/// The reference Vue player deliberately used a fire-and-forget same-origin
/// `fetch` with `keepalive: true` during unmount so the browser can finish the
/// small request while the preview and its ordinary request timers disappear.
/// `web-sys` does not expose that dictionary member in the pinned version, so
/// set it through the DOM dictionary object before constructing the request.
pub async fn save_media_progress_keepalive(
    id: &str,
    progress: &revaro_core::progress::SaveMediaProgress,
) -> Result<MediaProgress, RequestError> {
    put_progress_keepalive(&format!("/api/files/{id}/media/progress"), progress).await
}

pub async fn save_listening_session_keepalive(
    write: &revaro_core::progress::ListeningWrite,
) -> Result<revaro_core::progress::ListeningSession, RequestError> {
    put_progress_keepalive("/api/listening/session", write).await
}

async fn put_progress_keepalive<T: serde::de::DeserializeOwned>(
    path: &str,
    progress: &impl serde::Serialize,
) -> Result<T, RequestError> {
    let window = web_sys::window().ok_or_else(|| request_transport("window unavailable".into()))?;
    let body = serde_json::to_string(progress).map_err(|e| request_transport(e.to_string()))?;
    let init = RequestInit::new();
    init.set_method("PUT");
    init.set_credentials(RequestCredentials::SameOrigin);
    init.set_body(&JsValue::from_str(&body));
    let _ = Reflect::set(
        init.as_ref(),
        &JsValue::from_str("keepalive"),
        &JsValue::TRUE,
    );
    let request = BrowserRequest::new_with_str_and_init(path, &init)
        .map_err(|e| request_transport(format!("{e:?}")))?;
    request
        .headers()
        .set("Content-Type", "application/json")
        .map_err(|e| request_transport(format!("{e:?}")))?;
    use wasm_bindgen::JsCast as _;
    let response: web_sys::Response =
        wasm_bindgen_futures::JsFuture::from(window.fetch_with_request(&request))
            .await
            .map_err(|e| request_transport(format!("{e:?}")))?
            .unchecked_into();
    if !response.ok() {
        return Err(RequestError {
            status: response.status(),
            code: None,
            message: "播放进度同步失败".into(),
        });
    }
    let text = wasm_bindgen_futures::JsFuture::from(
        response
            .text()
            .map_err(|e| request_transport(format!("{e:?}")))?,
    )
    .await
    .map_err(|e| request_transport(format!("{e:?}")))?;
    serde_json::from_str(&text.as_string().unwrap_or_default())
        .map_err(|e| request_transport(e.to_string()))
}

pub fn save_book_progress_keepalive(id: &str, progress: &SaveProgressRequest) {
    save_json_keepalive(&format!("/api/files/{id}/book/progress"), progress);
}

fn save_json_keepalive(path: &str, value: &impl serde::Serialize) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let Ok(body) = serde_json::to_string(value) else {
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

    let Ok(request) = BrowserRequest::new_with_str_and_init(path, &init) else {
        return;
    };
    let _ = window.fetch_with_request(&request);
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
    let request = api_request_with_timeout(
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

/// Read the current public-share state of a file.
pub async fn fetch_share(id: &str) -> Result<ShareStatus, RequestError> {
    get_json(&format!("/api/files/{id}/share")).await
}

/// Create or replace a public-share link.
pub async fn create_share(id: &str, expiry: Option<i64>) -> Result<ShareStatus, RequestError> {
    let request = api_request(Request::post(&format!("/api/files/{id}/share")))
        .json(&revaro_core::features::ShareRequest {
            expires_in_seconds: expiry,
        })
        .map_err(|e| request_transport(e.to_string()))?;
    send_json(request).await
}

pub async fn fetch_listing(
    parent: &str,
    q: &str,
    sort: &str,
    offset: i64,
) -> Result<revaro_core::features::Listing, RequestError> {
    let descending = sort.ends_with("_desc");
    let sort = sort.strip_suffix("_desc").unwrap_or(sort);
    let encoded: String = js_sys::encode_uri_component(q).into();
    get_json(&format!(
        "/api/files?q={encoded}&sort={sort}&descending={descending}&offset={offset}&limit=100&parent_id={parent}"
    ))
    .await
}
/// Compact storage and runtime metrics for the topbar status popover.
#[derive(Clone, serde::Deserialize)]
pub struct SystemStatusSummary {
    pub disk_total_bytes: u64,
    pub disk_used_bytes: u64,
    pub disk_available_bytes: u64,
    pub cache: CacheSummary,
}

#[derive(Clone, serde::Deserialize)]
pub struct CacheSummary {
    pub memory_bytes: u64,
    #[serde(default)]
    pub memory_limit: u64,
    pub disk_bytes: u64,
}

pub async fn fetch_system_status() -> Result<SystemStatusSummary, RequestError> {
    get_json("/api/system/status").await
}

#[derive(Clone, Default)]
pub struct LibraryQuery {
    pub kind: String,
    pub query: String,
    pub favorite: bool,
    pub collection: String,
    pub recent: bool,
    pub opened_only: bool,
    pub group_stacks: bool,
    pub stack: String,
    pub offset: i64,
}

pub async fn fetch_library(
    query: &LibraryQuery,
) -> Result<revaro_core::library::LibraryListing, RequestError> {
    let mut path = format!(
        "/api/library/items?limit=60&offset={}&favorite={}&recent={}&opened_only={}&group_stacks={}&q={}",
        query.offset,
        query.favorite,
        query.recent,
        query.opened_only,
        query.group_stacks,
        js_sys::encode_uri_component(&query.query)
    );
    if !query.kind.is_empty() {
        path.push_str(&format!(
            "&kind={}",
            js_sys::encode_uri_component(&query.kind)
        ));
    }
    if !query.stack.is_empty() {
        path.push_str(&format!(
            "&stack={}",
            js_sys::encode_uri_component(&query.stack)
        ));
    }
    if !query.collection.is_empty() {
        path.push_str(&format!(
            "&collection={}",
            js_sys::encode_uri_component(&query.collection)
        ));
    }
    get_json(&path).await
}

pub async fn update_library_item(
    id: &str,
    update: &revaro_core::library::ItemUpdate,
) -> Result<(), RequestError> {
    let request = api_request(Request::patch(&format!("/api/library/items/{id}")))
        .json(update)
        .map_err(|e| request_transport(e.to_string()))?;
    send_empty(request).await
}

pub async fn fetch_collections() -> Result<Vec<revaro_core::library::Collection>, RequestError> {
    get_json("/api/library/collections").await
}

pub async fn create_collection(
    name: &str,
    kind: &str,
) -> Result<revaro_core::library::Collection, RequestError> {
    let request = api_request(Request::post("/api/library/collections"))
        .json(&revaro_core::library::CreateCollection {
            name: name.to_owned(),
            kind: kind.to_owned(),
        })
        .map_err(|e| request_transport(e.to_string()))?;
    send_json(request).await
}

pub async fn collection_member(
    collection: &str,
    file_id: &str,
    add: bool,
) -> Result<(), RequestError> {
    let path = format!("/api/library/collections/{collection}/items/{file_id}");
    let builder = if add {
        Request::put(&path)
    } else {
        Request::delete(&path)
    };
    send_empty(
        api_request(builder)
            .build()
            .map_err(|e| request_transport(e.to_string()))?,
    )
    .await
}

pub async fn delete_collection(id: &str) -> Result<(), RequestError> {
    send_empty(
        api_request(Request::delete(&format!("/api/library/collections/{id}")))
            .build()
            .map_err(|e| request_transport(e.to_string()))?,
    )
    .await
}

pub async fn fetch_stacks() -> Result<Vec<revaro_core::stacks::Stack>, RequestError> {
    get_json("/api/library/stacks").await
}

pub async fn fetch_stack_suggestions()
-> Result<(Vec<revaro_core::stacks::StackSuggestion>, bool), RequestError> {
    let response = api_request(Request::get("/api/library/stack-suggestions"))
        .send()
        .await
        .map_err(|error| request_transport(error.to_string()))?;
    if !response.ok() {
        return Err(decode_request_error(response).await);
    }
    let pending = response.headers().get("x-revaro-index-pending").as_deref() == Some("1");
    let list = response
        .json()
        .await
        .map_err(|error| request_transport(error.to_string()))?;
    Ok((list, pending))
}

pub async fn create_stack(
    name: &str,
    file_ids: Vec<String>,
) -> Result<revaro_core::stacks::Stack, RequestError> {
    let request = api_request(Request::post("/api/library/stacks"))
        .json(&revaro_core::stacks::CreateStack {
            name: name.to_owned(),
            file_ids,
        })
        .map_err(|e| request_transport(e.to_string()))?;
    send_json(request).await
}

pub async fn rename_stack(id: &str, name: &str) -> Result<(), RequestError> {
    let request = api_request(Request::patch(&format!("/api/library/stacks/{id}")))
        .json(&revaro_core::stacks::RenameStack {
            name: name.to_owned(),
        })
        .map_err(|e| request_transport(e.to_string()))?;
    send_empty(request).await
}

pub async fn stack_members(id: &str, file_ids: Vec<String>, add: bool) -> Result<(), RequestError> {
    let path = format!("/api/library/stacks/{id}/items");
    let request = api_request(if add {
        Request::post(&path)
    } else {
        Request::delete(&path)
    })
    .json(&revaro_core::stacks::StackMembers { file_ids })
    .map_err(|e| request_transport(e.to_string()))?;
    send_empty(request).await
}

pub async fn reorder_stack(id: &str, file_ids: Vec<String>) -> Result<(), RequestError> {
    let request = api_request(Request::put(&format!("/api/library/stacks/{id}/order")))
        .json(&revaro_core::stacks::StackMembers { file_ids })
        .map_err(|e| request_transport(e.to_string()))?;
    send_empty(request).await
}

pub async fn dissolve_stack(id: &str) -> Result<(), RequestError> {
    send_empty(
        api_request(Request::delete(&format!("/api/library/stacks/{id}")))
            .build()
            .map_err(|e| request_transport(e.to_string()))?,
    )
    .await
}

pub const SHARE_PAGE_SIZE: i64 = 200;

pub struct SharePage {
    pub items: Vec<revaro_core::features::ShareEntry>,
    pub has_next: bool,
}

pub async fn fetch_shares(offset: i64) -> Result<SharePage, RequestError> {
    let items: Vec<revaro_core::features::ShareEntry> =
        get_json(&format!("/api/shares?offset={offset}")).await?;
    // The endpoint has no total count. Check the following batch only when the
    // current one is full, so an exact full page does not offer an empty page.
    let has_next = if items.len() == SHARE_PAGE_SIZE as usize {
        let next: Vec<revaro_core::features::ShareEntry> =
            get_json(&format!("/api/shares?offset={}", offset + SHARE_PAGE_SIZE)).await?;
        !next.is_empty()
    } else {
        false
    };
    Ok(SharePage { items, has_next })
}
pub async fn fetch_versions(
    id: &str,
) -> Result<Vec<revaro_core::features::DocumentVersion>, RequestError> {
    get_json(&format!("/api/files/{id}/versions")).await
}
pub async fn fetch_version_content(id: &str, version: &str) -> Result<String, RequestError> {
    get_json(&format!("/api/files/{id}/versions/{version}/content")).await
}
pub async fn restore_version(
    id: &str,
    version: &str,
    etag: &str,
) -> Result<revaro_core::model::File, RequestError> {
    let request = api_request(Request::post(&format!(
        "/api/files/{id}/versions/{version}/restore"
    )))
    .json(&revaro_core::features::RestoreVersionRequest {
        etag: etag.to_owned(),
    })
    .map_err(|e| request_transport(e.to_string()))?;
    send_json(request).await
}

/// Revoke a public-share link.
pub async fn revoke_share(id: &str) -> Result<(), RequestError> {
    let request = api_request(Request::delete(&format!("/api/files/{id}/share")))
        .build()
        .map_err(|error| request_transport(error.to_string()))?;
    send_empty(request).await
}

/// Reserve a resumable, user-bound ZIP download ticket for multiple files.
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

/// Sign in, mapping a non-2xx answer to a decoded [`RequestError`].
pub async fn login(request: &LoginRequest) -> Result<Session, RequestError> {
    let request = api_request(Request::post("/api/auth/login"))
        .json(request)
        .map_err(|error| login_transport(error.to_string()))?;
    send_json_with_transport(request, login_transport).await
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
fn login_transport(message: String) -> RequestError {
    RequestError {
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
    send_json(request).await
}

async fn get_json_with_timeout<T>(path: &str, timeout_ms: u32) -> Result<T, RequestError>
where
    T: serde::de::DeserializeOwned,
{
    let request = api_request_with_timeout(Request::get(path), timeout_ms, None)
        .build()
        .map_err(|error| request_transport(error.to_string()))?;
    send_json(request).await
}

/// Send an already encoded JSON request and decode its success body.
async fn send_json<T>(request: Request) -> Result<T, RequestError>
where
    T: serde::de::DeserializeOwned,
{
    send_json_with_transport(request, request_transport).await
}

async fn send_json_with_transport<T>(
    request: Request,
    transport_error: fn(String) -> RequestError,
) -> Result<T, RequestError>
where
    T: serde::de::DeserializeOwned,
{
    let response = request
        .send()
        .await
        .map_err(|error| transport_error(error.to_string()))?;
    if response.ok() {
        return response
            .json::<T>()
            .await
            .map_err(|error| transport_error(error.to_string()));
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
