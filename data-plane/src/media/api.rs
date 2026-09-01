fn external_subtitle_language(name: &str) -> String {
    let stem = Path::new(name)
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or(name)
        .to_ascii_lowercase();
    let normalized = stem.replace(['_', ' '], "-");
    if has_subtitle_language_token(
        &normalized,
        &["cht", "tc", "traditional", "zh-tw", "zh-hk", "zh-hant"],
    ) {
        "cht".into()
    } else if has_subtitle_language_token(
        &normalized,
        &["chs", "sc", "simplified", "zh-cn", "zh-sg", "zh-hans", "zh"],
    ) {
        "chs".into()
    } else if ["ja", "jp", "jpn", "japanese"]
        .iter()
        .any(|token| normalized.split(['.', '-']).any(|part| part == *token))
    {
        "jpn".into()
    } else if ["en", "eng", "english"]
        .iter()
        .any(|token| normalized.split(['.', '-']).any(|part| part == *token))
    {
        "eng".into()
    } else if ["ko", "kr", "kor", "korean"]
        .iter()
        .any(|token| normalized.split(['.', '-']).any(|part| part == *token))
    {
        "kor".into()
    } else {
        "und".into()
    }
}

fn has_subtitle_language_token(value: &str, tokens: &[&str]) -> bool {
    tokens.iter().any(|token| {
        value.split('.').any(|part| part == *token) || value.ends_with(&format!("-{token}"))
    })
}

pub(crate) fn external_subtitle_meta(index: usize, name: &str) -> Subtitle {
    let stem = Path::new(name)
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or(name);
    Subtitle {
        index,
        codec: Path::new(name)
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or("")
            .to_ascii_lowercase(),
        language: external_subtitle_language(name),
        title: stem.to_owned(),
        default: false,
        forced: false,
    }
}

pub(crate) fn remux_mkv_sidecars(
    source: &Path,
    output: &Path,
    subtitles: &[(usize, String, std::path::PathBuf)],
) -> Result<(), String> {
    if subtitles.is_empty() {
        return Err("MKV subtitle remux requires at least one sidecar".into());
    }
    ffmpeg::init().map_err(|e| e.to_string())?;
    let input = format::input(source).map_err(|e| format!("open MKV for subtitle remux: {e}"))?;
    let original_streams = input.nb_streams() as usize;
    let original_subtitles = input
        .streams()
        .filter(|stream| stream.parameters().medium() == Type::Subtitle)
        .count();
    let original_av: Vec<_> = input
        .streams()
        .filter(|stream| matches!(stream.parameters().medium(), Type::Video | Type::Audio))
        .map(|stream| (stream.parameters().medium(), stream.parameters().id()))
        .collect();
    drop(input);

    let mut command = Command::new("ffmpeg");
    command.args(["-hide_banner", "-loglevel", "error", "-nostdin", "-i"]);
    command.arg(source);
    for (_, _, path) in subtitles {
        command.arg("-i").arg(path);
    }
    command.args(["-map", "0"]);
    for input_index in 1..=subtitles.len() {
        command.args(["-map", &format!("{input_index}:s:0")]);
    }
    command.args(["-map_metadata", "0", "-map_chapters", "0", "-c", "copy"]);
    for (offset, (_, name, _)) in subtitles.iter().enumerate() {
        let stream_index = original_subtitles + offset;
        let title = Path::new(name)
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or(name);
        command
            .arg(format!("-metadata:s:s:{stream_index}"))
            .arg(format!("language={}", external_subtitle_language(name)))
            .arg(format!("-metadata:s:s:{stream_index}"))
            .arg(format!("title={title}"));
        if Path::new(name)
            .extension()
            .and_then(|value| value.to_str())
            .is_some_and(|value| value.eq_ignore_ascii_case("vtt"))
        {
            command.arg(format!("-c:s:{stream_index}")).arg("srt");
        }
    }
    command.arg("-y").arg(output);
    let result = command
        .output()
        .map_err(|e| format!("start ffmpeg MKV subtitle remux: {e}"))?;
    if !result.status.success() {
        return Err(format!(
            "ffmpeg MKV subtitle remux failed: {}",
            String::from_utf8_lossy(&result.stderr).trim()
        ));
    }
    let remuxed = format::input(output).map_err(|e| format!("validate remuxed MKV: {e}"))?;
    if remuxed.nb_streams() as usize != original_streams + subtitles.len() {
        return Err("remuxed MKV stream count mismatch".into());
    }
    let remuxed_av: Vec<_> = remuxed
        .streams()
        .filter(|stream| matches!(stream.parameters().medium(), Type::Video | Type::Audio))
        .map(|stream| (stream.parameters().medium(), stream.parameters().id()))
        .collect();
    if remuxed_av != original_av {
        return Err("remuxed MKV changed video or audio codecs".into());
    }
    Ok(())
}

