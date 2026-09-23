//! Media metadata and thumbnail HTTP endpoints.
//!
//! The handler layer owns file permissions, cache keys and SQLite persistence;
//! [`revaro_media`] only sees an already-open object and performs bounded native
//! decoding.  This keeps FFmpeg away from request-controlled paths and makes a
//! future media engine replacement independent of the HTTP contract.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path as PathParam, State};
use axum::response::Response;
use axum::routing::{get, post};
use axum::{Json, Router};
use http::header::{CACHE_CONTROL, CONTENT_LENGTH, CONTENT_TYPE, ETAG, HeaderValue};
use revaro_core::classify;
use revaro_core::media::MediaProbe;
use revaro_core::model::{File, FileKind, FileStatus};
use revaro_core::{Timestamp, keys};
use revaro_media::{MediaEngine, MediaError};
use rusqlite::OptionalExtension;
use tokio_util::sync::CancellationToken;

use crate::auth::extract::AuthUser;
use crate::file_routes;
use crate::state::AppState;

const MAX_THUMB_BYTES: usize = 512 << 10;
const MAX_THUMB_SOURCE: usize = 64 << 20;
const THUMB_MAX_DIMENSION: u32 = 640;
const MEDIA_PROBE_VERSION: i64 = 2;

/// Routes mounted below the authenticated API subtree.
pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/files/{id}/audio", get(audio_media_info))
        .route("/files/{id}/media/reanalyze", post(reanalyze_media))
        .route("/files/{id}/thumbnail", get(thumbnail))
}

#[derive(Debug, Clone)]
struct StoredMetadata {
    probe: MediaProbe,
    source_etag: String,
    version: i64,
}

/// Resolve a ready file for one media operation.
async fn ready_media_file(
    state: Arc<AppState>,
    id: String,
    kind: MediaKind,
    message: &'static str,
) -> Result<File, revaro_core::ApiError> {
    state
        .db
        .call_api(move |connection| {
            let file = file_routes::lookup_file_any(connection, &id)
                .map_err(|error| not_found_or(error, message))?;
            let valid = file.kind == FileKind::File
                && file.status == FileStatus::Ready
                && match kind {
                    MediaKind::Any => true,
                    MediaKind::Audio => classify::is_audio(&file),
                };
            if !valid {
                return Err(revaro_core::ApiError::not_found(message));
            }
            Ok(file)
        })
        .await
}

#[derive(Debug, Clone, Copy)]
enum MediaKind {
    Any,
    Audio,
}

fn not_found_or(error: crate::db::DbError, message: &'static str) -> revaro_core::ApiError {
    if error.is_not_found() {
        revaro_core::ApiError::not_found(message)
    } else {
        file_routes::database_error(error)
    }
}

/// Read the current metadata row without treating stale data as valid.
fn read_metadata(
    connection: &rusqlite::Connection,
    file_id: &str,
) -> rusqlite::Result<Option<StoredMetadata>> {
    connection
        .query_row(
            "SELECT duration_ms,container,video_codec,audio_codec,width,height,bitrate,\
chapters_json,frame_rate,video_profile,video_level,\
source_etag,probe_version FROM media_metadata WHERE file_id = ?1",
            [file_id],
            |row| {
                let chapters_json: String = row.get(7)?;
                Ok(StoredMetadata {
                    probe: MediaProbe {
                        duration_ms: row.get(0)?,
                        container: row.get(1)?,
                        video_codec: row.get(2)?,
                        audio_codec: row.get(3)?,
                        width: row.get(4)?,
                        height: row.get(5)?,
                        bitrate: row.get(6)?,
                        frame_rate: row.get(8)?,
                        video_profile: row.get(9)?,
                        video_level: row.get(10)?,
                        chapters: serde_json::from_str(&chapters_json).unwrap_or_default(),
                    },
                    source_etag: row.get(11)?,
                    version: row.get(12)?,
                })
            },
        )
        .optional()
}

fn metadata_is_fresh(metadata: &StoredMetadata, file: &File) -> bool {
    metadata.source_etag == file.etag && metadata.version == MEDIA_PROBE_VERSION
}

