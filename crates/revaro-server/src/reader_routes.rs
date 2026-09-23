//! EPUB/TXT reader endpoints and persisted reading flows.
//!
//! Parsing and flow generation are CPU-heavy and run on blocking workers. The
//! request handlers only resolve the authenticated file, read or write bounded
//! objects, and translate failures into the established API envelope. Derived
//! flow objects are immutable: chunks are committed first and the manifest is
//! committed last, so a reader never observes a manifest that points at a
//! chunk which has not been written yet.

use std::io::Cursor;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{FromRequest, Path as PathParam, Request, State};
use axum::response::Response;
use axum::routing::get;
use axum::{Json, Router};
use http::StatusCode;
use http::header::{CACHE_CONTROL, CONTENT_DISPOSITION, CONTENT_LENGTH, CONTENT_TYPE, HeaderValue};
use revaro_core::ApiError;
use revaro_core::api::book::{Info as BookInfo, Progress as BookProgress, SaveProgressRequest};
use revaro_core::model::{File, FileKind, FileStatus};
use revaro_core::{keys, reader as reader_model};
use rusqlite::OptionalExtension;

use crate::auth::extract::AuthUser;
use crate::auth_routes::JsonBody;
use crate::cache::{
    CacheError, CacheLoadError, CacheLoadKind, READER_FLOW_CHUNK, READER_FLOW_MANIFEST,
    READER_SOURCE,
};
use crate::file_routes;
use crate::state::AppState;
use crate::storage::StorageError;

const FLOW_VERSION: u32 = revaro_reader::flow::FLOW_FORMAT_VERSION as u32;
const MAX_CHUNK_INDEX: u64 = 1 << 22;

/// Routes mounted below the authenticated API subtree.
pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/files/{id}/book", get(book_info))
        .route("/files/{id}/book/assets/{index}", get(book_asset))
        .route("/files/{id}/book/cover", get(book_cover))
        .route(
            "/files/{id}/book/progress",
            get(book_progress).put(save_book_progress),
        )
        .route("/files/{id}/book/flow", get(book_flow))
        .route("/files/{id}/book/flow/chunks/{index}", get(book_flow_chunk))
}

async fn reader_file(state: Arc<AppState>, id: String) -> Result<File, ApiError> {
    state
        .db
        .call_api(move |connection| {
            let file = file_routes::lookup_file_any(connection, &id).map_err(|error| {
                if error.is_not_found() {
                    ApiError::not_found("ready file not found")
                } else {
                    tracing::error!(%error, "reader file lookup failed");
                    ApiError::internal("database error")
                }
            })?;
            if file.kind != FileKind::File || file.status != FileStatus::Ready {
                return Err(ApiError::not_found("ready file not found"));
            }
            if !revaro_core::classify::is_book_name(&file.name) {
                return Err(ApiError::unsupported_media_type(
                    "只支持阅读 EPUB 和 TXT 文件",
                ));
            }
            if file.size > revaro_reader::MAX_EPUB {
                return Err(ApiError::payload_too_large("文件太大，请下载后离线阅读"));
            }
            Ok(file)
        })
        .await
}

pub(crate) async fn load_book(
    state: Arc<AppState>,
    file: &File,
) -> Result<Arc<revaro_reader::Book>, ApiError> {
    if let Some(book) = state.reader.books.get(&file.object_key) {
        return Ok(book);
    }

    let _guard = state.reader.book_lock(&file.object_key).await;
    if let Some(book) = state.reader.books.get(&file.object_key) {
        return Ok(book);
    }

    // `reader_file` deliberately keeps the Go route's single 128 MiB gate:
    // that gate answers 413. The parser has a smaller TXT limit and reports it
    // as a 422, so perform that check before reading the source to preserve the
    // distinction and its established error text.
    if !revaro_core::classify::is_epub_name(&file.name) && file.size > revaro_reader::MAX_TXT {
        return Err(reader_parse_error(format!(
            "文本文件超过 {} MiB 限制，请下载后离线阅读",
            revaro_reader::MAX_TXT >> 20
        )));
    }

    let limit = if revaro_core::classify::is_epub_name(&file.name) {
        revaro_reader::MAX_EPUB as usize
    } else {
        revaro_reader::MAX_TXT as usize
    };
    let source_key = file.object_key.clone();
    let source_key_for_load = source_key.clone();
    let store = state.store.clone();
    let bytes = state
        .cache
        .load(
            READER_SOURCE,
            &source_key,
            Duration::ZERO,
            move || async move {
                store
                    .read(&source_key_for_load, limit)
                    .await
                    .map_err(reader_cache_load_error)
            },
        )
        .await
        .map_err(|error| reader_cache_error(error, file))?;
    let name = file.name.clone();
    let size = file.size;
    let asset_base = format!("/api/files/{}/book/assets", file.id);
    let etag = file.etag.clone();
    let parsed = tokio::task::spawn_blocking(move || {
        revaro_reader::parse(&name, Cursor::new(bytes), size, &asset_base, &etag)
    })
    .await
    .map_err(|error| reader_parse_error(error.to_string()))?
    .map_err(|error| reader_parse_error(error.to_string()))?;
    let book = Arc::new(parsed);
    state.reader.books.put(&file.object_key, Arc::clone(&book));
    Ok(book)
}

