use std::{
    collections::HashSet,
    env,
    io::SeekFrom,
    path::{Component, PathBuf},
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

#[derive(Clone)]
pub struct BtState {
    api: Api,
}

impl BtState {
    pub async fn from_env() -> Result<Self, Box<dyn std::error::Error>> {
        let root = PathBuf::from(env::var("APP_WORK_DIR")?).join("revaro-bt");
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
}

#[derive(Serialize)]
pub struct ImportedFile {
    index: usize,
    key: String,
    size: i64,
    etag: String,
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
    let mut seen = HashSet::new();
    let mut imported = Vec::with_capacity(q.files.len());
    for requested in q.files {
        if !seen.insert(requested.index) || requested.key.is_empty() || requested.key.len() > 1024 {
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
        let mut path = root.clone();
        for component in components {
            let value = component
                .as_str()
                .ok_or_else(|| ApiError::internal("torrent file path is invalid"))?;
            let parsed = std::path::Path::new(value);
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
        let canonical = tokio::fs::canonicalize(&path)
            .await
            .map_err(ApiError::internal)?;
        if !canonical.starts_with(&root) {
            return Err(ApiError::bad_request("torrent file escapes download root"));
        }
        let metadata = tokio::fs::metadata(&canonical)
            .await
            .map_err(ApiError::internal)?;
        if !metadata.is_file() || metadata.len() != requested.size {
            return Err(ApiError::bad_request("torrent file size mismatch"));
        }
        let reader = tokio::fs::File::open(canonical)
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
        });
    }
    Ok(Json(imported))
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
