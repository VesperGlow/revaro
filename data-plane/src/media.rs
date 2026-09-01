use std::{
    collections::HashMap,
    env,
    io::{self, Read, Write},
    path::Path,
    process::Command,
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

include!("media/api.rs");
include!("media/transcode.rs");
include!("media/subtitles.rs");
include!("media/probe_thumbnail.rs");
include!("media/fmp4.rs");