fn reader_cache_load_error(error: StorageError) -> CacheLoadError {
    let kind = if error.is_not_found() {
        CacheLoadKind::NotFound
    } else if matches!(error, StorageError::TooLarge { .. }) {
        CacheLoadKind::TooLarge
    } else {
        CacheLoadKind::Other
    };
    CacheLoadError::new(kind, error.to_string())
}

fn reader_cache_error(error: CacheError, file: &File) -> ApiError {
    match error {
        CacheError::Loader(error) if error.kind() == CacheLoadKind::TooLarge => {
            if !revaro_core::classify::is_epub_name(&file.name) {
                reader_parse_error(format!(
                    "文本文件超过 {} MiB 限制，请下载后离线阅读",
                    revaro_reader::MAX_TXT >> 20
                ))
            } else {
                reader_parse_error(error.message())
            }
        }
        CacheError::Loader(error) => reader_parse_error(error.message()),
        error => {
            tracing::error!(%error, "could not read cached book source");
            reader_parse_error(error.to_string())
        }
    }
}

fn reader_parse_error(error: impl Into<String>) -> ApiError {
    ApiError::unprocessable(format!("无法解析这本书：{}", error.into()))
}

async fn book_info(
    State(state): State<Arc<AppState>>,
    _user: AuthUser,
    PathParam(id): PathParam<String>,
) -> Result<Json<BookInfo>, ApiError> {
    let file = reader_file(Arc::clone(&state), id).await?;
    let book = load_book(state, &file).await?;
    Ok(Json(BookInfo {
        format: book.format.as_str().to_owned(),
        title: book.title.clone(),
        name: file.name,
        cover: !book.cover.is_empty(),
        toc: book.toc.clone(),
    }))
}

async fn book_asset(
    State(state): State<Arc<AppState>>,
    _user: AuthUser,
    PathParam((id, index)): PathParam<(String, String)>,
) -> Result<Response, ApiError> {
    let file = reader_file(Arc::clone(&state), id).await?;
    let book = load_book(state, &file).await?;
    let Ok(index) = index.parse::<usize>() else {
        return Err(ApiError::not_found("asset not found"));
    };
    let Some(asset) = book.assets.get(index) else {
        return Err(ApiError::not_found("asset not found"));
    };
    Ok(bytes_response(
        StatusCode::OK,
        &revaro_core::classify::safe_delivery_mime(&asset.content_type),
        asset.data.clone(),
        "private, max-age=31536000, immutable",
    ))
}

async fn book_cover(
    State(state): State<Arc<AppState>>,
    _user: AuthUser,
    PathParam(id): PathParam<String>,
) -> Result<Response, ApiError> {
    let file = reader_file(Arc::clone(&state), id).await?;
    let book = load_book(state, &file).await?;
    if book.cover.is_empty() {
        return Err(ApiError::not_found("这本书没有内嵌封面"));
    }
    let content_type = revaro_core::classify::safe_delivery_mime(
        revaro_reader::asset_content_type(&book.cover_ext),
    );
    let mut response = bytes_response(
        StatusCode::OK,
        &content_type,
        book.cover.clone(),
        "private, max-age=3600",
    );
    if content_type == "application/octet-stream" {
        response
            .headers_mut()
            .insert(CONTENT_DISPOSITION, HeaderValue::from_static("attachment"));
    }
    Ok(response)
}

fn bytes_response(
    status: StatusCode,
    content_type: &str,
    data: Vec<u8>,
    cache_control: &str,
) -> Response {
    let length = data.len();
    let mut response = Response::new(axum::body::Body::from(data));
    *response.status_mut() = status;
    let headers = response.headers_mut();
    headers.insert(
        CONTENT_TYPE,
        HeaderValue::from_str(content_type)
            .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream")),
    );
    headers.insert(
        CACHE_CONTROL,
        HeaderValue::from_str(cache_control).expect("cache policy is a valid header value"),
    );
    headers.insert(
        CONTENT_LENGTH,
        HeaderValue::from_str(&length.to_string()).expect("a length is a valid header value"),
    );
    response
}

