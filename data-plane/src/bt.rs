use std::{
    collections::HashSet,
    env,
    io::SeekFrom,
    path::{Component, Path as FsPath, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use axum::{
    Json,
    body::Body,
    extract::{Path, Query, State},
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use base64::Engine;
use librqbit::{
    AddTorrent, AddTorrentOptions, Api, Session, SessionOptions, SessionPersistenceConfig,
    api::TorrentIdOrHash,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio_util::io::ReaderStream;

use crate::{AppState, error::ApiError};

fn is_external_subtitle(name: &str) -> bool {
    matches!(
        FsPath::new(name)
            .extension()
            .and_then(|v| v.to_str())
            .map(|v| v.to_ascii_lowercase())
            .as_deref(),
        Some("ass" | "ssa" | "srt")
    )
}

fn subtitle_matches_video(video: &str, subtitle: &str) -> bool {
    let video_stem = FsPath::new(video)
        .file_stem()
        .and_then(|v| v.to_str())
        .unwrap_or("");
    let subtitle_stem = FsPath::new(subtitle)
        .file_stem()
        .and_then(|v| v.to_str())
        .unwrap_or("");
    if subtitle_stem.eq_ignore_ascii_case(video_stem) {
        return true;
    }
    if subtitle_stem.len() <= video_stem.len()
        || !subtitle_stem[..video_stem.len()].eq_ignore_ascii_case(video_stem)
    {
        return false;
    }
    let suffix = &subtitle_stem[video_stem.len()..];
    let Some(delimiter) = suffix.chars().next() else {
        return false;
    };
    if !matches!(delimiter, '.' | ' ' | '_' | '-' | '[' | '(') {
        return false;
    }
    !suffix[delimiter.len_utf8()..]
        .trim_start()
        .starts_with(|value: char| value.is_ascii_digit())
}

async fn canonical_torrent_file(root: &FsPath, file: &Value) -> Result<PathBuf, ApiError> {
    let components = file
        .get("components")
        .and_then(Value::as_array)
        .ok_or_else(|| ApiError::internal("torrent file path is missing"))?;
    let mut path = root.to_owned();
    for component in components {
        let value = component
            .as_str()
            .ok_or_else(|| ApiError::internal("torrent file path is invalid"))?;
        let parsed = FsPath::new(value);
        if value.is_empty()
            || value.contains(['/', '\\', '\0'])
            || parsed
                .components()
                .any(|part| !matches!(part, Component::Normal(_)))
        {
            return Err(ApiError::bad_request("unsafe torrent file path"));
        }
        path.push(value);
    }
    let canonical = tokio::fs::canonicalize(path)
        .await
        .map_err(ApiError::internal)?;
    if !canonical.starts_with(root) {
        return Err(ApiError::bad_request("torrent file escapes download root"));
    }
    Ok(canonical)
}

#[derive(Clone)]
pub struct BtState {
    api: Api,
}

impl BtState {
    pub async fn from_env() -> Result<Self, Box<dyn std::error::Error>> {
        let work_root = PathBuf::from(env::var("APP_WORK_DIR")?);
        let web_root = work_root.join("revaro-bt-web");
        if tokio::fs::try_exists(&web_root).await? {
            tokio::fs::remove_dir_all(&web_root).await?;
        }
        tokio::fs::create_dir_all(&web_root).await?;
        let root = work_root.join("revaro-bt");
        tokio::fs::create_dir_all(&root).await?;
        let blocklist = root.join("private-addresses.blocklist");
        tokio::fs::write(
            &blocklist,
            concat!(
                "reserved:0.0.0.0-0.255.255.255\n",
                "private:10.0.0.0-10.255.255.255\n",
                "shared:100.64.0.0-100.127.255.255\n",
                "loopback:127.0.0.0-127.255.255.255\n",
                "linklocal:169.254.0.0-169.254.255.255\n",
                "private:172.16.0.0-172.31.255.255\n",
                "private:192.168.0.0-192.168.255.255\n",
                "benchmark:198.18.0.0-198.19.255.255\n",
                "documentation:192.0.2.0-192.0.2.255\n",
                "documentation:198.51.100.0-198.51.100.255\n",
                "documentation:203.0.113.0-203.0.113.255\n",
                "multicast:224.0.0.0-255.255.255.255\n",
                "loopback6:::0-::1\n",
                "private6:fc00::-fdff:ffff:ffff:ffff:ffff:ffff:ffff:ffff\n",
                "linklocal6:fe80::-febf:ffff:ffff:ffff:ffff:ffff:ffff:ffff\n",
                "multicast6:ff00::-ffff:ffff:ffff:ffff:ffff:ffff:ffff:ffff\n",
            ),
        )
        .await?;
        let persistence = root.join("session");
        tokio::fs::create_dir_all(&persistence).await?;
        let session = Session::new_with_opts(
            root,
            SessionOptions {
                fastresume: true,
                persistence: Some(SessionPersistenceConfig::Json {
                    folder: Some(persistence),
                }),
                concurrent_init_limit: Some(2),
                runtime_worker_threads: Some(2),
                peer_limit: Some(40),
                disable_local_service_discovery: true,
                blocklist_url: Some(format!("file://{}", blocklist.to_string_lossy())),
                ..Default::default()
            },
        )
        .await?;
        Ok(Self {
            api: Api::new(session, None),
        })
    }
    pub async fn stop(&self) {
        self.api.session().stop().await;
    }
}

#[derive(Deserialize)]
pub struct AddRequest {
    source_type: String,
    source: String,
    selected: Option<Vec<usize>>,
    paused: Option<bool>,
}

pub async fn add(
    State(state): State<AppState>,
    Json(q): Json<AddRequest>,
) -> Result<Json<Value>, ApiError> {
    let add = match q.source_type.as_str() {
        "magnet" => AddTorrent::from_url(q.source),
        "torrent" => AddTorrent::from_bytes(
            base64::engine::general_purpose::STANDARD
                .decode(q.source)
                .map_err(|e| ApiError::bad_request(e.to_string()))?,
        ),
        _ => return Err(ApiError::bad_request("invalid torrent source type")),
    };
    let opts = AddTorrentOptions {
        paused: q.paused.unwrap_or(true),
        only_files: q.selected,
        overwrite: true,
        ..Default::default()
    };
    let response = state
        .bt
        .api
        .api_add_torrent(add, Some(opts))
        .await
        .map_err(|e| ApiError::upstream_domain("bt", e))?;
    Ok(Json(
        serde_json::to_value(response).map_err(ApiError::internal)?,
    ))
}

fn torrent_id(raw: &str) -> Result<TorrentIdOrHash, ApiError> {
    TorrentIdOrHash::parse(raw).map_err(|e| ApiError::bad_request(e.to_string()))
}

pub async fn details(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let result = state
        .bt
        .api
        .api_torrent_details(torrent_id(&id)?)
        .map_err(|e| ApiError::upstream_domain("bt", e))?;
    Ok(Json(
        serde_json::to_value(result).map_err(ApiError::internal)?,
    ))
}

pub async fn stats(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let result = state
        .bt
        .api
        .api_stats_v1(torrent_id(&id)?)
        .map_err(|e| ApiError::upstream_domain("bt", e))?;
    let value = serde_json::to_value(result).map_err(ApiError::internal)?;
    let progress = value
        .get("progress_bytes")
        .and_then(Value::as_u64)
        .unwrap_or_default();
    let total = value
        .get("total_bytes")
        .and_then(Value::as_u64)
        .unwrap_or_default();
    let speed = value
        .pointer("/live/download_speed/mbps")
        .and_then(Value::as_f64)
        .unwrap_or_default();
    let peers = value
        .pointer("/live/snapshot/peer_stats/live")
        .and_then(Value::as_u64)
        .or_else(|| {
            value
                .pointer("/live/snapshot/peer_stats/connected")
                .and_then(Value::as_u64)
        })
        .unwrap_or_default();
    Ok(Json(json!({
        "progress_bytes": progress,
        "total_bytes": total,
        "download_speed": (speed * 1024.0 * 1024.0) as u64,
        "peers": peers,
        "finished": value.get("finished").and_then(Value::as_bool).unwrap_or(progress >= total && total > 0),
    })))
}

#[derive(Deserialize)]
pub struct Selection {
    files: Vec<usize>,
}

pub async fn select(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(q): Json<Selection>,
) -> Result<StatusCode, ApiError> {
    state
        .bt
        .api
        .api_torrent_action_update_only_files(
            torrent_id(&id)?,
            &q.files.into_iter().collect::<HashSet<_>>(),
        )
        .await
        .map_err(|e| ApiError::upstream_domain("bt", e))?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn start(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    state
        .bt
        .api
        .api_torrent_action_start(torrent_id(&id)?)
        .await
        .map_err(|e| ApiError::upstream_domain("bt", e))?;
    Ok(StatusCode::NO_CONTENT)
}
pub async fn pause(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    state
        .bt
        .api
        .api_torrent_action_pause(torrent_id(&id)?)
        .await
        .map_err(|e| ApiError::upstream_domain("bt", e))?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct ImportRequest {
    files: Vec<ImportFile>,
}

#[derive(Deserialize)]
struct ImportFile {
    index: usize,
    key: String,
    mime: String,
    size: u64,
    #[serde(default)]
    web_prefix: String,
}

#[derive(Serialize)]
pub struct ImportedFile {
    index: usize,
    key: String,
    size: i64,
    etag: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    web_media: Option<WebMediaAsset>,
}

#[derive(Serialize)]
struct WebMediaSubtitle {
    index: usize,
    key: String,
    size: i64,
    etag: String,
    language: String,
    title: String,
    default: bool,
    forced: bool,
}
#[derive(Serialize)]
struct WebMediaAsset {
    state: String,
    error: String,
    key: String,
    size: i64,
    etag: String,
    duration_ms: i64,
    video_codec: String,
    audio_codec: String,
    subtitles: Vec<WebMediaSubtitle>,
}
static NEXT_PREP: AtomicU64 = AtomicU64::new(1);
struct PrepCancel(tokio_util::sync::CancellationToken);
impl PrepCancel {
    fn disarm(&mut self) {
        self.0 = tokio_util::sync::CancellationToken::new();
    }
}
impl Drop for PrepCancel {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

struct WorkCleanup(PathBuf);
impl Drop for WorkCleanup {
    fn drop(&mut self) {
        let path = self.0.clone();
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                let _ = tokio::fs::remove_dir_all(path).await;
            });
        }
    }
}

struct UploadedCleanup {
    s3: crate::s3::S3State,
    keys: Vec<String>,
    armed: bool,
}
impl UploadedCleanup {
    fn disarm(&mut self) {
        self.armed = false;
    }
}
impl Drop for UploadedCleanup {
    fn drop(&mut self) {
        if !self.armed || self.keys.is_empty() {
            return;
        }
        let s3 = self.s3.clone();
        let keys = std::mem::take(&mut self.keys);
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                for key in keys {
                    let _ = s3
                        .client
                        .delete_object()
                        .bucket(&s3.bucket)
                        .key(key)
                        .send()
                        .await;
                }
            });
        }
    }
}

pub async fn import(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(q): Json<ImportRequest>,
) -> Result<Json<Vec<ImportedFile>>, ApiError> {
    if q.files.is_empty() || q.files.len() > 10_000 {
        return Err(ApiError::bad_request("invalid torrent import file count"));
    }
    let details = state
        .bt
        .api
        .api_torrent_details(torrent_id(&id)?)
        .map_err(|e| ApiError::upstream_domain("bt", e))?;
    let details = serde_json::to_value(details).map_err(ApiError::internal)?;
    let files = details
        .get("files")
        .and_then(Value::as_array)
        .ok_or_else(|| ApiError::internal("torrent details contain no files"))?;
    let output = details
        .get("output_folder")
        .and_then(Value::as_str)
        .ok_or_else(|| ApiError::internal("torrent details contain no output folder"))?;
    let root = tokio::fs::canonicalize(output)
        .await
        .map_err(|e| ApiError::upstream_domain("bt", e))?;
    // Resolve selected sidecar subtitles before consuming the request. A Web
    // media video is published under a derived MP4 key, so sibling subtitle
    // blobs cannot be discovered by the legacy file-neighbour lookup later.
    // Convert and attach them while the torrent staging tree is still present.
    let mut external_subtitles = Vec::new();
    for requested in &q.files {
        let Some(file) = files.get(requested.index) else {
            continue;
        };
        let Some(name) = file
            .get("components")
            .and_then(Value::as_array)
            .and_then(|parts| parts.last())
            .and_then(Value::as_str)
        else {
            continue;
        };
        if !is_external_subtitle(name) {
            continue;
        }
        let path = canonical_torrent_file(&root, file).await?;
        external_subtitles.push((requested.index, name.to_owned(), path));
    }
    let mut seen = HashSet::new();
    let mut imported = Vec::with_capacity(q.files.len());
    for requested in q.files {
        if !seen.insert(requested.index)
            || requested.key.len() > 1024
            || (requested.web_prefix.is_empty() && requested.key.is_empty())
        {
            return Err(ApiError::bad_request(
                "invalid or duplicate torrent import file",
            ));
        }
        let file = files
            .get(requested.index)
            .ok_or_else(|| ApiError::bad_request("torrent file index out of range"))?;
        let components = file
            .get("components")
            .and_then(Value::as_array)
            .ok_or_else(|| ApiError::internal("torrent file path is missing"))?;
        let canonical = canonical_torrent_file(&root, file).await?;
        let metadata = tokio::fs::metadata(&canonical)
            .await
            .map_err(ApiError::internal)?;
        if !metadata.is_file() || metadata.len() != requested.size {
            return Err(ApiError::bad_request("torrent file size mismatch"));
        }
        let web_media = if requested.web_prefix.is_empty() {
            None
        } else {
            if requested.web_prefix.len() > 900
                || requested.web_prefix.contains("..")
                || !requested.web_prefix.starts_with("derived/media/")
            {
                return Err(ApiError::bad_request("invalid Web media prefix"));
            }
            let video_name = components.last().and_then(Value::as_str).unwrap_or("");
            let sidecars = external_subtitles
                .iter()
                .filter(|(_, name, _)| subtitle_matches_video(video_name, name))
                .map(|(index, name, path)| (*index, name.clone(), path.clone()))
                .collect();
            Some(prepare_and_upload_web(&state, &canonical, &requested.web_prefix, sidecars).await?)
        };
        if let Some(web) = web_media {
            imported.push(ImportedFile {
                index: requested.index,
                key: web.key.clone(),
                size: web.size,
                etag: web.etag.clone(),
                web_media: Some(web),
            });
        } else {
            let reader = tokio::fs::File::open(&canonical)
                .await
                .map_err(ApiError::internal)?;
            let object = state
                .s3
                .store_reader(
                    &requested.key,
                    Some(&requested.mime),
                    reader,
                    Some(requested.size),
                )
                .await?;
            imported.push(ImportedFile {
                index: requested.index,
                key: requested.key,
                size: object.size,
                etag: object.etag,
                web_media: None,
            });
        }
    }
    Ok(Json(imported))
}

async fn prepare_and_upload_web(
    state: &AppState,
    source: &std::path::Path,
    prefix: &str,
    external_subtitles: Vec<(usize, String, PathBuf)>,
) -> Result<WebMediaAsset, ApiError> {
    let serial = NEXT_PREP.fetch_add(1, Ordering::Relaxed);
    let work = PathBuf::from(env::var("APP_WORK_DIR").map_err(ApiError::internal)?)
        .join("revaro-bt-web")
        .join(format!("{}-{serial}", std::process::id()));
    tokio::fs::create_dir(&work)
        .await
        .map_err(ApiError::internal)?;
    let _work_cleanup = WorkCleanup(work.clone());
    let output = work.join("playback.mp4");
    let source = source.to_owned();
    let output_for_task = output.clone();
    let cancel = state.shutdown.child_token();
    let mut cancel_guard = PrepCancel(cancel.clone());
    let prepared = tokio::task::spawn_blocking(move || {
        let mut prepared =
            crate::media::prepare_web_media(&source, &output_for_task, cancel.clone())?;
        for (file_index, name, path) in external_subtitles {
            let data = crate::media::external_subtitle_path(&path, cancel.clone())?;
            prepared.subtitles.push((
                crate::media::external_subtitle_meta(1_000_000 + file_index, &name),
                data,
            ));
        }
        Ok::<_, String>(prepared)
    })
    .await
    .map_err(ApiError::internal)?;
    cancel_guard.disarm();
    let prepared = match prepared {
        Ok(value) => value,
        Err(error) if error.starts_with("unsupported ") => {
            return Ok(WebMediaAsset {
                state: "unsupported".into(),
                error,
                key: String::new(),
                size: 0,
                etag: String::new(),
                duration_ms: 0,
                video_codec: String::new(),
                audio_codec: String::new(),
                subtitles: vec![],
            });
        }
        Err(error) => {
            return Err(ApiError::upstream_domain("media", error));
        }
    };
    let playback_key = format!("{prefix}/playback.mp4");
    let playback_len = tokio::fs::metadata(&output)
        .await
        .map_err(ApiError::internal)?
        .len();
    let mut cleanup = UploadedCleanup {
        s3: state.s3.clone(),
        keys: vec![playback_key.clone()],
        armed: true,
    };
    let playback = state
        .s3
        .store_reader(
            &playback_key,
            Some("video/mp4"),
            tokio::fs::File::open(&output)
                .await
                .map_err(ApiError::internal)?,
            Some(playback_len),
        )
        .await?;
    let result: Result<Vec<WebMediaSubtitle>, ApiError> = async {
        let mut tracks = Vec::new();
        for (meta, bytes) in prepared.subtitles {
            let key = format!("{prefix}/subtitles/{}.vtt", meta.index);
            cleanup.keys.push(key.clone());
            let local = work.join(format!("subtitle-{}.vtt", meta.index));
            tokio::fs::write(&local, &bytes)
                .await
                .map_err(ApiError::internal)?;
            let object = state
                .s3
                .store_reader(
                    &key,
                    Some("text/vtt; charset=utf-8"),
                    tokio::fs::File::open(&local)
                        .await
                        .map_err(ApiError::internal)?,
                    Some(bytes.len() as u64),
                )
                .await?;
            tracks.push(WebMediaSubtitle {
                index: meta.index,
                key,
                size: object.size,
                etag: object.etag,
                language: meta.language,
                title: meta.title,
                default: meta.default,
                forced: meta.forced,
            });
        }
        for key in &cleanup.keys {
            state
                .s3
                .client
                .head_object()
                .bucket(&state.s3.bucket)
                .key(key)
                .send()
                .await
                .map_err(ApiError::upstream)?;
        }
        Ok(tracks)
    }
    .await;
    let subtitles = match result {
        Ok(value) => value,
        Err(error) => return Err(error),
    };
    cleanup.disarm();
    Ok(WebMediaAsset {
        state: "completed".into(),
        error: String::new(),
        key: playback_key,
        size: playback.size,
        etag: playback.etag,
        duration_ms: prepared.duration_ms,
        video_codec: prepared.video_codec,
        audio_codec: if prepared.audio_codec == "aac" {
            "aac".into()
        } else if prepared.audio_codec.is_empty() {
            String::new()
        } else {
            "aac".into()
        },
        subtitles,
    })
}

pub async fn delete(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    state
        .bt
        .api
        .api_torrent_action_delete(torrent_id(&id)?)
        .await
        .map_err(|e| ApiError::upstream_domain("bt", e))?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct StreamQuery {
    start: Option<u64>,
    end: Option<u64>,
}

// stream serves one torrent file as it downloads. Seeking the underlying
// librqbit FileStream moves its read position, which in turn makes librqbit
// prioritize the pieces around the requested byte offset (a ~32 MiB lookahead
// window), so a browser seeking into the middle of a still-downloading file
// gets those pieces first.
pub async fn stream(
    State(state): State<AppState>,
    Path((id, file_id)): Path<(String, usize)>,
    Query(q): Query<StreamQuery>,
) -> Result<Response, ApiError> {
    let mut file = state
        .bt
        .api
        .api_stream(torrent_id(&id)?, file_id)
        .await
        .map_err(|e| ApiError::upstream_domain("bt", e))?;
    let len = file.len();
    if len == 0 {
        let mut response = Body::empty().into_response();
        response
            .headers_mut()
            .insert(header::CONTENT_LENGTH, HeaderValue::from_static("0"));
        response
            .headers_mut()
            .insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/octet-stream"),
        );
        return Ok(response);
    }
    let start = q.start.unwrap_or(0);
    if start >= len {
        return Err(ApiError::range_not_satisfiable(
            "invalid torrent stream start",
        ));
    }
    let end = q.end.map(|value| value.min(len - 1)).unwrap_or(len - 1);
    if end < start {
        return Err(ApiError::range_not_satisfiable(
            "invalid torrent stream range",
        ));
    }
    let length = end - start + 1;
    file.seek(SeekFrom::Start(start))
        .await
        .map_err(|e| ApiError::upstream_domain("bt", e))?;
    let mut response = Body::from_stream(ReaderStream::new(file.take(length))).into_response();
    *response.status_mut() = if q.start.is_some() || q.end.is_some() {
        StatusCode::PARTIAL_CONTENT
    } else {
        StatusCode::OK
    };
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_LENGTH,
        HeaderValue::from_str(&length.to_string()).unwrap(),
    );
    headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    if q.start.is_some() || q.end.is_some() {
        headers.insert(
            header::CONTENT_RANGE,
            HeaderValue::from_str(&format!("bytes {start}-{end}/{len}")).unwrap(),
        );
    }
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    Ok(response)
}

