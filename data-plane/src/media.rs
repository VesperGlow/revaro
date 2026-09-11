use std::collections::HashMap;
use std::io::Read;

use axum::{
    Json,
    body::Body,
    extract::State,
    http::{HeaderValue, header},
    response::{IntoResponse, Response},
};
use ffmpeg::{
    Rational, codec,
    format::context::StreamIo,
    media::Type,
    software::scaling::{context::Context as Scaler, flag::Flags},
    util::frame::video::Video,
};
use ffmpeg_next as ffmpeg;
use image::{ExtendedColorType, codecs::jpeg::JpegEncoder};
use serde::{Deserialize, Serialize};
use tokio::task;
use tokio_util::sync::CancellationToken;

use crate::{AppState, error::ApiError};

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
pub struct SubtitleRequest {
    key: String,
    format: Option<String>,
    stream_index: Option<usize>,
}

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
include!("media/input.rs");
include!("media/subtitles.rs");
include!("media/probe_thumbnail.rs");