fn progress_key(file_id: &str) -> String {
    format!("book_progress/{file_id}")
}

async fn book_progress(
    State(state): State<Arc<AppState>>,
    _user: AuthUser,
    PathParam(id): PathParam<String>,
) -> Result<Json<BookProgress>, ApiError> {
    let file = reader_file(Arc::clone(&state), id).await?;
    let key = progress_key(&file.id);
    let raw = state
        .db
        .call_api(move |connection| {
            connection
                .query_row("SELECT value FROM settings WHERE key = ?1", [&key], |row| {
                    row.get::<_, String>(0)
                })
                .optional()
                .map_err(|error| {
                    tracing::error!(%error, "could not read book progress");
                    ApiError::internal("could not read progress")
                })
        })
        .await?;

    let mut progress = raw
        .as_deref()
        .and_then(|value| serde_json::from_str::<BookProgress>(value).ok())
        .unwrap_or_default();
    if progress
        .anchor
        .as_ref()
        .is_some_and(|anchor| !anchor.is_valid())
    {
        progress.anchor = None;
    }
    Ok(Json(progress))
}

async fn save_book_progress(
    State(state): State<Arc<AppState>>,
    _user: AuthUser,
    PathParam(id): PathParam<String>,
    request: Request,
) -> Result<StatusCode, ApiError> {
    let file = reader_file(Arc::clone(&state), id).await?;
    let JsonBody(request) =
        JsonBody::<Option<SaveProgressRequest>>::from_request(request, &state).await?;
    // `encoding/json` accepts `null` into a struct and leaves it at its zero
    // value. Deserialising an Option preserves that small wire-level detail
    // while `{}` continues to mean the same thing as an empty progress object.
    let anchor = request.and_then(|request| request.anchor);
    if anchor.as_ref().is_some_and(|anchor| !anchor.is_valid()) {
        return Err(ApiError::bad_request("progress anchor is invalid"));
    }
    let raw = serde_json::to_string(&BookProgress { anchor })
        .map_err(|_| ApiError::bad_request("progress values are invalid"))?;
    let key = progress_key(&file.id);
    state
        .db
        .call_api(move |connection| {
            connection
                .execute(
                    "INSERT INTO settings(key,value,updated_at) VALUES(?1,?2,?3) \
                     ON CONFLICT(key) DO UPDATE SET value=excluded.value,updated_at=excluded.updated_at",
                    rusqlite::params![key, raw, revaro_core::Timestamp::now().to_rfc3339()],
                )
                .map_err(|error| {
                    tracing::error!(%error, "could not save book progress");
                    ApiError::internal("could not save progress")
                })?;
            Ok(StatusCode::NO_CONTENT)
        })
        .await
}

async fn book_flow(
    State(state): State<Arc<AppState>>,
    _user: AuthUser,
    PathParam(id): PathParam<String>,
) -> Result<Response, ApiError> {
    let file = reader_file(Arc::clone(&state), id).await?;
    ensure_flow(Arc::clone(&state), &file).await?;
    let manifest_key = keys::flow_manifest_key(&file.object_key, FLOW_VERSION);
    let data = read_flow_object(&state, READER_FLOW_MANIFEST, &manifest_key)
        .await
        .map_err(flow_cache_api_error)?;
    serde_json::from_slice::<reader_model::FlowManifest>(&data).map_err(|error| {
        tracing::error!(%error, "flow manifest is invalid");
        ApiError::internal("could not read flow manifest")
    })?;
    Ok(bytes_response(
        StatusCode::OK,
        "application/json; charset=utf-8",
        data,
        "private, no-cache",
    ))
}