/// Ensure one current metadata row exists, with one probe in flight per file.
pub(crate) async fn ensure_media_metadata(
    state: Arc<AppState>,
    file: File,
) -> Result<MediaProbe, revaro_core::ApiError> {
    let _lock = state.media.metadata_lock(&file.id).await;
    let file_id = file.id.clone();
    let stored = state
        .db
        .call_api(move |connection| {
            read_metadata(connection, &file_id)
                .map_err(crate::db::DbError::Query)
                .map_err(file_routes::database_error)
        })
        .await?;
    if let Some(metadata) = stored
        .as_ref()
        .filter(|metadata| metadata_is_fresh(metadata, &file))
    {
        return Ok(metadata.probe.clone());
    }

    let probe = probe_file(&state, &file).await?;
    persist_metadata(&state, &file, &probe).await?;
    Ok(probe)
}

async fn persist_metadata(
    state: &Arc<AppState>,
    file: &File,
    probe: &MediaProbe,
) -> Result<(), revaro_core::ApiError> {
    let chapters = serde_json::to_string(&probe.chapters)
        .map_err(|_| revaro_core::ApiError::internal("could not serialize media metadata"))?;
    let file_id = file.id.clone();
    let source_etag = file.etag.clone();
    let analyzed_at = Timestamp::now().to_rfc3339();
    let duration_ms = probe.duration_ms;
    let container = probe.container.clone();
    let video_codec = probe.video_codec.clone();
    let audio_codec = probe.audio_codec.clone();
    let width = probe.width;
    let height = probe.height;
    let bitrate = probe.bitrate.max(0);
    let frame_rate = probe.frame_rate.clone();
    let video_profile = probe.video_profile.clone();
    let video_level = probe.video_level;
    state
        .db
        .call_api(move |connection| {
            connection
                .execute(
                    "INSERT INTO media_metadata(file_id,duration_ms,container,video_codec,\
audio_codec,width,height,bitrate,chapters_json,analyzed_at,frame_rate,video_profile,\
video_level,source_etag,probe_version) VALUES \
                    (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15) \
                    ON CONFLICT(file_id) DO UPDATE SET duration_ms=excluded.duration_ms,\
container=excluded.container,video_codec=excluded.video_codec,\
audio_codec=excluded.audio_codec,width=excluded.width,height=excluded.height,\
bitrate=excluded.bitrate,chapters_json=excluded.chapters_json,\
analyzed_at=excluded.analyzed_at,frame_rate=excluded.frame_rate,\
video_profile=excluded.video_profile,video_level=excluded.video_level,\
source_etag=excluded.source_etag,\
probe_version=excluded.probe_version",
                    rusqlite::params![
                        file_id,
                        duration_ms,
                        container,
                        video_codec,
                        audio_codec,
                        width,
                        height,
                        bitrate,
                        chapters,
                        analyzed_at,
                        frame_rate,
                        video_profile,
                        video_level,
                        source_etag,
                        MEDIA_PROBE_VERSION,
                    ],
                )
                .map_err(crate::db::DbError::Query)
                .map_err(file_routes::database_error)
        })
        .await
        .map(|_| ())
}

async fn probe_file(
    state: &Arc<AppState>,
    file: &File,
) -> Result<MediaProbe, revaro_core::ApiError> {
    run_engine(
        state,
        file,
        Arc::clone(&state.media.light_slots),
        |engine, reader, cancel| engine.probe(reader, cancel),
    )
    .await
    .map_err(|error| {
        tracing::warn!(file = %file.id, %error, "media probe failed");
        revaro_core::ApiError::unprocessable("无法读取媒体信息")
    })
}

/// Run a blocking native operation with an owned object handle and a bounded
/// permit. Dropping the request future cancels FFmpeg's interrupt callback.
async fn run_engine<T, F>(
    state: &Arc<AppState>,
    file: &File,
    slots: Arc<tokio::sync::Semaphore>,
    operation: F,
) -> Result<T, MediaError>
where
    T: Send + 'static,
    F: FnOnce(MediaEngine, std::fs::File, CancellationToken) -> Result<T, MediaError>
        + Send
        + 'static,
{
    let object = state
        .store
        .open_object(&file.object_key)
        .await
        .map_err(|error| MediaError::Input(error.to_string()))?;
    let reader = object.file.into_std().await;
    let _permit = slots
        .acquire_owned()
        .await
        .map_err(|_| MediaError::Cancelled)?;
    let cancel = CancellationToken::new();
    let guard = CancelOnDrop(cancel.clone());
    let engine = state.media.engine;
    let result = tokio::task::spawn_blocking(move || operation(engine, reader, cancel))
        .await
        .map_err(|error| MediaError::Input(error.to_string()))?;
    drop(guard);
    result
}