pub async fn probe(
    State(state): State<AppState>,
    Json(q): Json<ProbeRequest>,
) -> Result<Json<ProbeResponse>, ApiError> {
    let _permit = state
        .media_light_slots
        .clone()
        .acquire_owned()
        .await
        .map_err(ApiError::internal)?;
    let cancel = state.shutdown.child_token();
    let reader = state.s3.range_reader(q.key, cancel.clone()).await?;
    let mut guard = CancelOnDrop(cancel.clone());
    let result = task::spawn_blocking(move || probe_blocking(reader, cancel))
        .await
        .map_err(ApiError::internal)?
        .map_err(ApiError::bad_request)?;
    guard.disarm();
    Ok(Json(result))
}

pub async fn thumbnail(
    State(state): State<AppState>,
    Json(q): Json<ThumbnailRequest>,
) -> Result<Response, ApiError> {
    let max_dimension = q.max_dimension.unwrap_or(480).clamp(64, 2048);
    let _permit = state
        .media_light_slots
        .clone()
        .acquire_owned()
        .await
        .map_err(ApiError::internal)?;
    let cancel = state.shutdown.child_token();
    let reader = state.s3.range_reader(q.key, cancel.clone()).await?;
    let mut guard = CancelOnDrop(cancel.clone());
    let attached_picture_only = q.attached_picture_only.unwrap_or(false);
    let jpeg = task::spawn_blocking(move || {
        thumbnail_blocking(reader, max_dimension, attached_picture_only, cancel)
    })
    .await
    .map_err(ApiError::internal)?
    .map_err(|error| {
        if error == "media has no attached picture" {
            ApiError::not_found("artwork", error)
        } else {
            ApiError::bad_request(error)
        }
    })?;
    guard.disarm();
    let mut response = Body::from(jpeg).into_response();
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static("image/jpeg"));
    Ok(response)
}

pub async fn fmp4(
    State(state): State<AppState>,
    Json(q): Json<Fmp4Request>,
) -> Result<Response, ApiError> {
    let transcode_video = q.transcode_video.unwrap_or(false);
    let transcode_audio = q.transcode_audio.unwrap_or(false);
    let slots = if transcode_video || transcode_audio {
        &state.media_heavy_slots
    } else {
        &state.media_stream_slots
    };
    let permit = slots
        .clone()
        .acquire_owned()
        .await
        .map_err(ApiError::internal)?;
    let cancel = state.shutdown.child_token();
    let reader = state.s3.range_reader(q.key, cancel.clone()).await?;
    let (tx, rx) = mpsc::channel::<Result<bytes::Bytes, io::Error>>(4);
    let start = q.start_seconds.unwrap_or(0.0).max(0.0);
    let include_audio = q.include_audio.unwrap_or(true);
    task::spawn_blocking(move || {
        let _permit = permit;
        let result = remux_fmp4(
            reader,
            ChannelWriter {
                tx: tx.clone(),
                cancel: cancel.clone(),
            },
            start,
            include_audio,
            transcode_video,
            transcode_audio,
            cancel,
        );
        if let Err(error) = result {
            let _ = tx.blocking_send(Err(io::Error::other(error)));
        }
    });
    let mut response = Body::from_stream(ReceiverStream::new(rx)).into_response();
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static("video/mp4"));
    Ok(response)
}