async fn book_flow_chunk(
    State(state): State<Arc<AppState>>,
    _user: AuthUser,
    PathParam((id, index)): PathParam<(String, String)>,
) -> Result<Response, ApiError> {
    let index = index
        .parse::<u64>()
        .ok()
        .filter(|index| *index <= MAX_CHUNK_INDEX)
        .and_then(|index| usize::try_from(index).ok())
        .ok_or_else(|| ApiError::bad_request("chunk index is invalid"))?;
    let file = reader_file(Arc::clone(&state), id).await?;
    ensure_flow(Arc::clone(&state), &file).await?;
    let manifest = read_manifest(&state, &file).await?;
    if index >= manifest.chunks.len() {
        return Err(ApiError::not_found("chunk not found"));
    }

    let key = keys::flow_chunk_key(
        &file.object_key,
        FLOW_VERSION,
        u32::try_from(index).unwrap_or(u32::MAX),
    );
    if let Err(error) = state.store.head(&key).await {
        if error.is_not_found() {
            let _ = state
                .cache
                .delete(
                    READER_FLOW_CHUNK,
                    &flow_chunk_cache_key_from_object_key(&key),
                )
                .await;
            rebuild_flow(Arc::clone(&state), &file).await?;
        } else {
            tracing::error!(%error, "could not stat flow chunk");
            return Err(ApiError::internal("could not read flow chunk"));
        }
    }
    let data = match read_flow_object(&state, READER_FLOW_CHUNK, &key).await {
        Ok(data) => data,
        Err(error) if error.is_loader_not_found() => {
            rebuild_flow(Arc::clone(&state), &file).await?;
            read_flow_object(&state, READER_FLOW_CHUNK, &key)
                .await
                .map_err(|_| ApiError::not_found("chunk not found"))?
        }
        Err(error) => {
            tracing::error!(%error, "could not read flow chunk");
            return Err(ApiError::internal("could not read flow chunk"));
        }
    };
    Ok(bytes_response(
        StatusCode::OK,
        "text/html; charset=utf-8",
        data,
        "private, max-age=31536000, immutable",
    ))
}

async fn read_manifest(
    state: &AppState,
    file: &File,
) -> Result<reader_model::FlowManifest, ApiError> {
    let key = keys::flow_manifest_key(&file.object_key, FLOW_VERSION);
    let data = read_flow_object(state, READER_FLOW_MANIFEST, &key)
        .await
        .map_err(flow_cache_api_error)?;
    serde_json::from_slice(&data).map_err(|error| {
        tracing::error!(%error, "flow manifest is invalid");
        ApiError::internal("could not read flow manifest")
    })
}

async fn ensure_flow(state: Arc<AppState>, file: &File) -> Result<(), ApiError> {
    let manifest_key = keys::flow_manifest_key(&file.object_key, FLOW_VERSION);
    if state.store.head(&manifest_key).await.is_ok() {
        return Ok(());
    }
    let _guard = state.reader.flow_lock(&file.object_key).await;
    if state.store.head(&manifest_key).await.is_ok() {
        return Ok(());
    }
    generate_flow(state, file).await
}

async fn rebuild_flow(state: Arc<AppState>, file: &File) -> Result<(), ApiError> {
    let _guard = state.reader.flow_lock(&file.object_key).await;
    let _ = state
        .cache
        .invalidate(&format!(
            "{READER_FLOW_MANIFEST}\0{}",
            flow_manifest_cache_key(&file.object_key)
        ))
        .await;
    let _ = state
        .cache
        .invalidate(&format!(
            "{READER_FLOW_CHUNK}\0{}",
            flow_chunk_cache_key_prefix(&file.object_key)
        ))
        .await;
    generate_flow(state, file).await
}

async fn read_flow_object(
    state: &AppState,
    class: &'static str,
    object_key: &str,
) -> Result<Vec<u8>, CacheError> {
    let cache_key = if class == READER_FLOW_MANIFEST {
        flow_manifest_cache_key_from_object_key(object_key)
    } else {
        flow_chunk_cache_key_from_object_key(object_key)
    };
    let store_key = object_key.to_owned();
    let store = state.store.clone();
    state
        .cache
        .load(
            class,
            &cache_key,
            state.config.flow_cache_ttl,
            move || async move {
                store
                    .read(&store_key, revaro_reader::flow::MAX_FLOW_OBJECT)
                    .await
                    .map_err(reader_cache_load_error)
            },
        )
        .await
}

fn flow_manifest_cache_key(object_key: &str) -> String {
    format!("manifest/{object_key}/f{FLOW_VERSION}")
}

fn flow_manifest_cache_key_from_object_key(object_key: &str) -> String {
    let prefix = "flows/";
    let Some(rest) = object_key.strip_prefix(prefix) else {
        return object_key.to_owned();
    };
    let Some(book) = rest.strip_suffix("/manifest.json") else {
        return object_key.to_owned();
    };
    format!("manifest/{book}")
}

fn flow_chunk_cache_key_prefix(object_key: &str) -> String {
    format!("chunk/{object_key}/f{FLOW_VERSION}/")
}

fn flow_chunk_cache_key_from_object_key(object_key: &str) -> String {
    let prefix = "flows/";
    let Some(rest) = object_key.strip_prefix(prefix) else {
        return object_key.to_owned();
    };
    let Some((book, index)) = rest.split_once("/chunks/") else {
        return object_key.to_owned();
    };
    let Some(index) = index.strip_suffix(".html") else {
        return object_key.to_owned();
    };
    format!("chunk/{book}/{index}")
}

