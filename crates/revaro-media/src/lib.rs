//! Native media operations used directly by the Revaro server.
//!
//! The old deployment exposed these operations through a loopback data-plane
//! process.  Keeping the engine as a small blocking library gives the server a
//! clearer boundary: it owns authentication, object lookup, persistence and
//! HTTP semantics, while this crate owns only FFmpeg/image decoding and text
//! conversion.  The server runs the blocking methods on a worker thread and
//! supplies a cancellation token for request disconnects and shutdown.

mod image;
mod probe;
mod subtitle;
mod thumbnail;

use std::io::{Read, Seek};
use std::sync::OnceLock;

use ffmpeg_next as ffmpeg;
use tokio_util::sync::CancellationToken;

pub use image::resize_image_to_jpeg;
pub use revaro_core::media::{EmbeddedSubtitle, MediaChapter, MediaProbe};

/// Maximum source dimensions accepted by the still-image thumbnail path.
pub const MAX_IMAGE_PIXELS: u64 = 40_000_000;
/// Maximum width or height accepted by the still-image thumbnail path.
pub const MAX_IMAGE_SIDE: u32 = 30_000;
/// Maximum encoded thumbnail returned by the media engine.
pub const MAX_THUMBNAIL_BYTES: usize = 8 << 20;
/// Maximum source read by the external subtitle converter.
pub const MAX_SUBTITLE_BYTES: usize = 16 << 20;
/// Maximum converted subtitle returned by the engine.
pub const MAX_CONVERTED_SUBTITLE_BYTES: usize = 32 << 20;

/// Errors that can be surfaced by a media operation.
#[derive(Debug, thiserror::Error)]
pub enum MediaError {
    /// FFmpeg could not initialise or open the input.
    #[error("media input error: {0}")]
    Input(String),
    /// The operation was cancelled by its caller.
    #[error("media operation cancelled")]
    Cancelled,
    /// No video stream was available for a video thumbnail.
    #[error("media has no video stream")]
    NoVideo,
    /// No attached picture was available for an audio cover.
    #[error("media has no attached picture")]
    NoArtwork,
    /// A video did not yield a usable decoded frame.
    #[error("video produced no usable thumbnail frame")]
    NoUsableFrame,
    /// The source subtitle exceeded the bounded input limit.
    #[error("subtitle is too large")]
    SubtitleTooLarge,
    /// Conversion would exceed the bounded output limit.
    #[error("converted subtitle is too large")]
    ConvertedSubtitleTooLarge,
    /// The source subtitle was not UTF-8.
    #[error("subtitle is not valid UTF-8")]
    SubtitleNotUtf8,
    /// A WebVTT source did not contain the required header.
    #[error("invalid WebVTT header")]
    InvalidWebVtt,
    /// The requested external subtitle format is not supported.
    #[error("unsupported subtitle format")]
    UnsupportedSubtitleFormat,
    /// The requested stream is not a usable subtitle stream.
    #[error("{0}")]
    InvalidSubtitleStream(String),
    /// A decoded image or media frame was structurally invalid.
    #[error("invalid media data: {0}")]
    InvalidData(String),
}

impl MediaError {
    /// Whether this error was caused by an already-cancelled operation.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        matches!(self, Self::Cancelled)
    }
}

/// The in-process media engine.
#[derive(Debug, Clone, Copy, Default)]
pub struct MediaEngine;

impl MediaEngine {
    /// Probe container, stream, chapter and embedded-subtitle metadata.
    pub fn probe<R: Read + Seek + Send + 'static>(
        &self,
        reader: R,
        cancel: CancellationToken,
    ) -> Result<MediaProbe, MediaError> {
        probe::probe(reader, cancel)
    }

    /// Decode a video frame or an attached audio picture into a JPEG.
    pub fn thumbnail<R: Read + Seek + Send + 'static>(
        &self,
        reader: R,
        max_dimension: u32,
        attached_picture_only: bool,
        cancel: CancellationToken,
    ) -> Result<Vec<u8>, MediaError> {
        thumbnail::thumbnail(reader, max_dimension, attached_picture_only, cancel)
    }

    /// Convert an external subtitle or an embedded subtitle stream to WebVTT.
    pub fn subtitle<R: Read + Seek + Send + 'static>(
        &self,
        reader: R,
        format: Option<&str>,
        stream_index: Option<usize>,
        cancel: CancellationToken,
    ) -> Result<Vec<u8>, MediaError> {
        subtitle::subtitle(reader, format, stream_index, cancel)
    }
}

static FFMPEG_INIT: OnceLock<Result<(), String>> = OnceLock::new();

pub(crate) fn init_ffmpeg() -> Result<(), MediaError> {
    match FFMPEG_INIT.get_or_init(|| ffmpeg::init().map_err(|error| error.to_string())) {
        Ok(()) => Ok(()),
        Err(error) => Err(MediaError::Input(format!(
            "media engine initialization failed: {error}"
        ))),
    }
}

pub(crate) fn open_input<R: Read + Seek + Send + 'static>(
    reader: R,
    cancel: CancellationToken,
) -> Result<ffmpeg::format::context::Input, MediaError> {
    if cancel.is_cancelled() {
        return Err(MediaError::Cancelled);
    }
    let io = ffmpeg::format::context::StreamIo::from_read_seek_with_capacity(reader, 256 << 10)
        .map_err(|error| MediaError::Input(error.to_string()))?;
    let interrupt = cancel.clone();
    ffmpeg::format::input_from_stream_with_interrupt(io, None, None, move || {
        interrupt.is_cancelled()
    })
    .map_err(|error| {
        if cancel.is_cancelled() {
            MediaError::Cancelled
        } else {
            MediaError::Input(format!("open media: {error}"))
        }
    })
}

pub(crate) fn check_cancel(cancel: &CancellationToken) -> Result<(), MediaError> {
    if cancel.is_cancelled() {
        Err(MediaError::Cancelled)
    } else {
        Ok(())
    }
}
