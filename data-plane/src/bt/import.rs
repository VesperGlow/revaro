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
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    consumed: bool,
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
    let requested_count = q.files.len();
    let mut requested_files = q.files;
    // Process videos before sidecars even when torrent order lists subtitles
    // first, so successfully embedded sidecars can be omitted from storage.
    requested_files.sort_by_key(|file| file.web_prefix.is_empty());
    let mut consumed_subtitles = HashSet::new();
    let mut seen = HashSet::new();
    let mut imported = Vec::with_capacity(requested_count);
    for requested in requested_files {
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
        if consumed_subtitles.contains(&requested.index) {
            imported.push(ImportedFile {
                index: requested.index,
                key: String::new(),
                size: 0,
                etag: String::new(),
                consumed: true,
                web_media: None,
            });
            continue;
        }
        if !requested.web_prefix.is_empty() {
            if requested.web_prefix.len() > 900
                || requested.web_prefix.contains("..")
                || !requested.web_prefix.starts_with("derived/media/")
            {
                return Err(ApiError::bad_request("invalid Web media prefix"));
            }
            let video_name = components.last().and_then(Value::as_str).unwrap_or("");
            let sidecars = external_subtitles
                .iter()
                .filter(|(_, name, path)| {
                    path.parent() == canonical.parent() && subtitle_matches_video(video_name, name)
                })
                .map(|(index, name, path)| (*index, name.clone(), path.clone()))
                .collect::<Vec<_>>();
            let embed_sidecars = canonical
                .extension()
                .and_then(|value| value.to_str())
                .is_some_and(|value| value.eq_ignore_ascii_case("mkv"));
            let embedded_indices: Vec<_> = if embed_sidecars {
                sidecars.iter().map(|(index, _, _)| *index).collect()
            } else {
                Vec::new()
            };
            let (web, object) = prepare_and_upload_web(
                &state,
                &canonical,
                &requested.key,
                &requested.mime,
                &requested.web_prefix,
                sidecars,
                embed_sidecars,
            )
            .await?;
            consumed_subtitles.extend(embedded_indices);
            imported.push(ImportedFile {
                index: requested.index,
                key: requested.key,
                size: object.size,
                etag: object.etag,
                consumed: false,
                web_media: Some(web),
            });
            continue;
        }
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
            consumed: false,
            web_media: None,
        });
    }
    Ok(Json(imported))
}

async fn prepare_and_upload_web(
    state: &AppState,
    source: &std::path::Path,
    source_key: &str,
    source_mime: &str,
    prefix: &str,
    external_subtitles: Vec<(usize, String, PathBuf)>,
    embed_sidecars: bool,
) -> Result<(WebMediaAsset, crate::s3::ObjectInfo), ApiError> {
    let serial = NEXT_PREP.fetch_add(1, Ordering::Relaxed);
    let work = PathBuf::from(env::var("APP_WORK_DIR").map_err(ApiError::internal)?)
        .join("revaro-bt-web")
        .join(format!("{}-{serial}", std::process::id()));
    tokio::fs::create_dir(&work)
        .await
        .map_err(ApiError::internal)?;
    let _work_cleanup = WorkCleanup(work.clone());
    let output = work.join("playback.mp4");
    let remuxed = work.join("source.mkv");
    let source = source.to_owned();
    let media_source = if external_subtitles.is_empty() || !embed_sidecars {
        source.clone()
    } else {
        let source_for_task = source.clone();
        let remuxed_for_task = remuxed.clone();
        let subtitles_for_task = external_subtitles.clone();
        tokio::task::spawn_blocking(move || {
            crate::media::remux_mkv_sidecars(
                &source_for_task,
                &remuxed_for_task,
                &subtitles_for_task,
            )
        })
        .await
        .map_err(ApiError::internal)?
        .map_err(|e| ApiError::upstream_domain("media", e))?;
        remuxed.clone()
    };
    let source_for_upload = media_source.clone();
    let web_sidecars = if embed_sidecars {
        Vec::new()
    } else {
        external_subtitles
    };
    let output_for_task = output.clone();
    let cancel = state.shutdown.child_token();
    let mut cancel_guard = PrepCancel(cancel.clone());
    let prepared = tokio::task::spawn_blocking(move || {
        let mut prepared =
            crate::media::prepare_web_media(&media_source, &output_for_task, cancel.clone())?;
        for (file_index, name, path) in web_sidecars {
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
            return Ok((
                WebMediaAsset {
                    state: "unsupported".into(),
                    error,
                    key: String::new(),
                    size: 0,
                    etag: String::new(),
                    duration_ms: 0,
                    video_codec: String::new(),
                    audio_codec: String::new(),
                    subtitles: vec![],
                },
                crate::s3::ObjectInfo {
                    size: 0,
                    etag: String::new(),
                },
            ));
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
        keys: vec![source_key.to_owned(), playback_key.clone()],
        armed: true,
    };
    let source_len = tokio::fs::metadata(&source_for_upload)
        .await
        .map_err(ApiError::internal)?
        .len();
    let source_object = state
        .s3
        .store_reader(
            source_key,
            Some(source_mime),
            tokio::fs::File::open(&source_for_upload)
                .await
                .map_err(ApiError::internal)?,
            Some(source_len),
        )
        .await?;
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
    Ok((
        WebMediaAsset {
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
        },
        source_object,
    ))
}