async fn generate_flow(state: Arc<AppState>, file: &File) -> Result<(), ApiError> {
    let book = load_book(Arc::clone(&state), file).await?;
    let built = tokio::task::spawn_blocking(move || revaro_reader::flow::build(&book))
        .await
        .map_err(|error| flow_build_error(error.to_string()))?
        .map_err(|error| flow_build_error(error.to_string()))?;

    for chunk in &built.chunks {
        if chunk.html.len() > revaro_reader::flow::MAX_FLOW_OBJECT {
            return Err(ApiError::unprocessable("阅读流片段超过大小限制"));
        }
        let key = keys::flow_chunk_key(
            &file.object_key,
            FLOW_VERSION,
            u32::try_from(chunk.meta.index).unwrap_or(u32::MAX),
        );
        state
            .store
            .put_immutable(&key, chunk.html.as_bytes())
            .await
            .map_err(|error| {
                tracing::error!(%error, "could not write flow chunk");
                ApiError::new(502, "could not write flow chunk")
            })?;
    }

    let mut manifest = built.manifest;
    manifest.book_key = keys::flow_book_fingerprint(&file.object_key);
    let raw = serde_json::to_vec(&manifest).map_err(|error| {
        tracing::error!(%error, "could not serialize flow manifest");
        ApiError::internal("could not write flow manifest")
    })?;
    if raw.len() > revaro_reader::flow::MAX_FLOW_OBJECT {
        return Err(ApiError::unprocessable("阅读流清单超过大小限制"));
    }
    let manifest_key = keys::flow_manifest_key(&file.object_key, FLOW_VERSION);
    state
        .store
        .put_immutable(&manifest_key, &raw)
        .await
        .map_err(|error| {
            tracing::error!(%error, "could not write flow manifest");
            ApiError::new(502, "could not write flow manifest")
        })?;
    Ok(())
}

fn flow_build_error(error: impl Into<String>) -> ApiError {
    ApiError::unprocessable(format!("无法生成阅读流：{}", error.into()))
}