#[cfg(test)]
mod tests {
    use std::{net::Ipv4Addr, time::Duration};

    use anyhow::Context;
    use librqbit::{
        CreateTorrentOptions, ListenerOptions, Session, SessionOptions, SessionPersistenceConfig,
        create_torrent, spawn_utils::BlockingSpawner,
    };
    use tempfile::TempDir;
    use tokio::{
        io::{AsyncReadExt, AsyncSeekExt},
        time::timeout,
    };

    use super::*;

    #[test]
    fn external_subtitle_sidecars_match_only_their_video() {
        assert!(is_external_subtitle("Movie.zh-Hans.ass"));
        assert!(is_external_subtitle("Movie.srt"));
        assert!(!is_external_subtitle("Movie.mka"));
        assert!(subtitle_matches_video("Movie.mkv", "Movie.ass"));
        assert!(subtitle_matches_video("Movie.mkv", "Movie.zh-Hans.srt"));
        assert!(!subtitle_matches_video("Movie.mkv", "Movie 2.zh-Hans.ass"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn progressive_seek_and_fastresume_survive_restart() -> anyhow::Result<()> {
        timeout(Duration::from_secs(30), async {
            let seed = TempDir::new()?;
            let content: Vec<u8> = (0..8 * 1024 * 1024).map(|i| (i % 251) as u8).collect();
            std::fs::write(seed.path().join("fixture.bin"), &content)?;
            let torrent = create_torrent(
                seed.path(),
                CreateTorrentOptions {
                    piece_length: Some(64 << 10),
                    ..Default::default()
                },
                &BlockingSpawner::new(1),
            )
            .await?;
            let torrent_bytes = torrent.as_bytes()?;

            let seeder = Session::new_with_opts(
                seed.path().into(),
                SessionOptions {
                    dht: None,
                    persistence: None,
                    listen: Some(ListenerOptions {
                        listen_addr: (Ipv4Addr::LOCALHOST, 0).into(),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            )
            .await?;
            seeder
                .add_torrent(
                    AddTorrent::from_bytes(torrent_bytes.clone()),
                    Some(AddTorrentOptions {
                        paused: false,
                        output_folder: Some(seed.path().to_string_lossy().into_owned()),
                        overwrite: true,
                        ..Default::default()
                    }),
                )
                .await?
                .into_handle()
                .context("missing seeder handle")?
                .wait_until_completed()
                .await?;
            let peer = seeder.listen_addr().context("seeder did not listen")?;

            let client = TempDir::new()?;
            let state = client.path().join("session");
            std::fs::create_dir_all(&state)?;
            let options = || SessionOptions {
                dht: None,
                fastresume: true,
                persistence: Some(SessionPersistenceConfig::Json {
                    folder: Some(state.clone()),
                }),
                concurrent_init_limit: Some(1),
                runtime_worker_threads: Some(2),
                peer_limit: Some(4),
                ..Default::default()
            };
            let downloader = Session::new_with_opts(client.path().into(), options()).await?;
            let handle = downloader
                .add_torrent(
                    AddTorrent::from_bytes(torrent_bytes.clone()),
                    Some(AddTorrentOptions {
                        paused: false,
                        initial_peers: Some(vec![peer]),
                        overwrite: true,
                        ..Default::default()
                    }),
                )
                .await?
                .into_handle()
                .context("missing downloader handle")?;
            handle.wait_until_initialized().await?;
            let offset = content.len() as u64 - (96 << 10);
            let mut stream = handle.clone().stream(0).await?;
            stream.seek(SeekFrom::Start(offset)).await?;
            let mut tail = vec![0; 64 << 10];
            stream.read_exact(&mut tail).await?;
            assert_eq!(tail, content[offset as usize..offset as usize + tail.len()]);
            drop(stream);
            drop(handle);
            downloader.stop().await;
            drop(downloader);

            let restored = Session::new_with_opts(client.path().into(), options()).await?;
            let restored_handle = restored
                .add_torrent(
                    AddTorrent::from_bytes(torrent_bytes),
                    Some(AddTorrentOptions {
                        paused: false,
                        initial_peers: Some(vec![peer]),
                        overwrite: true,
                        ..Default::default()
                    }),
                )
                .await?
                .into_handle()
                .context("missing restored handle")?;
            restored_handle.wait_until_initialized().await?;
            let mut restored_stream = restored_handle.stream(0).await?;
            restored_stream.seek(SeekFrom::Start(offset)).await?;
            let mut restored_tail = vec![0; 64 << 10];
            restored_stream.read_exact(&mut restored_tail).await?;
            assert_eq!(restored_tail, tail);
            restored.stop().await;
            seeder.stop().await;
            anyhow::Ok(())
        })
        .await??;
        Ok(())
    }
}