struct CancelOnDrop(CancellationToken);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

/// `GET /api/files/{id}/audio`.
async fn audio_media_info(
    State(state): State<Arc<AppState>>,
    _user: AuthUser,
    PathParam(id): PathParam<String>,
) -> Result<Json<revaro_core::api::media::AudioMedia>, revaro_core::ApiError> {
    let file = ready_media_file(
        state.clone(),
        id,
        MediaKind::Audio,
        "ready audio file not found",
    )
    .await?;
    let probe = ensure_media_metadata(state, file.clone())
        .await
        .map_err(|_| revaro_core::ApiError::not_found("audio metadata is not available"))?;
    let has_cover = probe.has_video_stream();
    let duration = probe.duration_seconds();
    let chapters = probe
        .chapters
        .into_iter()
        .enumerate()
        .map(|(index, chapter)| revaro_core::media::AudioChapter {
            id: i32::try_from(index + 1).unwrap_or(i32::MAX),
            title: chapter.title,
            start: chapter.start_ms as f64 / 1000.0,
            end: chapter.end_ms as f64 / 1000.0,
        })
        .collect();
    Ok(Json(revaro_core::api::media::AudioMedia {
        duration,
        chapters,
        cover_url: if has_cover {
            format!("/api/files/{}/thumbnail?v={}", file.id, file.etag)
        } else {
            String::new()
        },
        has_cover,
    }))
}

/// `POST /api/files/{id}/media/reanalyze`.
async fn reanalyze_media(
    State(state): State<Arc<AppState>>,
    _user: AuthUser,
    PathParam(id): PathParam<String>,
) -> Result<Json<revaro_core::api::media::ReanalyzeResult>, revaro_core::ApiError> {
    let file = ready_media_file(
        state.clone(),
        id,
        MediaKind::Any,
        "ready media file not found",
    )
    .await?;
    if !classify::is_audio(&file) && !classify::is_video(&file) {
        return Err(revaro_core::ApiError::not_found(
            "ready media file not found",
        ));
    }
    let _lock = state.media.metadata_lock(&file.id).await;
    let file_id = file.id.clone();
    state
        .db
        .call_api(move |connection| {
            connection
                .execute("DELETE FROM media_metadata WHERE file_id = ?1", [&file_id])
                .map_err(crate::db::DbError::Query)
                .map_err(file_routes::database_error)
        })
        .await?;
    let probe = probe_file(&state, &file)
        .await
        .map_err(|_| revaro_core::ApiError::unprocessable("media re-analysis failed"))?;
    persist_metadata(&state, &file, &probe).await?;
    Ok(Json(revaro_core::api::media::ReanalyzeResult {
        status: "ready".to_owned(),
    }))
}

fn thumbnail_key(file: &File) -> String {
    if classify::is_video(file) {
        keys::video_thumbnail_key(&file.object_key)
    } else if classify::is_audio(file) {
        keys::audio_thumbnail_key(&file.object_key)
    } else {
        keys::image_thumbnail_key(&file.object_key)
    }
}

