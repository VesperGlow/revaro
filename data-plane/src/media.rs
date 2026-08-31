use std::{
    env,
    io::{self, Read, Write},
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use axum::{
    Json,
    body::Body,
    extract::{Path as AxumPath, State},
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use ffmpeg::{
    ChannelLayout, Dictionary, Packet, Rational, codec, encoder, format,
    format::context::StreamIo,
    frame,
    media::Type,
    software::{
        resampling::context::Context as Resampler,
        scaling::{context::Context as Scaler, flag::Flags},
    },
    util::frame::video::Video,
};
use ffmpeg_next as ffmpeg;
use image::{ExtendedColorType, codecs::jpeg::JpegEncoder};
use serde::{Deserialize, Serialize};
use tokio::{sync::mpsc, task};
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::CancellationToken;

use crate::{AppState, audio_fifo::AudioFrameAccumulator, error::ApiError};

#[derive(Deserialize)]
pub struct ProbeRequest {
    key: String,
}

#[derive(Deserialize)]
pub struct ThumbnailRequest {
    key: String,
    max_dimension: Option<u32>,
    attached_picture_only: Option<bool>,
}

#[derive(Deserialize)]
pub struct Fmp4Request {
    key: String,
    start_seconds: Option<f64>,
    include_audio: Option<bool>,
    transcode_video: Option<bool>,
    transcode_audio: Option<bool>,
}

#[derive(Deserialize)]
pub struct HlsRequest {
    key: String,
    output_dir: String,
    start_seconds: Option<f64>,
    audio_only: Option<bool>,
}

#[derive(Deserialize)]
pub struct SubtitleRequest {
    key: String,
    format: Option<String>,
    stream_index: Option<usize>,
}

#[derive(Serialize)]
pub struct HlsResponse {
    duration_ms: i64,
    video_codec: String,
    audio_codec: String,
    transcoding: bool,
    job_id: String,
}

#[derive(Default)]
struct HlsJobState {
    done: bool,
    error: String,
}

pub(crate) struct HlsJob {
    cancel: CancellationToken,
    state: Mutex<HlsJobState>,
}

#[derive(Serialize)]
pub struct HlsStatusResponse {
    done: bool,
    #[serde(skip_serializing_if = "String::is_empty")]
    error: String,
}

static NEXT_HLS_JOB: AtomicU64 = AtomicU64::new(1);

#[derive(Serialize, Default)]
pub struct ProbeResponse {
    duration_ms: i64,
    container: String,
    video_codec: String,
    audio_codec: String,
    width: u32,
    height: u32,
    bitrate: i64,
    frame_rate: String,
    video_profile: String,
    video_level: i32,
    chapters: Vec<Chapter>,
    subtitles: Vec<Subtitle>,
}

#[derive(Serialize)]
pub struct Chapter {
    title: String,
    start_ms: i64,
    end_ms: i64,
}

#[derive(Serialize)]
pub struct Subtitle {
    pub(crate) index: usize,
    codec: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub(crate) language: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub(crate) title: String,
    pub(crate) default: bool,
    pub(crate) forced: bool,
}

pub(crate) fn external_subtitle_meta(index: usize, name: &str) -> Subtitle {
    let stem = Path::new(name)
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or(name);
    let language = stem.rsplit(['.', '_', '-']).next().unwrap_or("");
    Subtitle {
        index,
        codec: Path::new(name)
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or("")
            .to_ascii_lowercase(),
        language: language.to_owned(),
        title: stem.to_owned(),
        default: false,
        forced: false,
    }
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

fn millis(value: i64, base: Rational) -> i64 {
    if value < 0 {
        return 0;
    }
    let numerator = i128::from(value) * i128::from(base.numerator()) * 1000;
    let denominator = i128::from(base.denominator()).max(1);
    (numerator / denominator).clamp(0, i128::from(i64::MAX)) as i64
}

fn open_input(
    reader: crate::s3::S3RangeReader,
    cancel: CancellationToken,
) -> Result<ffmpeg::format::context::Input, String> {
    let io =
        StreamIo::from_read_seek_with_capacity(reader, 256 << 10).map_err(|e| e.to_string())?;
    ffmpeg::format::input_from_stream_with_interrupt(io, None, None, move || cancel.is_cancelled())
        .map_err(|e| format!("open media: {e}"))
}

// ---- transcoding: decode + re-encode to browser-safe codecs ----

const H264_DEFAULT_BITRATE: i64 = 8_000_000;
const AAC_DEFAULT_BITRATE: usize = 192_000;

fn is_h264_codec(id: codec::Id) -> bool {
    id == codec::Id::H264
}

fn is_aac_codec(id: codec::Id) -> bool {
    id == codec::Id::AAC
}

struct VideoEncode {
    decoder: codec::decoder::Video,
    encoder: codec::encoder::Video,
    scaler: Scaler,
    scaled: Video,
    output_index: usize,
    output_time_base: Rational,
    start_pts: i64,
}

impl VideoEncode {
    fn drain(&mut self, output: &mut format::context::Output) -> Result<(), String> {
        let mut encoded = Packet::empty();
        while self.encoder.receive_packet(&mut encoded).is_ok() {
            encoded.set_stream(self.output_index);
            if encoded.duration() == 0 {
                encoded.set_duration(1);
            }
            encoded.rescale_ts(self.encoder.time_base(), self.output_time_base);
            encoded
                .write_interleaved(output)
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    fn send(
        &mut self,
        packet: &Packet,
        output: &mut format::context::Output,
    ) -> Result<(), String> {
        self.decoder
            .send_packet(packet)
            .map_err(|e| e.to_string())?;
        let mut decoded = Video::empty();
        while self.decoder.receive_frame(&mut decoded).is_ok() {
            let Some(pts) = decoded.pts() else { continue };
            if pts < self.start_pts {
                continue;
            }
            self.scaler
                .run(&decoded, &mut self.scaled)
                .map_err(|e| e.to_string())?;
            self.scaled.set_pts(Some(pts - self.start_pts));
            self.encoder
                .send_frame(&self.scaled)
                .map_err(|e| e.to_string())?;
            self.drain(output)?;
        }
        Ok(())
    }

    fn flush(&mut self, output: &mut format::context::Output) -> Result<(), String> {
        self.decoder.send_eof().map_err(|e| e.to_string())?;
        let mut decoded = Video::empty();
        while self.decoder.receive_frame(&mut decoded).is_ok() {
            let Some(pts) = decoded.pts() else { continue };
            if pts < self.start_pts {
                continue;
            }
            self.scaler
                .run(&decoded, &mut self.scaled)
                .map_err(|e| e.to_string())?;
            self.scaled.set_pts(Some(pts - self.start_pts));
            self.encoder
                .send_frame(&self.scaled)
                .map_err(|e| e.to_string())?;
            self.drain(output)?;
        }
        self.encoder.send_eof().map_err(|e| e.to_string())?;
        self.drain(output)
    }
}

struct AudioEncode {
    decoder: codec::decoder::Audio,
    encoder: codec::encoder::Audio,
    resampler: Resampler,
    output_index: usize,
    output_time_base: Rational,
    next_pts: i64,
    input_time_base: Rational,
    start_pts: i64,
    initialized: bool,
    accumulator: AudioFrameAccumulator,
}

impl AudioEncode {
    fn drain(&mut self, output: &mut format::context::Output) -> Result<(), String> {
        let mut encoded = Packet::empty();
        while self.encoder.receive_packet(&mut encoded).is_ok() {
            encoded.set_stream(self.output_index);
            encoded.rescale_ts(self.encoder.time_base(), self.output_time_base);
            encoded
                .write_interleaved(output)
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    fn encode_accumulated(
        &mut self,
        output: &mut format::context::Output,
        finishing: bool,
    ) -> Result<(), String> {
        while let Some(mut frame) = self.accumulator.next_frame(finishing)? {
            frame.set_pts(Some(self.next_pts));
            self.next_pts += frame.samples() as i64;
            self.encoder.send_frame(&frame).map_err(|e| e.to_string())?;
            self.drain(output)?;
        }
        Ok(())
    }

    fn resample_and_queue(
        &mut self,
        decoded: &frame::Audio,
        output: &mut format::context::Output,
    ) -> Result<(), String> {
        let mut resampled = frame::Audio::empty();
        self.resampler
            .run(decoded, &mut resampled)
            .map_err(|e| e.to_string())?;
        self.accumulator.push(&resampled)?;
        self.encode_accumulated(output, false)
    }

    fn send(
        &mut self,
        packet: &Packet,
        output: &mut format::context::Output,
    ) -> Result<(), String> {
        self.decoder
            .send_packet(packet)
            .map_err(|e| e.to_string())?;
        let mut decoded = frame::Audio::empty();
        while self.decoder.receive_frame(&mut decoded).is_ok() {
            if !self.initialized {
                let source_pts = decoded.pts().unwrap_or(self.start_pts);
                if source_pts < self.start_pts {
                    continue;
                }
                self.next_pts = rescale_value(
                    source_pts - self.start_pts,
                    self.input_time_base,
                    self.encoder.time_base(),
                );
                self.initialized = true;
            }
            self.resample_and_queue(&decoded, output)?;
        }
        Ok(())
    }

    fn flush(&mut self, output: &mut format::context::Output) -> Result<(), String> {
        self.decoder.send_eof().map_err(|e| e.to_string())?;
        let mut decoded = frame::Audio::empty();
        while self.decoder.receive_frame(&mut decoded).is_ok() {
            if !self.initialized {
                let source_pts = decoded.pts().unwrap_or(self.start_pts);
                if source_pts < self.start_pts {
                    continue;
                }
                self.next_pts = rescale_value(
                    source_pts - self.start_pts,
                    self.input_time_base,
                    self.encoder.time_base(),
                );
                self.initialized = true;
            }
            self.resample_and_queue(&decoded, output)?;
        }
        let mut resampler_drained = false;
        for _ in 0..32 {
            let samples = self
                .resampler
                .delay()
                .map(|delay| delay.output.max(0) as usize)
                .unwrap_or(0)
                .saturating_add(self.accumulator.frame_size().max(32));
            let mut delayed = frame::Audio::new(
                self.encoder.format(),
                samples,
                self.encoder.channel_layout(),
            );
            let remaining = self
                .resampler
                .flush(&mut delayed)
                .map_err(|e| e.to_string())?;
            let produced = delayed.samples();
            if produced > 0 {
                self.accumulator.push(&delayed)?;
                self.encode_accumulated(output, false)?;
            }
            if remaining.is_none() || produced == 0 {
                resampler_drained = true;
                break;
            }
        }
        if !resampler_drained {
            return Err("audio resampler did not drain within bounded iterations".into());
        }
        self.encode_accumulated(output, true)?;
        self.encoder.send_eof().map_err(|e| e.to_string())?;
        self.drain(output)
    }
}

enum Transform {
    Copy {
        source: usize,
        dest: usize,
        input_time_base: Rational,
        output_time_base: Rational,
        start_pts: i64,
        next_dts: Option<i64>,
        timestamp_offset: i64,
        last_mux_dts: Option<i64>,
    },
    Video(VideoEncode),
    Audio(AudioEncode),
}

fn process_transform(
    transform: &mut Transform,
    packet: &mut Packet,
    output: &mut format::context::Output,
) -> Result<(), String> {
    match transform {
        Transform::Copy {
            source,
            dest,
            input_time_base,
            output_time_base,
            start_pts,
            next_dts,
            timestamp_offset,
            last_mux_dts,
        } => {
            let original_pts = packet.pts();
            let original_dts = packet.dts();
            if original_pts
                .or(original_dts)
                .is_some_and(|value| value < *start_pts)
            {
                return Ok(());
            }
            // Matroska commonly omits DTS for presentation-ordered streams.
            // MP4 requires both timestamps. Preserve every supplied timestamp;
            // when one is absent, derive it from the other (or the end of the
            // previous packet) before rescaling the complete packet, including
            // its duration, into the muxer's time base.
            let pts = original_pts
                .or(original_dts)
                .map(|value| value - *start_pts);
            let dts = original_dts
                .map(|value| value - *start_pts)
                .or(*next_dts)
                .or(pts);
            packet.set_pts(pts.or(dts));
            packet.set_dts(dts);
            if let Some(value) = dts {
                *next_dts = Some(value.saturating_add(packet.duration().max(1)));
            }
            packet.rescale_ts(*input_time_base, *output_time_base);
            normalize_copy_timestamps(packet, timestamp_offset, last_mux_dts);
            packet.set_position(-1);
            packet.set_stream(*dest);
            packet
                .write_interleaved(output)
                .map_err(|e| format!("mux copied input stream {source} as MP4 stream {dest}: {e}"))
        }
        Transform::Video(video) => video.send(packet, output),
        Transform::Audio(audio) => audio.send(packet, output),
    }
}

fn flush_transform(
    transform: &mut Transform,
    output: &mut format::context::Output,
) -> Result<(), String> {
    match transform {
        Transform::Copy { .. } => Ok(()),
        Transform::Video(video) => video.flush(output),
        Transform::Audio(audio) => audio.flush(output),
    }
}

fn copy_stream(
    stream: &ffmpeg::format::stream::Stream,
    output: &mut format::context::Output,
    start_seconds: f64,
) -> Result<Transform, String> {
    let mut target = output
        .add_stream(encoder::find(codec::Id::None))
        .map_err(|e| e.to_string())?;
    target.set_parameters(stream.parameters());
    unsafe {
        (*target.parameters().as_mut_ptr()).codec_tag = 0;
    }
    Ok(Transform::Copy {
        source: stream.index(),
        dest: target.index(),
        input_time_base: stream.time_base(),
        output_time_base: Rational(0, 1),
        start_pts: seconds_to_pts(start_seconds, stream.time_base()),
        next_dts: None,
        timestamp_offset: 0,
        last_mux_dts: None,
    })
}

// Some Matroska remuxes contain a timestamp discontinuity at the beginning of
// a track (observed as video DTS 3690 followed by 0). The MP4 muxer rejects the
// second packet with EINVAL. Keep the supplied PTS/DTS relationship, but carry
// a stable offset across the discontinuity so decoded timestamps remain
// strictly increasing instead of adjusting just one packet and failing again.
fn normalize_copy_timestamps(
    packet: &mut Packet,
    timestamp_offset: &mut i64,
    last_mux_dts: &mut Option<i64>,
) {
    let Some(raw_dts) = packet.dts() else {
        return;
    };
    let mut dts = raw_dts.saturating_add(*timestamp_offset);
    if let Some(last) = *last_mux_dts
        && dts <= last
    {
        let step = packet.duration().max(1);
        let increase = last.saturating_add(step).saturating_sub(dts);
        *timestamp_offset = timestamp_offset.saturating_add(increase);
        dts = raw_dts.saturating_add(*timestamp_offset);
    }
    packet.set_dts(Some(dts));
    if let Some(pts) = packet.pts() {
        packet.set_pts(Some(pts.saturating_add(*timestamp_offset)));
    }
    *last_mux_dts = Some(dts);
}

fn setup_video_encode(
    stream: &ffmpeg::format::stream::Stream,
    output: &mut format::context::Output,
    source_bitrate: i64,
    global_header: bool,
    start_seconds: f64,
) -> Result<VideoEncode, String> {
    let input_time_base = stream.time_base();
    let decoder = codec::context::Context::from_parameters(stream.parameters())
        .map_err(|e| e.to_string())?
        .decoder()
        .video()
        .map_err(|e| e.to_string())?;
    let width = decoder.width();
    let height = decoder.height();
    if width == 0 || height == 0 {
        return Err("video stream has unknown dimensions".into());
    }
    let codec = encoder::find_by_name("libx264")
        .ok_or("libx264 H.264 encoder is unavailable")?
        .video()
        .map_err(|e| e.to_string())?;
    let mut context = codec::context::Context::new_with_codec(*codec)
        .encoder()
        .video()
        .map_err(|e| e.to_string())?;
    context.set_width(width);
    context.set_height(height);
    context.set_format(ffmpeg::format::Pixel::YUV420P);
    context.set_time_base(input_time_base);
    context.set_frame_rate(stream.avg_frame_rate().into());
    context.set_aspect_ratio(decoder.aspect_ratio());
    context.set_bit_rate(source_bitrate.clamp(1_000_000, H264_DEFAULT_BITRATE) as usize);
    context.set_gop(60);
    context.set_max_b_frames(0);
    unsafe {
        (*context.as_mut_ptr()).profile = ffmpeg::ffi::FF_PROFILE_H264_HIGH;
    }
    // FFmpeg 5 bindings still expose `safe` while FFmpeg 6+ removed it.
    // Default fills version-specific fields without referring to either ABI;
    // newer bindings make the update syntactically redundant, hence the
    // narrowly scoped lint allowance.
    #[allow(clippy::needless_update)]
    let threading = codec::threading::Config {
        kind: codec::threading::Type::Frame,
        count: 1,
        ..Default::default()
    };
    context.set_threading(threading);
    if global_header {
        context.set_flags(codec::Flags::GLOBAL_HEADER);
    }
    let mut options = Dictionary::new();
    options.set("preset", "veryfast");
    options.set("tune", "zerolatency");
    let opened = context
        .open_with(options)
        .map_err(|e| format!("open H.264 encoder: {e}"))?;
    let mut target = output.add_stream(codec).map_err(|e| e.to_string())?;
    target.set_parameters(&opened);
    target.set_time_base(input_time_base);
    let output_index = target.index();
    let scaler = Scaler::get(
        decoder.format(),
        width,
        height,
        ffmpeg::format::Pixel::YUV420P,
        width,
        height,
        Flags::BILINEAR,
    )
    .map_err(|e| e.to_string())?;
    Ok(VideoEncode {
        decoder,
        encoder: opened,
        scaler,
        scaled: Video::empty(),
        output_index,
        output_time_base: Rational(0, 1),
        start_pts: seconds_to_pts(start_seconds, input_time_base),
    })
}

fn setup_audio_encode(
    stream: &ffmpeg::format::stream::Stream,
    output: &mut format::context::Output,
    global_header: bool,
    start_seconds: f64,
) -> Result<AudioEncode, String> {
    let decoder = codec::context::Context::from_parameters(stream.parameters())
        .map_err(|e| e.to_string())?
        .decoder()
        .audio()
        .map_err(|e| e.to_string())?;
    let codec = encoder::find(codec::Id::AAC)
        .ok_or("AAC encoder is unavailable")?
        .audio()
        .map_err(|e| e.to_string())?;
    let mut context = codec::context::Context::new_with_codec(*codec)
        .encoder()
        .audio()
        .map_err(|e| e.to_string())?;
    let source_layout = if decoder.channel_layout().is_empty() {
        ChannelLayout::default(decoder.channels().into())
    } else {
        decoder.channel_layout()
    };
    let layout = if decoder.channels() > 6 {
        ChannelLayout::STEREO
    } else {
        source_layout
    };
    context.set_rate(48_000);
    context.set_channel_layout(layout);
    context.set_format(
        codec
            .formats()
            .ok_or("AAC encoder exposes no sample formats")?
            .next()
            .ok_or("AAC encoder exposes no sample formats")?,
    );
    context.set_bit_rate((layout.channels() as usize * 96_000).clamp(AAC_DEFAULT_BITRATE, 384_000));
    context.set_time_base((1, 48_000));
    if global_header {
        context.set_flags(codec::Flags::GLOBAL_HEADER);
    }
    let opened = context
        .open_as(codec)
        .map_err(|e| format!("open AAC encoder: {e}"))?;
    let mut target = output.add_stream(codec).map_err(|e| e.to_string())?;
    target.set_parameters(&opened);
    target.set_time_base((1, 48_000));
    let output_index = target.index();
    let resampler = Resampler::get(
        decoder.format(),
        if decoder.channel_layout().is_empty() {
            ChannelLayout::default(decoder.channels().into())
        } else {
            decoder.channel_layout()
        },
        decoder.rate(),
        opened.format(),
        opened.channel_layout(),
        opened.rate(),
    )
    .map_err(|e| e.to_string())?;
    let accumulator = AudioFrameAccumulator::new(&opened)?;
    Ok(AudioEncode {
        decoder,
        encoder: opened,
        resampler,
        output_index,
        output_time_base: Rational(0, 1),
        next_pts: 0,
        input_time_base: stream.time_base(),
        start_pts: seconds_to_pts(start_seconds, stream.time_base()),
        initialized: false,
        accumulator,
    })
}

fn seconds_to_pts(seconds: f64, base: Rational) -> i64 {
    if seconds <= 0.0 || base.numerator() == 0 {
        return 0;
    }
    (seconds / f64::from(base)).round() as i64
}

fn rescale_value(value: i64, from: Rational, to: Rational) -> i64 {
    if to.numerator() == 0 || from.denominator() == 0 {
        return 0;
    }
    let numerator = i128::from(value) * i128::from(from.numerator()) * i128::from(to.denominator());
    let denominator = i128::from(from.denominator()) * i128::from(to.numerator());
    (numerator / denominator).clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64
}

fn assign_output_time_bases(transforms: &mut [Transform], output: &format::context::Output) {
    let output_bases: Vec<_> = output.streams().map(|stream| stream.time_base()).collect();
    for transform in transforms {
        match transform {
            Transform::Copy {
                dest,
                output_time_base,
                ..
            } => *output_time_base = output_bases[*dest],
            Transform::Video(video) => video.output_time_base = output_bases[video.output_index],
            Transform::Audio(audio) => audio.output_time_base = output_bases[audio.output_index],
        }
    }
}

fn remux_hls(
    reader: crate::s3::S3RangeReader,
    output_dir: &Path,
    start_seconds: f64,
    audio_only: bool,
    cancel: CancellationToken,
) -> Result<HlsResponse, String> {
    ffmpeg::init().map_err(|e| e.to_string())?;
    let mut input = open_input(reader, cancel.clone())?;
    let playlist = output_dir.join("index.m3u8");
    let mut output = ffmpeg::format::output_as(&playlist, "hls")
        .map_err(|e| format!("create HLS output: {e}"))?;
    let global_header = output
        .format()
        .flags()
        .contains(format::Flags::GLOBAL_HEADER);
    let mut mapping = vec![None; input.nb_streams() as usize];
    let mut transforms: Vec<Transform> = Vec::new();
    let mut video_codec = String::new();
    let mut audio_codec = String::new();
    let mut transcoding = false;
    let video_bitrate = if input.bit_rate() > 0 {
        input.bit_rate()
    } else {
        H264_DEFAULT_BITRATE
    };
    for stream in input.streams() {
        let medium = stream.parameters().medium();
        if (audio_only || medium != Type::Video) && medium != Type::Audio {
            continue;
        }
        if medium == Type::Video && !video_codec.is_empty()
            || medium == Type::Audio && !audio_codec.is_empty()
        {
            continue;
        }
        let id = stream.parameters().id();
        let codec_name = id.name().to_lowercase();
        let transform = if medium == Type::Video {
            video_codec = codec_name;
            if is_h264_codec(id) {
                copy_stream(&stream, &mut output, start_seconds)?
            } else {
                transcoding = true;
                Transform::Video(setup_video_encode(
                    &stream,
                    &mut output,
                    video_bitrate,
                    global_header,
                    start_seconds,
                )?)
            }
        } else {
            audio_codec = codec_name;
            if is_aac_codec(id) {
                copy_stream(&stream, &mut output, start_seconds)?
            } else {
                transcoding = true;
                Transform::Audio(setup_audio_encode(
                    &stream,
                    &mut output,
                    global_header,
                    start_seconds,
                )?)
            }
        };
        mapping[stream.index()] = Some(transforms.len());
        transforms.push(transform);
    }
    if (audio_only && audio_codec.is_empty()) || (!audio_only && video_codec.is_empty()) {
        return Err("required media stream is unavailable".into());
    }
    if start_seconds > 0.0 {
        let position = (start_seconds * f64::from(ffmpeg::ffi::AV_TIME_BASE)) as i64;
        input
            .seek(position, ..position)
            .map_err(|e| format!("seek HLS input: {e}"))?;
    }
    let mut options = Dictionary::new();
    options.set("hls_time", if audio_only { "6" } else { "4" });
    options.set("hls_list_size", "0");
    options.set("hls_playlist_type", "event");
    options.set("hls_flags", "temp_file+independent_segments");
    let segment = output_dir
        .join("segment-%06d.ts")
        .to_string_lossy()
        .into_owned();
    options.set("hls_segment_filename", &segment);
    output
        .write_header_with(options)
        .map_err(|e| format!("write HLS header: {e}"))?;
    assign_output_time_bases(&mut transforms, &output);
    let stop_at = start_seconds + 180.0;
    for (stream, mut packet) in input.packets() {
        if cancel.is_cancelled() {
            return Err("HLS cancelled".into());
        }
        let Some(index) = mapping[stream.index()] else {
            continue;
        };
        if packet
            .pts()
            .is_some_and(|pts| f64::from(stream.time_base()) * pts as f64 > stop_at)
        {
            break;
        }
        process_transform(&mut transforms[index], &mut packet, &mut output)?;
    }
    for transform in &mut transforms {
        flush_transform(transform, &mut output)?;
    }
    output
        .write_trailer()
        .map_err(|e| format!("finish HLS: {e}"))?;
    Ok(HlsResponse {
        duration_ms: (input.duration() / 1000).max(0),
        video_codec,
        audio_codec,
        transcoding,
        job_id: String::new(),
    })
}

fn subtitle_blocking(
    reader: crate::s3::S3RangeReader,
    format: Option<&str>,
    stream_index: Option<usize>,
    cancel: CancellationToken,
) -> Result<Vec<u8>, String> {
    const MAX: usize = 16 << 20;
    if let Some(index) = stream_index {
        return embedded_subtitle(reader, index, cancel);
    }
    let mut raw = Vec::new();
    reader
        .take((MAX + 1) as u64)
        .read_to_end(&mut raw)
        .map_err(|e| e.to_string())?;
    if raw.len() > MAX {
        return Err("subtitle is too large".into());
    }
    let text = String::from_utf8(raw).map_err(|_| "subtitle is not valid UTF-8")?;
    let format = format
        .unwrap_or("vtt")
        .trim_start_matches('.')
        .to_ascii_lowercase();
    let output = match format.as_str() {
        "vtt" => {
            if !text.trim_start().starts_with("WEBVTT") {
                return Err("invalid WebVTT header".into());
            }
            text
        }
        "srt" => srt_to_vtt(&text),
        "ass" | "ssa" => ass_to_vtt(&text),
        _ => return Err("unsupported subtitle format".into()),
    };
    if output.len() > MAX {
        return Err("converted subtitle is too large".into());
    }
    Ok(output.into_bytes())
}

fn embedded_subtitle(
    reader: crate::s3::S3RangeReader,
    index: usize,
    cancel: CancellationToken,
) -> Result<Vec<u8>, String> {
    ffmpeg::init().map_err(|e| e.to_string())?;
    let mut input = open_input(reader, cancel.clone())?;
    let stream = input
        .stream(index)
        .ok_or("subtitle stream index is out of range")?;
    if stream.parameters().medium() != Type::Subtitle {
        return Err("selected stream is not a subtitle".into());
    }
    let base = stream.time_base();
    let mut decoder = codec::context::Context::from_parameters(stream.parameters())
        .map_err(|e| e.to_string())?
        .decoder()
        .subtitle()
        .map_err(|e| e.to_string())?;
    let mut output = String::from("WEBVTT\n\n");
    for (packet_stream, packet) in input.packets() {
        if cancel.is_cancelled() {
            return Err("subtitle conversion cancelled".into());
        }
        if packet_stream.index() != index {
            continue;
        }
        let packet_start = packet
            .pts()
            .map(|pts| pts as f64 * f64::from(base))
            .unwrap_or_default();
        let packet_duration = packet.duration() as f64 * f64::from(base);
        let mut subtitle = ffmpeg::Subtitle::new();
        if !decoder
            .decode(&packet, &mut subtitle)
            .map_err(|e| e.to_string())?
        {
            continue;
        }
        let start = subtitle
            .pts()
            .map(|pts| pts as f64 / 1_000_000.0)
            .unwrap_or(packet_start)
            + subtitle.start() as f64 / 1000.0;
        let display_duration = subtitle.end().saturating_sub(subtitle.start()) as f64 / 1000.0;
        // libavcodec leaves start/end_display_time at zero for Matroska ASS.
        // In that case the Matroska block duration carried by the packet is
        // authoritative. Treating the missing display duration as 1 ms made
        // otherwise valid embedded ASS cues effectively impossible to see.
        let end = start + subtitle_cue_duration(display_duration, packet_duration);
        let mut lines = Vec::new();
        for rect in subtitle.rects() {
            match rect {
                ffmpeg::subtitle::Rect::Text(value) => lines.push(value.get().to_string()),
                ffmpeg::subtitle::Rect::Ass(value) => lines.push(strip_decoded_ass(value.get())),
                _ => {}
            }
        }
        if !lines.is_empty() {
            output.push_str(&vtt_time(start));
            output.push_str(" --> ");
            output.push_str(&vtt_time(end));
            output.push('\n');
            output.push_str(&lines.join("\n"));
            output.push_str("\n\n");
        }
        if output.len() > 16 << 20 {
            return Err("converted subtitle is too large".into());
        }
    }
    Ok(output.into_bytes())
}

fn srt_to_vtt(text: &str) -> String {
    let normalized = text.trim_start_matches('\u{feff}').replace("\r\n", "\n");
    let mut out = String::from("WEBVTT\n\n");
    for line in normalized.lines() {
        if line.contains(" --> ") {
            out.push_str(&line.replace(',', "."));
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    out
}

fn ass_to_vtt(text: &str) -> String {
    let mut out = String::from("WEBVTT\n\n");
    for line in text.lines() {
        let Some(raw) = line.strip_prefix("Dialogue:") else {
            continue;
        };
        let fields: Vec<_> = raw.trim().splitn(10, ',').collect();
        if fields.len() != 10 {
            continue;
        }
        let (Some(start), Some(end)) = (ass_time(fields[1]), ass_time(fields[2])) else {
            continue;
        };
        out.push_str(&vtt_time(start));
        out.push_str(" --> ");
        out.push_str(&vtt_time(end));
        out.push('\n');
        out.push_str(&strip_ass(fields[9]));
        out.push_str("\n\n");
    }
    out
}

fn ass_time(value: &str) -> Option<f64> {
    let mut fields = value.trim().split(':');
    let hours = fields.next()?.parse::<f64>().ok()?;
    let minutes = fields.next()?.parse::<f64>().ok()?;
    let seconds = fields.next()?.parse::<f64>().ok()?;
    Some(hours * 3600.0 + minutes * 60.0 + seconds)
}

fn strip_ass(value: &str) -> String {
    let body = if value.starts_with("Dialogue:") {
        value.splitn(10, ',').nth(9).unwrap_or(value)
    } else {
        value
    };
    let mut result = String::new();
    let mut tag = false;
    for ch in body.chars() {
        match ch {
            '{' => tag = true,
            '}' => tag = false,
            _ if !tag => result.push(ch),
            _ => {}
        }
    }
    result.replace("\\N", "\n").replace("\\n", "\n")
}

fn strip_decoded_ass(value: &str) -> String {
    // AVSubtitleRect::ass omits "Dialogue:" and the Start/End columns. Its
    // nine fields are ReadOrder, Layer, Style, Name, three margins, Effect,
    // and Text. Commas in Text must remain intact.
    let fields: Vec<_> = value.splitn(9, ',').collect();
    strip_ass(if fields.len() == 9 { fields[8] } else { value })
}

fn subtitle_cue_duration(display_duration: f64, packet_duration: f64) -> f64 {
    if display_duration.is_finite() && display_duration > 0.0 {
        display_duration
    } else if packet_duration.is_finite() && packet_duration > 0.0 {
        packet_duration
    } else {
        0.001
    }
}

fn vtt_time(value: f64) -> String {
    let milliseconds = (value.max(0.0) * 1000.0).round() as u64;
    format!(
        "{:02}:{:02}:{:02}.{:03}",
        milliseconds / 3_600_000,
        milliseconds / 60_000 % 60,
        milliseconds / 1000 % 60,
        milliseconds % 1000
    )
}

fn probe_blocking(
    reader: crate::s3::S3RangeReader,
    cancel: CancellationToken,
) -> Result<ProbeResponse, String> {
    ffmpeg::init().map_err(|e| e.to_string())?;
    let input = open_input(reader, cancel)?;
    let mut result = ProbeResponse {
        duration_ms: (input.duration() / 1000).max(0),
        container: input.format().name().to_lowercase(),
        bitrate: input.bit_rate().max(0),
        ..Default::default()
    };
    for stream in input.streams() {
        let parameters = stream.parameters();
        let codec = parameters.id().name().to_lowercase();
        match parameters.medium() {
            Type::Video if result.video_codec.is_empty() => {
                result.video_codec = codec;
                result.frame_rate = stream.avg_frame_rate().to_string();
                unsafe {
                    let raw = parameters.as_ptr();
                    result.video_level = (*raw).level;
                    result.video_profile = match (result.video_codec.as_str(), (*raw).profile) {
                        ("h264", 66) => "Baseline",
                        ("h264", 77) => "Main",
                        ("h264", 100) => "High",
                        ("hevc", 2) => "Main 10",
                        ("hevc", 1) => "Main",
                        _ => "",
                    }
                    .to_string();
                }
                if let Ok(context) = ffmpeg::codec::context::Context::from_parameters(parameters)
                    && let Ok(decoder) = context.decoder().video()
                {
                    result.width = decoder.width();
                    result.height = decoder.height();
                }
            }
            Type::Audio if result.audio_codec.is_empty() => result.audio_codec = codec,
            Type::Subtitle => {
                let metadata = stream.metadata();
                let disposition = stream.disposition();
                result.subtitles.push(Subtitle {
                    index: stream.index(),
                    codec,
                    language: metadata.get("language").unwrap_or_default().to_string(),
                    title: metadata.get("title").unwrap_or_default().to_string(),
                    default: disposition.contains(ffmpeg::format::stream::Disposition::DEFAULT),
                    forced: disposition.contains(ffmpeg::format::stream::Disposition::FORCED),
                });
            }
            _ => {}
        }
    }
    for (index, chapter) in input.chapters().enumerate() {
        let title = chapter
            .metadata()
            .get("title")
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| format!("Chapter {}", index + 1));
        result.chapters.push(Chapter {
            title,
            start_ms: millis(chapter.start(), chapter.time_base()),
            end_ms: millis(chapter.end(), chapter.time_base()),
        });
    }
    Ok(result)
}

fn thumbnail_blocking(
    reader: crate::s3::S3RangeReader,
    max_dimension: u32,
    attached_picture_only: bool,
    cancel: CancellationToken,
) -> Result<Vec<u8>, String> {
    ffmpeg::init().map_err(|e| e.to_string())?;
    let mut input = open_input(reader, cancel.clone())?;
    let stream = if attached_picture_only {
        input.streams().find(|stream| {
            stream.parameters().medium() == Type::Video
                && stream
                    .disposition()
                    .contains(ffmpeg::format::stream::Disposition::ATTACHED_PIC)
        })
    } else {
        input.streams().best(Type::Video)
    }
    .ok_or(if attached_picture_only {
        "media has no attached picture"
    } else {
        "media has no video stream"
    })?;
    let stream_index = stream.index();
    let context = ffmpeg::codec::context::Context::from_parameters(stream.parameters())
        .map_err(|e| e.to_string())?;
    let mut decoder = context.decoder().video().map_err(|e| e.to_string())?;
    let (width, height) = fit(decoder.width(), decoder.height(), max_dimension);
    let mut scaler = Scaler::get(
        decoder.format(),
        decoder.width(),
        decoder.height(),
        ffmpeg::format::Pixel::RGB24,
        width,
        height,
        Flags::BILINEAR,
    )
    .map_err(|e| e.to_string())?;
    let mut decoded = Video::empty();
    let mut rgb = Video::empty();
    if attached_picture_only {
        let mut found = false;
        for (packet_stream, packet) in input.packets() {
            if cancel.is_cancelled() {
                return Err("thumbnail cancelled".into());
            }
            if packet_stream.index() != stream_index {
                continue;
            }
            decoder.send_packet(&packet).map_err(|e| e.to_string())?;
            if decoder.receive_frame(&mut decoded).is_ok() {
                scaler.run(&decoded, &mut rgb).map_err(|e| e.to_string())?;
                found = true;
                break;
            }
        }
        if !found {
            return Err("media has no attached picture".into());
        }
        return encode_thumbnail_rgb(&rgb, width, height);
    }
    // libav exposes the probed container duration in AV_TIME_BASE units. Seek
    // before decoding so only the GOP around the requested frame is read.
    // Later positions avoid black leaders and title cards; stop at the first
    // frame that is not overwhelmingly near-black.
    let duration = input.duration();
    let positions = if duration > 0 {
        [
            duration * 20 / 100,
            duration * 35 / 100,
            duration * 50 / 100,
        ]
    } else {
        [1_000_000, 2_000_000, 3_000_000]
    };
    let mut found = false;
    for seek_timestamp in positions {
        if cancel.is_cancelled() {
            return Err("thumbnail cancelled".into());
        }
        if input.seek(seek_timestamp, ..seek_timestamp).is_err() {
            continue;
        }
        decoder.flush();
        for (packet_stream, packet) in input.packets() {
            if cancel.is_cancelled() {
                return Err("thumbnail cancelled".into());
            }
            if packet_stream.index() != stream_index {
                continue;
            }
            if decoder.send_packet(&packet).is_err() {
                continue;
            }
            if decoder.receive_frame(&mut decoded).is_ok() {
                scaler.run(&decoded, &mut rgb).map_err(|e| e.to_string())?;
                if !frame_is_near_black(&rgb, width, height) {
                    found = true;
                }
                break;
            }
        }
        if found {
            break;
        }
    }
    if !found {
        return Err("video produced no usable thumbnail frame".into());
    }
    encode_thumbnail_rgb(&rgb, width, height)
}

fn encode_thumbnail_rgb(rgb: &Video, width: u32, height: u32) -> Result<Vec<u8>, String> {
    let row_bytes = width as usize * 3;
    let stride = rgb.stride(0);
    let plane = rgb.data(0);
    let packed;
    let pixels = if stride == row_bytes {
        &plane[..row_bytes * height as usize]
    } else {
        packed = (0..height as usize)
            .flat_map(|row| {
                plane[row * stride..row * stride + row_bytes]
                    .iter()
                    .copied()
            })
            .collect::<Vec<_>>();
        packed.as_slice()
    };
    let mut jpeg = Vec::with_capacity((width * height) as usize / 2);
    JpegEncoder::new_with_quality(&mut jpeg, 82)
        .encode(pixels, width, height, ExtendedColorType::Rgb8)
        .map_err(|e| e.to_string())?;
    if jpeg.len() > 8 << 20 {
        return Err("thumbnail exceeds output limit".into());
    }
    Ok(jpeg)
}

// Sample the RGB frame on a coarse grid. Treat it as near-black only when at
// least 98% of samples have very low luma; this deliberately simple check is
// cheap and avoids rejecting ordinary dark scenes with visible highlights.
fn frame_is_near_black(frame: &Video, width: u32, height: u32) -> bool {
    let stride = frame.stride(0);
    let plane = frame.data(0);
    rgb_is_near_black(plane, stride, width, height)
}

fn rgb_is_near_black(plane: &[u8], stride: usize, width: u32, height: u32) -> bool {
    let step_x = (width / 64).max(1) as usize;
    let step_y = (height / 36).max(1) as usize;
    let mut samples = 0usize;
    let mut black = 0usize;
    for y in (0..height as usize).step_by(step_y) {
        for x in (0..width as usize).step_by(step_x) {
            let offset = y * stride + x * 3;
            if offset + 2 >= plane.len() {
                continue;
            }
            samples += 1;
            let luma = (u16::from(plane[offset]) * 54
                + u16::from(plane[offset + 1]) * 183
                + u16::from(plane[offset + 2]) * 19)
                / 256;
            if luma < 16 {
                black += 1;
            }
        }
    }
    samples > 0 && black * 100 >= samples * 98
}

fn fit(width: u32, height: u32, max_dimension: u32) -> (u32, u32) {
    if width == 0 || height == 0 {
        return (max_dimension, max_dimension);
    }
    if width <= max_dimension && height <= max_dimension {
        return (width, height);
    }
    if width >= height {
        (
            max_dimension,
            ((u64::from(height) * u64::from(max_dimension) / u64::from(width)).max(1) as u32),
        )
    } else {
        (
            ((u64::from(width) * u64::from(max_dimension) / u64::from(height)).max(1) as u32),
            max_dimension,
        )
    }
}

struct ChannelWriter {
    tx: mpsc::Sender<Result<bytes::Bytes, io::Error>>,
    cancel: CancellationToken,
}

impl Write for ChannelWriter {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        if self.cancel.is_cancelled() {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "fMP4 cancelled"));
        }
        self.tx
            .blocking_send(Ok(bytes::Bytes::copy_from_slice(data)))
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "fMP4 consumer closed"))?;
        Ok(data.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn remux_fmp4(
    reader: crate::s3::S3RangeReader,
    writer: ChannelWriter,
    start: f64,
    include_audio: bool,
    transcode_video: bool,
    transcode_audio: bool,
    cancel: CancellationToken,
) -> Result<(), String> {
    ffmpeg::init().map_err(|e| e.to_string())?;
    let mut input = open_input(reader, cancel.clone())?;
    if start > 0.0 {
        input
            .seek((start * 1_000_000.0) as i64, ..)
            .map_err(|e| e.to_string())?;
    }
    let output_io =
        StreamIo::from_write_with_capacity(writer, 256 << 10).map_err(|e| e.to_string())?;
    let mut output = ffmpeg::format::output_to_stream(output_io, Some("stream.mp4"), Some("mp4"))
        .map_err(|e| e.to_string())?;
    let global_header = output
        .format()
        .flags()
        .contains(format::Flags::GLOBAL_HEADER);
    let mut mapping = vec![None; input.nb_streams() as usize];
    let mut transforms: Vec<Transform> = Vec::new();
    let video_bitrate = if input.bit_rate() > 0 {
        input.bit_rate()
    } else {
        H264_DEFAULT_BITRATE
    };
    let mut have_video = false;
    let mut have_audio = false;
    for stream in input.streams() {
        let medium = stream.parameters().medium();
        let transform = if medium == Type::Video && !have_video {
            have_video = true;
            if transcode_video {
                Transform::Video(setup_video_encode(
                    &stream,
                    &mut output,
                    video_bitrate,
                    global_header,
                    start,
                )?)
            } else {
                copy_stream(&stream, &mut output, start)?
            }
        } else if medium == Type::Audio && include_audio && !have_audio {
            have_audio = true;
            if transcode_audio {
                Transform::Audio(setup_audio_encode(
                    &stream,
                    &mut output,
                    global_header,
                    start,
                )?)
            } else {
                copy_stream(&stream, &mut output, start)?
            }
        } else {
            continue;
        };
        mapping[stream.index()] = Some(transforms.len());
        transforms.push(transform);
    }
    if transforms.is_empty() {
        return Err("no MP4-compatible media stream".into());
    }
    let mut options = Dictionary::new();
    options.set("movflags", "frag_keyframe+empty_moov+default_base_moof");
    options.set("frag_duration", "2000000");
    options.set("min_frag_duration", "500000");
    output
        .write_header_with(options)
        .map_err(|e| e.to_string())?;
    assign_output_time_bases(&mut transforms, &output);
    for (stream, mut packet) in input.packets() {
        if cancel.is_cancelled() {
            return Err("fMP4 cancelled".into());
        }
        let Some(index) = mapping[stream.index()] else {
            continue;
        };
        process_transform(&mut transforms[index], &mut packet, &mut output)?;
    }
    for transform in &mut transforms {
        flush_transform(transform, &mut output)?;
    }
    output.write_trailer().map_err(|e| e.to_string())?;
    Ok(())
}

pub(crate) struct PreparedWebMedia {
    pub duration_ms: i64,
    pub video_codec: String,
    pub audio_codec: String,
    pub subtitles: Vec<(Subtitle, Vec<u8>)>,
}

fn web_video_supported(id: codec::Id) -> bool {
    matches!(id, codec::Id::H264 | codec::Id::HEVC)
}

// Some(false) means packet copy; Some(true) means AAC encode.
fn web_audio_mode(id: codec::Id) -> Option<bool> {
    match id {
        codec::Id::AAC => Some(false),
        codec::Id::FLAC | codec::Id::AC3 | codec::Id::EAC3 => Some(true),
        _ => None,
    }
}

// Narrow BT ingest remux: one video, one audio, text subtitles out-of-band.
// The seekable MP4 output permits faststart instead of the live MSE fragments.
pub(crate) fn prepare_web_media(
    input_path: &Path,
    output_path: &Path,
    cancel: CancellationToken,
) -> Result<PreparedWebMedia, String> {
    ffmpeg::init().map_err(|e| e.to_string())?;
    let mut input = format::input(input_path).map_err(|e| format!("open media: {e}"))?;
    let duration_ms = (input.duration() / 1000).max(0);
    let mut output =
        format::output_as(output_path, "mp4").map_err(|e| format!("create MP4: {e}"))?;
    let global_header = output
        .format()
        .flags()
        .contains(format::Flags::GLOBAL_HEADER);
    let mut mapping = vec![None; input.nb_streams() as usize];
    let mut transforms = Vec::new();
    let mut video_codec = String::new();
    let mut audio_codec = String::new();
    let mut subtitle_meta = Vec::new();
    for stream in input.streams() {
        let id = stream.parameters().id();
        let codec_name = id.name().to_ascii_lowercase();
        match stream.parameters().medium() {
            Type::Video if video_codec.is_empty() => {
                if !web_video_supported(id) {
                    return Err(format!("unsupported video codec: {codec_name}"));
                }
                video_codec = codec_name;
                let transform = copy_stream(&stream, &mut output, 0.0)?;
                if id == codec::Id::HEVC {
                    let dest = match transform {
                        Transform::Copy { dest, .. } => dest,
                        _ => unreachable!(),
                    };
                    if let Some(target) = output.stream_mut(dest) {
                        unsafe {
                            (*target.parameters().as_mut_ptr()).codec_tag =
                                u32::from_le_bytes(*b"hvc1");
                        }
                    }
                }
                mapping[stream.index()] = Some(transforms.len());
                transforms.push(transform);
            }
            Type::Audio if audio_codec.is_empty() => {
                let Some(transcode) = web_audio_mode(id) else {
                    return Err(format!("unsupported audio codec: {codec_name}"));
                };
                audio_codec = codec_name;
                let transform = if !transcode {
                    copy_stream(&stream, &mut output, 0.0)?
                } else {
                    setup_audio_encode(&stream, &mut output, global_header, 0.0)
                        .map(Transform::Audio)?
                };
                mapping[stream.index()] = Some(transforms.len());
                transforms.push(transform);
            }
            Type::Subtitle if matches!(id, codec::Id::ASS | codec::Id::SSA | codec::Id::SUBRIP) => {
                let metadata = stream.metadata();
                subtitle_meta.push(Subtitle {
                    index: stream.index(),
                    codec: codec_name,
                    language: metadata.get("language").unwrap_or("").to_owned(),
                    title: metadata.get("title").unwrap_or("").to_owned(),
                    default: stream
                        .disposition()
                        .contains(ffmpeg::format::stream::Disposition::DEFAULT),
                    forced: stream
                        .disposition()
                        .contains(ffmpeg::format::stream::Disposition::FORCED),
                });
            }
            _ => {}
        }
    }
    if video_codec.is_empty() {
        return Err("unsupported video codec: none".into());
    }
    let mut options = Dictionary::new();
    options.set("movflags", "+faststart");
    output
        .write_header_with(options)
        .map_err(|e| format!("write MP4 header: {e}"))?;
    assign_output_time_bases(&mut transforms, &output);
    for (stream, mut packet) in input.packets() {
        if cancel.is_cancelled() {
            return Err("web media preparation cancelled".into());
        }
        if let Some(index) = mapping[stream.index()] {
            process_transform(&mut transforms[index], &mut packet, &mut output)?;
        }
    }
    for transform in &mut transforms {
        flush_transform(transform, &mut output)?;
    }
    output
        .write_trailer()
        .map_err(|e| format!("finish MP4: {e}"))?;
    let mut subtitles = Vec::new();
    for meta in subtitle_meta {
        let index = meta.index;
        let data = embedded_subtitle_path(input_path, index, cancel.clone())?;
        subtitles.push((meta, data));
    }
    Ok(PreparedWebMedia {
        duration_ms,
        video_codec,
        audio_codec,
        subtitles,
    })
}

fn embedded_subtitle_path(
    path: &Path,
    index: usize,
    cancel: CancellationToken,
) -> Result<Vec<u8>, String> {
    let mut input = format::input(path).map_err(|e| e.to_string())?;
    let stream = input
        .stream(index)
        .ok_or("subtitle stream index is out of range")?;
    let base = stream.time_base();
    let mut decoder = codec::context::Context::from_parameters(stream.parameters())
        .map_err(|e| e.to_string())?
        .decoder()
        .subtitle()
        .map_err(|e| e.to_string())?;
    let mut output = String::from("WEBVTT\n\n");
    for (packet_stream, packet) in input.packets() {
        if cancel.is_cancelled() {
            return Err("subtitle conversion cancelled".into());
        }
        if packet_stream.index() != index {
            continue;
        }
        let packet_start = packet
            .pts()
            .map(|pts| pts as f64 * f64::from(base))
            .unwrap_or_default();
        let packet_duration = packet.duration() as f64 * f64::from(base);
        let mut subtitle = ffmpeg::Subtitle::new();
        if !decoder
            .decode(&packet, &mut subtitle)
            .map_err(|e| e.to_string())?
        {
            continue;
        }
        let start = subtitle
            .pts()
            .map(|pts| pts as f64 / 1_000_000.0)
            .unwrap_or(packet_start)
            + subtitle.start() as f64 / 1000.0;
        let end = start
            + subtitle_cue_duration(
                subtitle.end().saturating_sub(subtitle.start()) as f64 / 1000.0,
                packet_duration,
            );
        let lines: Vec<_> = subtitle
            .rects()
            .filter_map(|rect| match rect {
                ffmpeg::subtitle::Rect::Text(v) => Some(v.get().to_owned()),
                ffmpeg::subtitle::Rect::Ass(v) => Some(strip_decoded_ass(v.get())),
                _ => None,
            })
            .collect();
        if !lines.is_empty() {
            output.push_str(&format!(
                "{} --> {}\n{}\n\n",
                vtt_time(start),
                vtt_time(end),
                lines.join("\n")
            ));
        }
        if output.len() > 16 << 20 {
            return Err("converted subtitle is too large".into());
        }
    }
    Ok(output.into_bytes())
}

pub(crate) fn external_subtitle_path(
    path: &Path,
    cancel: CancellationToken,
) -> Result<Vec<u8>, String> {
    ffmpeg::init().map_err(|e| e.to_string())?;
    let input = format::input(path).map_err(|e| format!("open external subtitle: {e}"))?;
    let index = input
        .streams()
        .best(Type::Subtitle)
        .ok_or("external subtitle contains no supported stream")?
        .index();
    drop(input);
    embedded_subtitle_path(path, index, cancel)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};

    #[test]
    fn near_black_detection_allows_visible_frames() {
        let black = vec![0_u8; 16 * 9 * 3];
        assert!(rgb_is_near_black(&black, 16 * 3, 16, 9));

        let mut visible = black;
        // More than 2% bright samples makes the deliberately coarse detector
        // accept the frame, while a tiny isolated compression speck does not.
        for index in 0..4 {
            visible[index * 3..index * 3 + 3].copy_from_slice(&[180, 180, 180]);
        }
        assert!(!rgb_is_near_black(&visible, 16 * 3, 16, 9));
    }
    #[test]
    fn rational_time_is_bounded() {
        assert_eq!(millis(90_000, Rational(1, 90_000)), 1000);
        assert_eq!(millis(-1, Rational(1, 1)), 0);
    }

    #[test]
    fn decoded_ass_uses_packet_duration_and_removes_packet_fields() {
        assert_eq!(subtitle_cue_duration(0.0, 2.84), 2.84);
        assert_eq!(subtitle_cue_duration(1.25, 2.84), 1.25);
        assert_eq!(
            strip_decoded_ass(r"2,0,Dial_CH,,0,0,0,,{\an8}你好,世界\N第二行"),
            "你好,世界\n第二行"
        );
    }

    #[test]
    fn web_ingest_codec_policy_is_narrow_and_copy_preserving() {
        assert!(web_video_supported(codec::Id::H264));
        assert!(web_video_supported(codec::Id::HEVC));
        assert!(!web_video_supported(codec::Id::VP9));
        assert_eq!(web_audio_mode(codec::Id::AAC), Some(false));
        assert_eq!(web_audio_mode(codec::Id::FLAC), Some(true));
        assert_eq!(web_audio_mode(codec::Id::AC3), Some(true));
        assert_eq!(web_audio_mode(codec::Id::EAC3), Some(true));
        assert_eq!(web_audio_mode(codec::Id::OPUS), None);
    }

    #[test]
    fn copied_packet_timestamp_reset_gets_a_stable_offset() {
        let mut offset = 0;
        let mut last = None;
        let mut first = Packet::empty();
        first.set_dts(Some(3690));
        first.set_pts(Some(3690));
        first.set_duration(40);
        normalize_copy_timestamps(&mut first, &mut offset, &mut last);
        assert_eq!(
            (first.dts(), first.pts(), offset),
            (Some(3690), Some(3690), 0)
        );

        let mut reset = Packet::empty();
        reset.set_dts(Some(0));
        reset.set_pts(Some(80));
        reset.set_duration(40);
        normalize_copy_timestamps(&mut reset, &mut offset, &mut last);
        assert_eq!(
            (reset.dts(), reset.pts(), offset),
            (Some(3730), Some(3810), 3730)
        );

        let mut following = Packet::empty();
        following.set_dts(Some(40));
        following.set_pts(Some(120));
        following.set_duration(40);
        normalize_copy_timestamps(&mut following, &mut offset, &mut last);
        assert_eq!(
            (following.dts(), following.pts(), offset),
            (Some(3770), Some(3850), 3730)
        );
    }

    #[test]
    fn web_ingest_handles_hevc_aac_eac3_and_two_ass_tracks() {
        if Command::new("ffmpeg")
            .arg("-version")
            .stdout(Stdio::null())
            .status()
            .is_err()
        {
            return;
        }
        let temp = tempfile::tempdir().unwrap();
        let ass = "[Script Info]\nScriptType: v4.00+\n[V4+ Styles]\nFormat: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\nStyle: Default,Arial,20,&H00FFFFFF,&H000000FF,&H00000000,&H00000000,0,0,0,0,100,100,0,0,1,1,0,2,10,10,10,1\n[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\nDialogue: 0,0:00:00.00,0:00:00.80,Default,,0,0,0,,fixture\n";
        let ass_one = temp.path().join("one.ass");
        let ass_two = temp.path().join("two.ass");
        std::fs::write(&ass_one, ass).unwrap();
        std::fs::write(&ass_two, ass).unwrap();
        let input = temp.path().join("kaguya-layout.mkv");
        let status = Command::new("ffmpeg")
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=128x72:rate=12",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:sample_rate=48000",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=660:sample_rate=48000",
            ])
            .arg("-i")
            .arg(&ass_one)
            .arg("-i")
            .arg(&ass_two)
            .args([
                "-t",
                "1",
                "-map",
                "0:v:0",
                "-map",
                "1:a:0",
                "-map",
                "2:a:0",
                "-map",
                "3:s:0",
                "-map",
                "4:s:0",
                "-c:v",
                "libx265",
                "-x265-params",
                "pools=1:frame-threads=1",
                "-pix_fmt",
                "yuv420p10le",
                "-c:a:0",
                "aac",
                "-c:a:1",
                "eac3",
                "-c:s",
                "ass",
                "-y",
            ])
            .arg(&input)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        if !status.success() {
            return;
        }
        let output = temp.path().join("playback.mp4");
        let prepared = prepare_web_media(&input, &output, CancellationToken::new()).unwrap();
        assert_eq!(prepared.video_codec, "hevc");
        assert_eq!(prepared.audio_codec, "aac");
        assert_eq!(prepared.subtitles.len(), 2);
        let probe = format::input(&output).unwrap();
        let codecs: Vec<_> = probe.streams().map(|s| s.parameters().id()).collect();
        assert_eq!(codecs, [codec::Id::HEVC, codec::Id::AAC]);
    }
}