fn flow_cache_api_error(error: CacheError) -> ApiError {
    tracing::error!(%error, "could not read cached flow object");
    ApiError::internal("could not read flow object")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Write};

    use crate::db::DbError;
    use axum::body::Body;
    use http::{HeaderMap, Request, StatusCode};
    use http_body_util::BodyExt as _;
    use revaro_core::ids::ROOT_ID;
    use tower::ServiceExt as _;
    use zip::write::SimpleFileOptions;
    use zip::{CompressionMethod, ZipWriter};

    const SESSION: &str = "reader-route-session";

    async fn state() -> Arc<AppState> {
        let root =
            std::env::temp_dir().join(format!("revaro-reader-routes-{}", uuid::Uuid::new_v4()));
        let config = crate::config::Config::from_lookup(&|name| match name {
            "APP_BASE_URL" => Some("http://localhost:8080".to_owned()),
            "APP_WEB_DIR" => Some("/nonexistent-web-dir".to_owned()),
            "APP_CACHES_DIR" => Some(root.join("caches").display().to_string()),
            _ => None,
        })
        .expect("test configuration is valid");
        let store = crate::storage::LocalStore::open(root)
            .await
            .expect("object store opens");
        let database = crate::db::Database::open_in_memory().expect("database opens");
        let auth = crate::auth::AuthService::new(database.clone());
        AppState::new(Arc::new(config), database, store, auth)
    }

    async fn authenticate(state: &Arc<AppState>) {
        let hash = crate::auth::token_hash(SESSION);
        state
            .db
            .call(move |connection| {
                connection
                    .execute(
                        "INSERT OR REPLACE INTO settings(key,value,updated_at) \
                         VALUES('admin_username','admin','2024-01-01T00:00:00Z')",
                        [],
                    )
                    .map_err(DbError::Query)?;
                connection
                    .execute(
                        "INSERT OR REPLACE INTO sessions(id,token_hash,created_at,expires_at) \
                         VALUES('reader-session',?1,'2024-01-01T00:00:00Z','2999-01-01T00:00:00Z')",
                        [&hash],
                    )
                    .map_err(DbError::Query)?;
                Ok(())
            })
            .await
            .expect("session inserts");
    }

    async fn request(
        state: &Arc<AppState>,
        method: &str,
        uri: &str,
        body: Option<Vec<u8>>,
    ) -> (StatusCode, HeaderMap, Vec<u8>) {
        authenticate(state).await;
        request_with_session(state, method, uri, body).await
    }

    async fn request_with_session(
        state: &Arc<AppState>,
        method: &str,
        uri: &str,
        body: Option<Vec<u8>>,
    ) -> (StatusCode, HeaderMap, Vec<u8>) {
        let mut builder = Request::builder().method(method).uri(uri).header(
            "cookie",
            format!("{}={SESSION}", crate::auth::SESSION_COOKIE),
        );
        if body.is_some() {
            builder = builder
                .header("content-type", "application/json")
                .header("origin", "http://localhost:8080");
        }
        let response = crate::router::build(state.clone())
            .oneshot(builder.body(Body::from(body.unwrap_or_default())).unwrap())
            .await
            .expect("request completes");
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("response body collects")
            .to_bytes()
            .to_vec();
        (status, headers, bytes)
    }

    async fn seed_book(
        state: &Arc<AppState>,
        id: &str,
        name: &str,
        content_type: &str,
        bytes: &[u8],
    ) {
        let object_key = keys::blob_key(id);
        let info = state
            .store
            .put(&object_key, bytes)
            .await
            .expect("book object writes");
        let id = id.to_owned();
        let name = name.to_owned();
        let object_key_for_db = object_key;
        let size = bytes.len() as i64;
        let content_type = content_type.to_owned();
        state
            .db
            .call(move |connection| {
                connection
                    .execute(
                        "INSERT INTO files(\
                         id,parent_id,name,kind,object_key,size,mime_type,etag,status,created_at,updated_at\
                         ) VALUES(?1,?2,?3,'file',?4,?5,?6,?7,'ready',?8,?8)",
                        rusqlite::params![
                            id,
                            ROOT_ID,
                            name,
                            object_key_for_db,
                            size,
                            content_type,
                            info.etag,
                            "2024-01-01T00:00:00Z"
                        ],
                    )
                    .map_err(DbError::Query)?;
                Ok(())
            })
            .await
            .expect("book row inserts");
    }

    fn fake_png(width: u32, height: u32) -> Vec<u8> {
        let mut data = vec![0u8; 33];
        data[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
        data[8..12].copy_from_slice(&13u32.to_be_bytes());
        data[12..16].copy_from_slice(b"IHDR");
        data[16..20].copy_from_slice(&width.to_be_bytes());
        data[20..24].copy_from_slice(&height.to_be_bytes());
        data
    }

    fn epub_fixture() -> Vec<u8> {
        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        let add = |writer: &mut ZipWriter<Cursor<Vec<u8>>>, name: &str, data: &[u8]| {
            writer.start_file(name, options).expect("zip entry starts");
            writer.write_all(data).expect("zip entry writes");
        };
        add(&mut writer, "mimetype", b"application/epub+zip");
        add(
            &mut writer,
            "META-INF/container.xml",
            br#"<?xml version="1.0"?><container><rootfiles><rootfile full-path="OEBPS/content.opf"/></rootfiles></container>"#,
        );
        add(
            &mut writer,
            "OEBPS/content.opf",
            r#"<package xmlns="http://www.idpf.org/2007/opf"><metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>路由测试书</dc:title><meta name="cover" content="cover-img"/></metadata><manifest><item id="ch" href="ch.xhtml" media-type="application/xhtml+xml"/><item id="cover-img" href="img/cover.png" media-type="image/png"/><item id="fig" href="img/fig.png" media-type="image/png"/></manifest><spine><itemref idref="ch"/></spine></package>"#.as_bytes(),
        );
        add(
            &mut writer,
            "OEBPS/ch.xhtml",
            r#"<html><body><h1 id="start">标题</h1><p>正文<img src="img/fig.png" alt="图"/></p></body></html>"#.as_bytes(),
        );
        add(&mut writer, "OEBPS/img/cover.png", &fake_png(300, 400));
        add(&mut writer, "OEBPS/img/fig.png", &fake_png(10, 20));
        writer.finish().expect("zip finishes").into_inner()
    }

    #[test]
    fn progress_key_is_scoped_to_the_file_id() {
        assert_eq!(progress_key("book-1"), "book_progress/book-1");
    }

    #[test]
    fn chunk_index_bounds_are_explicit() {
        assert!(
            "0".parse::<u64>()
                .ok()
                .is_some_and(|index| index <= MAX_CHUNK_INDEX)
        );
        assert!("-1".parse::<u64>().is_err());
        assert!(
            (MAX_CHUNK_INDEX + 1)
                .to_string()
                .parse::<u64>()
                .ok()
                .is_some_and(|index| index > MAX_CHUNK_INDEX)
        );
    }

    #[tokio::test]
    async fn info_asset_and_cover_routes_return_real_epub_bytes() {
        let state = state().await;
        let epub = epub_fixture();
        seed_book(
            &state,
            "reader-epub",
            "book.epub",
            "application/epub+zip",
            &epub,
        )
        .await;

        let (status, _, body) = request(&state, "GET", "/api/files/reader-epub/book", None).await;
        assert_eq!(status, StatusCode::OK);
        let info: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(info["format"], "epub");
        assert_eq!(info["title"], "路由测试书");
        assert_eq!(info["name"], "book.epub");
        assert_eq!(info["cover"], true);

        let (status, headers, body) =
            request(&state, "GET", "/api/files/reader-epub/book/assets/0", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(headers[CONTENT_TYPE], "image/png");
        assert_eq!(headers[CONTENT_LENGTH], fake_png(10, 20).len().to_string());
        assert_eq!(body, fake_png(10, 20));

        let (status, headers, body) =
            request(&state, "GET", "/api/files/reader-epub/book/cover", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(headers[CONTENT_TYPE], "image/png");
        assert_eq!(headers[CACHE_CONTROL], "private, max-age=3600");
        assert_eq!(body, fake_png(300, 400));

        let (status, headers, body) =
            request(&state, "GET", "/api/files/reader-epub/book/flow", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(headers[CONTENT_TYPE], "application/json; charset=utf-8");
        let manifest: reader_model::FlowManifest = serde_json::from_slice(&body).unwrap();
        assert_eq!(manifest.format, "epub");
        assert_eq!(manifest.spines.len(), 1);
        assert_eq!(manifest.total_blocks(), 2);

        let (status, headers, body) = request(
            &state,
            "GET",
            "/api/files/reader-epub/book/flow/chunks/0",
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(headers[CONTENT_TYPE], "text/html; charset=utf-8");
        let html = String::from_utf8(body).unwrap();
        assert!(html.contains(r#"data-block="0""#));
        assert!(html.contains("/api/files/reader-epub/book/assets/0?v="));
        assert!(!html.contains("<script"));
        let stats = state.cache.stats();
        assert_eq!(stats.classes[READER_SOURCE].loads, 1);
        assert_eq!(stats.classes[READER_FLOW_MANIFEST].loads, 1);
        assert_eq!(stats.classes[READER_FLOW_CHUNK].loads, 1);
    }

    #[tokio::test]
    async fn flow_route_persists_chunks_and_repairs_a_missing_chunk() {
        let state = state().await;
        let text = "第一章\n正文😀\n第二章\n继续阅读\n";
        seed_book(
            &state,
            "reader-txt",
            "book.txt",
            "text/plain",
            text.as_bytes(),
        )
        .await;

        let (status, headers, body) =
            request(&state, "GET", "/api/files/reader-txt/book/flow", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(headers[CONTENT_TYPE], "application/json; charset=utf-8");
        assert_eq!(headers[CACHE_CONTROL], "private, no-cache");
        let manifest: reader_model::FlowManifest = serde_json::from_slice(&body).unwrap();
        assert_eq!(manifest.version, revaro_reader::flow::FLOW_FORMAT_VERSION);
        assert_eq!(manifest.format, "txt");
        assert_eq!(manifest.total_chars, text.encode_utf16().count() as i64);
        assert_eq!(
            manifest.book_key,
            keys::flow_book_fingerprint("blobs/reader-txt")
        );
        assert!(!manifest.chunks.is_empty());

        let manifest_key = keys::flow_manifest_key("blobs/reader-txt", FLOW_VERSION);
        assert!(state.store.head(&manifest_key).await.is_ok());
        let (status, headers, body) = request(
            &state,
            "GET",
            "/api/files/reader-txt/book/flow/chunks/0",
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(headers[CONTENT_TYPE], "text/html; charset=utf-8");
        assert!(
            headers[CACHE_CONTROL]
                .to_str()
                .unwrap()
                .contains("immutable")
        );
        let html = String::from_utf8(body).unwrap();
        assert!(html.contains("data-block=\"0\""));
        assert!(html.contains("第一章"));

        let chunk_key = keys::flow_chunk_key("blobs/reader-txt", FLOW_VERSION, 0);
        state.store.delete(&chunk_key).await.unwrap();
        let (status, _, repaired) = request(
            &state,
            "GET",
            "/api/files/reader-txt/book/flow/chunks/0",
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(repaired, html.into_bytes());
        assert!(state.store.head(&chunk_key).await.is_ok());

        let (status, _, _) = request(
            &state,
            "GET",
            "/api/files/reader-txt/book/flow/chunks/-1",
            None,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (status, _, _) = request(
            &state,
            "GET",
            "/api/files/reader-txt/book/flow/chunks/99",
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_flow_requests_share_one_persistent_build() {
        let state = state().await;
        seed_book(
            &state,
            "reader-concurrent",
            "concurrent.txt",
            "text/plain",
            b"concurrent reader source\n",
        )
        .await;
        authenticate(&state).await;

        let (first, second) = tokio::join!(
            request_with_session(
                &state,
                "GET",
                "/api/files/reader-concurrent/book/flow",
                None,
            ),
            request_with_session(
                &state,
                "GET",
                "/api/files/reader-concurrent/book/flow",
                None,
            )
        );
        assert_eq!(first.0, StatusCode::OK);
        assert_eq!(second.0, StatusCode::OK);
        assert_eq!(first.2, second.2);

        let objects = state
            .store
            .list_prefix("flows/blobs/reader-concurrent/")
            .await
            .unwrap();
        assert_eq!(objects.len(), 2, "one manifest and one chunk: {objects:?}");
    }

    #[tokio::test]
    async fn progress_route_accepts_empty_and_legacy_anchors_but_rejects_invalid_data() {
        let state = state().await;
        seed_book(
            &state,
            "reader-progress",
            "book.txt",
            "text/plain",
            b"content",
        )
        .await;

        let (status, _, body) = request(
            &state,
            "GET",
            "/api/files/reader-progress/book/progress",
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&body).unwrap(),
            serde_json::json!({})
        );

        let (status, _, _) = request(
            &state,
            "PUT",
            "/api/files/reader-progress/book/progress",
            Some(br#"{}"#.to_vec()),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (status, _, _) = request(
            &state,
            "PUT",
            "/api/files/reader-progress/book/progress",
            Some(br#"null"#.to_vec()),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        let (status, _, _) = request(
            &state,
            "PUT",
            "/api/files/reader-progress/book/progress",
            Some(br#"{"anchor":{"spine":1,"block":7,"path":[2],"offset":3}}"#.to_vec()),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (status, _, body) = request(
            &state,
            "GET",
            "/api/files/reader-progress/book/progress",
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let progress: BookProgress = serde_json::from_slice(&body).unwrap();
        assert_eq!(progress.anchor.unwrap().block, 7);

        let (status, _, body) = request(
            &state,
            "PUT",
            "/api/files/reader-progress/book/progress",
            Some(br#"{"anchor":{"spine":0,"block":0,"offset":-2}}"#.to_vec()),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&body).unwrap()["error"]["message"],
            "progress anchor is invalid"
        );

        let (status, _, _) = request(
            &state,
            "PUT",
            "/api/files/reader-progress/book/progress",
            Some(br#"{"anchor":{"spine":0,"path":[9,2],"offset":1}}"#.to_vec()),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (_, _, body) = request(
            &state,
            "GET",
            "/api/files/reader-progress/book/progress",
            None,
        )
        .await;
        let progress: BookProgress = serde_json::from_slice(&body).unwrap();
        let anchor = progress.anchor.unwrap();
        assert_eq!(anchor.block, 9);
        assert_eq!(anchor.path, vec![2]);
    }

    #[tokio::test]
    async fn reader_rejects_non_books_before_touching_the_source() {
        let state = state().await;
        seed_book(&state, "reader-pdf", "book.pdf", "application/pdf", b"pdf").await;
        let (status, _, body) = request(&state, "GET", "/api/files/reader-pdf/book", None).await;
        assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&body).unwrap()["error"]["message"],
            "只支持阅读 EPUB 和 TXT 文件"
        );
    }

    #[tokio::test]
    async fn oversized_txt_keeps_the_parser_error_distinct_from_the_route_gate() {
        let state = state().await;
        seed_book(
            &state,
            "reader-large-txt",
            "large.txt",
            "text/plain",
            b"small source",
        )
        .await;
        state
            .db
            .call(|connection| {
                connection
                    .execute(
                        "UPDATE files SET size = ?1 WHERE id = ?2",
                        rusqlite::params![revaro_reader::MAX_TXT + 1, "reader-large-txt"],
                    )
                    .map_err(DbError::Query)?;
                Ok(())
            })
            .await
            .expect("file size updates");

        let (status, _, body) =
            request(&state, "GET", "/api/files/reader-large-txt/book", None).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&body).unwrap()["error"]["message"],
            "无法解析这本书：文本文件超过 16 MiB 限制，请下载后离线阅读"
        );
    }
}