/// `GET /api/files/{id}/thumbnail`.
async fn thumbnail(
    State(state): State<Arc<AppState>>,
    _user: AuthUser,
    PathParam(id): PathParam<String>,
) -> Result<Response, revaro_core::ApiError> {
    let file = ready_media_file(state.clone(), id, MediaKind::Any, "ready file not found").await?;
    let typed_key = thumbnail_key(&file);
    if let Some(data) = read_thumbnail(&state, &typed_key).await {
        return Ok(thumbnail_response(data));
    }
    if !classify::is_video(&file) {
        let legacy_key = keys::thumbnail_v2_key(&file.object_key);
        if let Some(data) = read_thumbnail(&state, &legacy_key).await {
            if let Err(error) = state.store.put_immutable(&typed_key, &data).await {
                tracing::warn!(%error, "could not migrate legacy thumbnail key");
            }
            return Ok(thumbnail_response(data));
        }
    }
    if classify::is_audio(&file) {
        let data = generate_audio_thumbnail(&state, &file, &typed_key)
            .await
            .map_err(|error| {
                tracing::warn!(file = %file.id, %error, "audio thumbnail failed");
                revaro_core::ApiError::not_found("audio cover is unavailable")
            })?;
        return Ok(thumbnail_response(data));
    }
    if classify::is_video(&file) {
        schedule_video_thumbnail(state, file, typed_key);
        return Err(revaro_core::ApiError::not_found(
            "thumbnail is being generated",
        ));
    }
    let Some(data) = generate_still_thumbnail(&state, &file).await else {
        return Err(revaro_core::ApiError::not_found("no thumbnail available"));
    };
    if data.len() > MAX_THUMB_BYTES {
        return Err(revaro_core::ApiError::not_found("no thumbnail available"));
    }
    if let Err(error) = state.store.put_immutable(&typed_key, &data).await {
        tracing::warn!(%error, "could not persist generated thumbnail");
    }
    Ok(thumbnail_response(data))
}

async fn read_thumbnail(state: &Arc<AppState>, key: &str) -> Option<Vec<u8>> {
    match state.store.read(key, MAX_THUMB_BYTES).await {
        Ok(data) if is_jpeg(&data) => Some(data),
        _ => None,
    }
}

async fn generate_still_thumbnail(state: &Arc<AppState>, file: &File) -> Option<Vec<u8>> {
    let _permit = state.media.image_slots.clone().acquire_owned().await.ok()?;
    let source = if classify::is_epub_name(&file.name) {
        crate::reader_routes::load_book(state.clone(), file)
            .await
            .ok()?
            .cover
            .clone()
    } else if classify::is_image(file) {
        state
            .store
            .read(&file.object_key, MAX_THUMB_SOURCE)
            .await
            .ok()?
    } else {
        return None;
    };
    if source.is_empty() {
        return None;
    }
    tokio::task::spawn_blocking(move || {
        revaro_media::resize_image_to_jpeg(&source, THUMB_MAX_DIMENSION)
    })
    .await
    .ok()?
    .ok()
}

async fn generate_audio_thumbnail(
    state: &Arc<AppState>,
    file: &File,
    key: &str,
) -> Result<Vec<u8>, MediaError> {
    if let Some(data) = read_thumbnail(state, key).await {
        return Ok(data);
    }
    let _lock = state.media.thumbnail_lock(key).await;
    if let Some(data) = read_thumbnail(state, key).await {
        return Ok(data);
    }
    let probe = ensure_media_metadata(state.clone(), file.clone())
        .await
        .map_err(|error| MediaError::Input(error.message))?;
    if !probe.has_video_stream() {
        return Err(MediaError::NoArtwork);
    }
    let data = run_engine(
        state,
        file,
        Arc::clone(&state.media.light_slots),
        |engine, reader, cancel| engine.thumbnail(reader, THUMB_MAX_DIMENSION, true, cancel),
    )
    .await?;
    if data.len() > MAX_THUMB_BYTES || !is_jpeg(&data) {
        return Err(MediaError::InvalidData(
            "audio cover generator returned an invalid JPEG".to_owned(),
        ));
    }
    state
        .store
        .put_immutable(key, &data)
        .await
        .map_err(|error| MediaError::Input(error.to_string()))?;
    Ok(data)
}

fn is_jpeg(data: &[u8]) -> bool {
    data.len() >= 2 && data[..2] == [0xff, 0xd8]
}

fn thumbnail_response(data: Vec<u8>) -> Response {
    let etag = format!("\"{}\"", keys::sha256_hex(&data));
    let mut response = Response::new(Body::from(data.clone()));
    response
        .headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("image/jpeg"));
    response.headers_mut().insert(
        CONTENT_LENGTH,
        HeaderValue::from_str(&data.len().to_string()).expect("length is a valid header"),
    );
    response.headers_mut().insert(
        CACHE_CONTROL,
        HeaderValue::from_static("private, max-age=31536000, immutable"),
    );
    response.headers_mut().insert(
        ETAG,
        HeaderValue::from_str(&etag).expect("digest is a valid etag"),
    );
    response
}