pub async fn hls(
    State(state): State<AppState>,
    Json(q): Json<HlsRequest>,
) -> Result<Json<HlsResponse>, ApiError> {
    let work_root = tokio::fs::canonicalize(env::var("APP_WORK_DIR").map_err(ApiError::internal)?)
        .await
        .map_err(ApiError::internal)?;
    let output_dir = tokio::fs::canonicalize(&q.output_dir)
        .await
        .map_err(|error| ApiError::bad_request(error.to_string()))?;
    if !output_dir.starts_with(work_root) {
        return Err(ApiError::bad_request(
            "HLS output directory is outside work root",
        ));
    }
    let cancel = state.shutdown.child_token();
    let reader = state.s3.range_reader(q.key, cancel.clone()).await?;
    let id = format!("hls-{}", NEXT_HLS_JOB.fetch_add(1, Ordering::Relaxed));
    let job = Arc::new(HlsJob {
        cancel: cancel.clone(),
        state: Mutex::new(HlsJobState::default()),
    });
    let mut request_guard = CancelOnDrop(cancel.clone());
    state
        .hls_jobs
        .lock()
        .map_err(ApiError::internal)?
        .insert(id.clone(), job.clone());
    let heavy_slots = state.media_heavy_slots.clone();
    let start = q.start_seconds.unwrap_or_default();
    let audio_only = q.audio_only.unwrap_or(false);
    let playlist = output_dir.join("index.m3u8");
    let worker_job = job.clone();
    task::spawn(async move {
        let result = match heavy_slots.acquire_owned().await {
            Ok(permit) => task::spawn_blocking(move || {
                let _permit = permit;
                remux_hls(reader, &output_dir, start, audio_only, cancel)
            })
            .await
            .map_err(|error| error.to_string())
            .and_then(|value| value),
            Err(error) => Err(error.to_string()),
        };
        let mut status = worker_job
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        status.done = true;
        if let Err(error) = result {
            status.error = error;
        }
    });

    // Return as soon as a playable prefix exists. The worker, heavy permit and
    // Range reader remain owned by the job rather than by this HTTP request.
    let startup_segments = if audio_only { 1 } else { 2 };
    let deadline = tokio::time::Instant::now() + Duration::from_secs(90);
    loop {
        if playlist_segment_count(&playlist).await >= startup_segments {
            request_guard.disarm();
            return Ok(Json(HlsResponse {
                duration_ms: 0,
                video_codec: String::new(),
                audio_codec: String::new(),
                transcoding: false,
                job_id: id,
            }));
        }
        let (done, job_error) = {
            let status = job.state.lock().map_err(ApiError::internal)?;
            (status.done, status.error.clone())
        };
        if done {
            if playlist_segment_count(&playlist).await > 0 && job_error.is_empty() {
                request_guard.disarm();
                return Ok(Json(HlsResponse {
                    duration_ms: 0,
                    video_codec: String::new(),
                    audio_codec: String::new(),
                    transcoding: false,
                    job_id: id,
                }));
            }
            let error = if job_error.is_empty() {
                "data plane produced no HLS segments".into()
            } else {
                job_error
            };
            state
                .hls_jobs
                .lock()
                .map_err(ApiError::internal)?
                .remove(&id);
            return Err(ApiError::bad_request(error));
        }
        if tokio::time::Instant::now() >= deadline {
            job.cancel.cancel();
            state
                .hls_jobs
                .lock()
                .map_err(ApiError::internal)?
                .remove(&id);
            return Err(ApiError::bad_request("HLS startup timed out"));
        }
        tokio::time::sleep(Duration::from_millis(75)).await;
    }
}

async fn playlist_segment_count(path: &Path) -> usize {
    tokio::fs::read_to_string(path)
        .await
        .ok()
        .map(|body| {
            body.lines()
                .filter(|line| line.starts_with("#EXTINF:"))
                .count()
        })
        .unwrap_or(0)
}

pub async fn hls_status(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<HlsStatusResponse>, ApiError> {
    let job = state
        .hls_jobs
        .lock()
        .map_err(ApiError::internal)?
        .get(&id)
        .cloned()
        .ok_or_else(|| ApiError::bad_request("HLS job not found"))?;
    let status = job.state.lock().map_err(ApiError::internal)?;
    let response = HlsStatusResponse {
        done: status.done,
        error: status.error.clone(),
    };
    drop(status);
    if response.done {
        state
            .hls_jobs
            .lock()
            .map_err(ApiError::internal)?
            .remove(&id);
    }
    Ok(Json(response))
}

pub async fn cancel_hls(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
) -> StatusCode {
    if let Ok(mut jobs) = state.hls_jobs.lock()
        && let Some(job) = jobs.remove(&id)
    {
        job.cancel.cancel();
    }
    StatusCode::NO_CONTENT
}

pub async fn subtitle(
    State(state): State<AppState>,
    Json(q): Json<SubtitleRequest>,
) -> Result<Response, ApiError> {
    let _permit = state
        .media_light_slots
        .clone()
        .acquire_owned()
        .await
        .map_err(ApiError::internal)?;
    let cancel = state.shutdown.child_token();
    let reader = state.s3.range_reader(q.key, cancel.clone()).await?;
    let mut guard = CancelOnDrop(cancel.clone());
    let converted = task::spawn_blocking(move || {
        subtitle_blocking(reader, q.format.as_deref(), q.stream_index, cancel)
    })
    .await
    .map_err(ApiError::internal)?
    .map_err(ApiError::bad_request)?;
    guard.disarm();
    let mut response = Body::from(converted).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/vtt; charset=utf-8"),
    );
    Ok(response)
}

struct CancelOnDrop(CancellationToken);
impl CancelOnDrop {
    fn disarm(&mut self) {
        self.0 = CancellationToken::new();
    }
}
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}