fn schedule_video_thumbnail(state: Arc<AppState>, file: File, key: String) {
    if !state.media.claim_video_thumbnail(&key) {
        return;
    }
    tokio::spawn(async move {
        let result = async {
            if read_thumbnail(&state, &key).await.is_some() {
                return Ok::<(), MediaError>(());
            }
            let data = run_engine(
                &state,
                &file,
                Arc::clone(&state.media.light_slots),
                |engine, reader, cancel| {
                    engine.thumbnail(reader, THUMB_MAX_DIMENSION, false, cancel)
                },
            )
            .await?;
            if data.len() > MAX_THUMB_BYTES || !is_jpeg(&data) {
                return Err(MediaError::InvalidData(
                    "video thumbnail generator returned an invalid JPEG".to_owned(),
                ));
            }
            state
                .store
                .put_immutable(&key, &data)
                .await
                .map_err(|error| MediaError::Input(error.to_string()))?;
            Ok(())
        }
        .await;
        if let Err(error) = result {
            tracing::debug!(file = %file.id, %error, "background video thumbnail failed");
        }
        state.media.release_video_thumbnail(&key);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    use axum::body::Body;
    use http::{HeaderMap, Request, StatusCode};
    use http_body_util::BodyExt as _;
    use image::GenericImageView as _;
    use revaro_core::ids::ROOT_ID;
    use tower::ServiceExt as _;

    async fn state() -> Arc<AppState> {
        let root =
            std::env::temp_dir().join(format!("revaro-media-routes-{}", uuid::Uuid::new_v4()));
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
        let hash = crate::auth::token_hash("media-route-session");
        state
            .db
            .call(move |connection| {
                connection
                    .execute(
                        "INSERT OR REPLACE INTO settings(key,value,updated_at) \
                         VALUES('admin_username','admin','2024-01-01T00:00:00Z')",
                        [],
                    )
                    .map_err(crate::db::DbError::Query)?;
                connection
                    .execute(
                        "INSERT OR REPLACE INTO sessions(id,token_hash,created_at,expires_at) \
                         VALUES('media-session',?1,'2024-01-01T00:00:00Z','2999-01-01T00:00:00Z')",
                        [&hash],
                    )
                    .map_err(crate::db::DbError::Query)?;
                Ok(())
            })
            .await
            .expect("session inserts");
    }

    async fn request(
        state: &Arc<AppState>,
        method: &str,
        uri: &str,
    ) -> (StatusCode, HeaderMap, Vec<u8>) {
        authenticate(state).await;
        let mut builder = Request::builder().method(method).uri(uri).header(
            "cookie",
            format!("{}=media-route-session", crate::auth::SESSION_COOKIE),
        );
        if method != "GET" {
            builder = builder.header("origin", "http://localhost:8080");
        }
        let response = crate::router::build(state.clone())
            .oneshot(builder.body(Body::empty()).expect("request builds"))
            .await
            .expect("request completes");
        let status = response.status();
        let headers = response.headers().clone();
        let body = response
            .into_body()
            .collect()
            .await
            .expect("response body collects")
            .to_bytes()
            .to_vec();
        (status, headers, body)
    }

    async fn insert_ready_file(
        state: &Arc<AppState>,
        id: &str,
        name: &str,
        mime: &str,
        data: &[u8],
    ) {
        let object_key = keys::blob_key(id);
        state
            .store
            .put(&object_key, data)
            .await
            .expect("source object stores");
        let size = i64::try_from(data.len()).expect("fixture fits in sqlite integer");
        let id = id.to_owned();
        let name = name.to_owned();
        let mime = mime.to_owned();
        let etag = format!("etag-{id}");
        let object_key_for_db = object_key;
        state
            .db
            .call(move |connection| {
                connection
                    .execute(
                        "INSERT INTO files(id,parent_id,name,kind,object_key,size,mime_type,etag,status,created_at,updated_at) \
                         VALUES(?1,?2,?3,'file',?4,?5,?6,?7,'ready',?8,?8)",
                        rusqlite::params![
                            id,
                            ROOT_ID,
                            name,
                            object_key_for_db,
                            size,
                            mime,
                            etag,
                            Timestamp::now().to_rfc3339(),
                        ],
                    )
                    .map_err(crate::db::DbError::Query)?;
                Ok(())
            })
            .await
            .expect("file row inserts");
    }

    fn png_fixture() -> Vec<u8> {
        let image = image::RgbaImage::from_pixel(800, 400, image::Rgba([24, 96, 180, 255]));
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgba8(image)
            .write_to(&mut Cursor::new(&mut bytes), image::ImageFormat::Png)
            .expect("png encodes");
        bytes
    }

    fn wav_fixture() -> Vec<u8> {
        let sample_rate = 8_000u32;
        let pcm = vec![0u8; sample_rate as usize * 2];
        let data_size = u32::try_from(pcm.len()).expect("fixture fits in wav");
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&(36 + data_size).to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&sample_rate.to_le_bytes());
        bytes.extend_from_slice(&(sample_rate * 2).to_le_bytes());
        bytes.extend_from_slice(&2u16.to_le_bytes());
        bytes.extend_from_slice(&16u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&data_size.to_le_bytes());
        bytes.extend_from_slice(&pcm);
        bytes
    }

    #[tokio::test]
    async fn image_thumbnail_route_generates_and_reuses_a_persistent_jpeg() {
        let state = state().await;
        let source = png_fixture();
        insert_ready_file(&state, "image-1", "photo.png", "image/png", &source).await;

        let (status, headers, first) = request(&state, "GET", "/api/files/image-1/thumbnail").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(headers.get(CONTENT_TYPE).unwrap(), "image/jpeg");
        assert_eq!(
            headers.get(CACHE_CONTROL).unwrap(),
            "private, max-age=31536000, immutable"
        );
        assert_eq!(
            headers.get(CONTENT_LENGTH).unwrap().to_str().unwrap(),
            first.len().to_string()
        );
        let decoded = image::load_from_memory(&first).expect("thumbnail is a jpeg");
        assert_eq!(decoded.dimensions(), (640, 320));

        let key = keys::image_thumbnail_key("blobs/image-1");
        assert!(state.store.head(&key).await.is_ok());
        state
            .store
            .delete("blobs/image-1")
            .await
            .expect("source deletion succeeds");
        let (status, _, second) = request(&state, "GET", "/api/files/image-1/thumbnail").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            second, first,
            "the immutable thumbnail is served from storage"
        );
    }

    #[tokio::test]
    async fn reanalyze_route_probes_a_real_audio_object_and_persists_metadata() {
        let state = state().await;
        insert_ready_file(&state, "audio-1", "tone.wav", "audio/wav", &wav_fixture()).await;

        let (status, _, body) = request(&state, "POST", "/api/files/audio-1/media/reanalyze").await;
        assert_eq!(status, StatusCode::OK);
        let result: serde_json::Value = serde_json::from_slice(&body).expect("reanalyze json");
        assert_eq!(result, serde_json::json!({"status": "ready"}));

        let (status, _, body) = request(&state, "GET", "/api/files/audio-1/audio").await;
        assert_eq!(status, StatusCode::OK);
        let audio: serde_json::Value = serde_json::from_slice(&body).expect("audio json");
        assert_eq!(audio["duration"], 1.0);
        assert_eq!(audio["has_cover"], false);
        assert_eq!(audio["chapters"], serde_json::json!([]));

        let persisted = state
            .db
            .call(|connection| {
                connection
                    .query_row(
                        "SELECT probe_version,source_etag,audio_codec FROM media_metadata WHERE file_id='audio-1'",
                        [],
                        |row| {
                            Ok((
                                row.get::<_, i64>(0)?,
                                row.get::<_, String>(1)?,
                                row.get::<_, String>(2)?,
                            ))
                        },
                    )
                    .map_err(crate::db::DbError::Query)
            })
            .await
            .expect("metadata query succeeds");
        assert_eq!(persisted.0, MEDIA_PROBE_VERSION);
        assert_eq!(persisted.1, "etag-audio-1");
        assert_eq!(persisted.2, "pcm_s16le");
    }
}
