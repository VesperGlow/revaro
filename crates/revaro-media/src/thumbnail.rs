use std::io::{Read, Seek};

use ffmpeg::{
    media::Type,
    software::scaling::{context::Context as Scaler, flag::Flags},
    util::frame::video::Video,
};
use ffmpeg_next as ffmpeg;
use image::{ExtendedColorType, codecs::jpeg::JpegEncoder};
use tokio_util::sync::CancellationToken;

use crate::{MAX_THUMBNAIL_BYTES, MediaError, check_cancel, init_ffmpeg, open_input};

pub fn thumbnail<R: Read + Seek + Send + 'static>(
    reader: R,
    max_dimension: u32,
    attached_picture_only: bool,
    cancel: CancellationToken,
) -> Result<Vec<u8>, MediaError> {
    init_ffmpeg()?;
    let max_dimension = max_dimension.clamp(64, 2048);
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
    };
    let stream = stream.ok_or(if attached_picture_only {
        MediaError::NoArtwork
    } else {
        MediaError::NoVideo
    })?;
    let stream_index = stream.index();
    let context = ffmpeg::codec::context::Context::from_parameters(stream.parameters())
        .map_err(|error| MediaError::Input(error.to_string()))?;
    let mut decoder = context
        .decoder()
        .video()
        .map_err(|error| MediaError::Input(error.to_string()))?;
    let (width, height) = fit(decoder.width(), decoder.height(), max_dimension)?;
    let mut scaler = Scaler::get(
        decoder.format(),
        decoder.width(),
        decoder.height(),
        ffmpeg::format::Pixel::RGB24,
        width,
        height,
        Flags::BILINEAR,
    )
    .map_err(|error| MediaError::Input(error.to_string()))?;
    let mut decoded = Video::empty();
    let mut rgb = Video::empty();

    if attached_picture_only {
        for (packet_stream, packet) in input.packets() {
            check_cancel(&cancel)?;
            if packet_stream.index() != stream_index {
                continue;
            }
            decoder
                .send_packet(&packet)
                .map_err(|error| MediaError::Input(error.to_string()))?;
            if decoder.receive_frame(&mut decoded).is_ok() {
                scaler
                    .run(&decoded, &mut rgb)
                    .map_err(|error| MediaError::Input(error.to_string()))?;
                return encode_thumbnail_rgb(&rgb, width, height);
            }
        }
        return Err(MediaError::NoArtwork);
    }

    // FFmpeg exposes duration in AV_TIME_BASE units. Seeking to a few points
    // avoids decoding an entire long video just to find a representative frame.
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
    for seek_timestamp in positions {
        check_cancel(&cancel)?;
        if input.seek(seek_timestamp, ..seek_timestamp).is_err() {
            continue;
        }
        decoder.flush();
        for (packet_stream, packet) in input.packets() {
            check_cancel(&cancel)?;
            if packet_stream.index() != stream_index {
                continue;
            }
            if decoder.send_packet(&packet).is_err() {
                continue;
            }
            if decoder.receive_frame(&mut decoded).is_ok() {
                scaler
                    .run(&decoded, &mut rgb)
                    .map_err(|error| MediaError::Input(error.to_string()))?;
                if !frame_is_near_black(&rgb, width, height) {
                    return encode_thumbnail_rgb(&rgb, width, height);
                }
                break;
            }
        }
    }
    Err(MediaError::NoUsableFrame)
}

fn encode_thumbnail_rgb(rgb: &Video, width: u32, height: u32) -> Result<Vec<u8>, MediaError> {
    let row_bytes = width
        .checked_mul(3)
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(|| MediaError::InvalidData("thumbnail dimensions overflow".to_owned()))?;
    let height = usize::try_from(height)
        .map_err(|_| MediaError::InvalidData("thumbnail height overflow".to_owned()))?;
    let stride = rgb.stride(0);
    if stride < row_bytes {
        return Err(MediaError::InvalidData(
            "decoded frame stride is too small".to_owned(),
        ));
    }
    let required = stride
        .checked_mul(height)
        .ok_or_else(|| MediaError::InvalidData("decoded frame dimensions overflow".to_owned()))?;
    let plane = rgb.data(0);
    if plane.len() < required {
        return Err(MediaError::InvalidData(
            "decoded frame is shorter than its stride".to_owned(),
        ));
    }
    let pixels = if stride == row_bytes {
        &plane[..row_bytes * height]
    } else {
        let mut packed = Vec::with_capacity(row_bytes * height);
        for row in 0..height {
            packed.extend_from_slice(&plane[row * stride..row * stride + row_bytes]);
        }
        return encode_pixels(&packed, row_bytes / 3, height);
    };
    encode_pixels(pixels, row_bytes / 3, height)
}

fn encode_pixels(pixels: &[u8], width: usize, height: usize) -> Result<Vec<u8>, MediaError> {
    let width = u32::try_from(width)
        .map_err(|_| MediaError::InvalidData("thumbnail width overflow".to_owned()))?;
    let height = u32::try_from(height)
        .map_err(|_| MediaError::InvalidData("thumbnail height overflow".to_owned()))?;
    let mut jpeg = Vec::with_capacity((width as usize * height as usize / 2).max(1024));
    JpegEncoder::new_with_quality(&mut jpeg, 82)
        .encode(pixels, width, height, ExtendedColorType::Rgb8)
        .map_err(|error| MediaError::InvalidData(error.to_string()))?;
    if jpeg.len() > MAX_THUMBNAIL_BYTES {
        return Err(MediaError::InvalidData(
            "thumbnail exceeds output limit".to_owned(),
        ));
    }
    Ok(jpeg)
}

fn frame_is_near_black(frame: &Video, width: u32, height: u32) -> bool {
    rgb_is_near_black(frame.data(0), frame.stride(0), width, height)
}

fn rgb_is_near_black(plane: &[u8], stride: usize, width: u32, height: u32) -> bool {
    let step_x = (width / 64).max(1) as usize;
    let step_y = (height / 36).max(1) as usize;
    let mut samples = 0usize;
    let mut black = 0usize;
    for y in (0..height as usize).step_by(step_y) {
        for x in (0..width as usize).step_by(step_x) {
            let Some(offset) = y
                .checked_mul(stride)
                .and_then(|row| row.checked_add(x.saturating_mul(3)))
            else {
                continue;
            };
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

fn fit(width: u32, height: u32, max_dimension: u32) -> Result<(u32, u32), MediaError> {
    if width == 0 || height == 0 || max_dimension == 0 {
        return Err(MediaError::InvalidData(
            "video has invalid dimensions".to_owned(),
        ));
    }
    if width <= max_dimension && height <= max_dimension {
        return Ok((width, height));
    }
    if width >= height {
        Ok((
            max_dimension,
            ((u64::from(height) * u64::from(max_dimension) / u64::from(width)).max(1)) as u32,
        ))
    } else {
        Ok((
            ((u64::from(width) * u64::from(max_dimension) / u64::from(height)).max(1)) as u32,
            max_dimension,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn fitting_preserves_aspect_ratio_and_clamps_dimension() {
        assert_eq!(fit(1920, 1080, 640).unwrap(), (640, 360));
        assert_eq!(fit(640, 1920, 640).unwrap(), (213, 640));
        assert!(fit(0, 10, 640).is_err());
    }

    #[test]
    fn near_black_detection_allows_visible_highlights() {
        let mut pixels = vec![0; 16 * 16 * 3];
        assert!(rgb_is_near_black(&pixels, 16 * 3, 16, 16));
        for index in 0..8 {
            pixels[index * 3..index * 3 + 3].fill(255);
        }
        assert!(!rgb_is_near_black(&pixels, 16 * 3, 16, 16));
    }

    #[test]
    fn extracts_a_thumbnail_from_a_real_video_stream() {
        let Ok(version) = Command::new("ffmpeg").arg("-version").output() else {
            return;
        };
        assert!(
            version.status.success(),
            "ffmpeg is required for this integration fixture"
        );

        let directory = std::env::temp_dir().join(format!(
            "revaro-media-thumbnail-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock is after the unix epoch")
                .as_nanos()
        ));
        std::fs::create_dir_all(&directory).expect("fixture directory creates");
        let path = directory.join("fixture.mp4");
        let output = Command::new("ffmpeg")
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "color=c=blue:s=320x180:r=1:d=1",
                "-c:v",
                "mpeg4",
                "-pix_fmt",
                "yuv420p",
            ])
            .arg(&path)
            .output()
            .expect("ffmpeg fixture command starts");
        assert!(output.status.success(), "ffmpeg fixture failed: {output:?}");
        let bytes = std::fs::read(&path).expect("fixture reads");
        let jpeg = crate::MediaEngine.thumbnail(
            std::io::Cursor::new(bytes),
            640,
            false,
            CancellationToken::new(),
        );
        let jpeg = jpeg.expect("FFmpeg decodes a valid video");
        assert_eq!(&jpeg[..2], &[0xff, 0xd8]);
        let decoded = image::load_from_memory(&jpeg).expect("thumbnail decodes");
        assert_eq!(decoded.width(), 320);
        assert_eq!(decoded.height(), 180);
        std::fs::remove_dir_all(directory).expect("fixture directory removes");
    }

    #[test]
    fn extracts_a_cover_from_a_real_attached_picture_stream() {
        let Ok(version) = Command::new("ffmpeg").arg("-version").output() else {
            return;
        };
        assert!(
            version.status.success(),
            "ffmpeg is required for this integration fixture"
        );

        let directory = std::env::temp_dir().join(format!(
            "revaro-media-cover-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock is after the unix epoch")
                .as_nanos()
        ));
        std::fs::create_dir_all(&directory).expect("fixture directory creates");
        let cover_path = directory.join("cover.jpg");
        let audio_path = directory.join("audio.mp3");
        let cover = Command::new("ffmpeg")
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "color=c=red:s=100x80",
                "-frames:v",
                "1",
            ])
            .arg(&cover_path)
            .output()
            .expect("cover fixture command starts");
        assert!(cover.status.success(), "cover fixture failed: {cover:?}");
        let audio = Command::new("ffmpeg")
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=1000:duration=1",
                "-i",
            ])
            .arg(&cover_path)
            .args([
                "-map",
                "0:a:0",
                "-map",
                "1:v:0",
                "-c:a",
                "libmp3lame",
                "-b:a",
                "64k",
                "-c:v",
                "mjpeg",
                "-id3v2_version",
                "3",
                "-disposition:v:0",
                "attached_pic",
            ])
            .arg(&audio_path)
            .output()
            .expect("audio fixture command starts");
        assert!(audio.status.success(), "audio fixture failed: {audio:?}");
        let bytes = std::fs::read(&audio_path).expect("audio fixture reads");
        let jpeg = crate::MediaEngine.thumbnail(
            std::io::Cursor::new(bytes),
            640,
            true,
            CancellationToken::new(),
        );
        let jpeg = jpeg.expect("FFmpeg decodes an attached picture");
        let decoded = image::load_from_memory(&jpeg).expect("cover decodes");
        assert_eq!(decoded.width(), 100);
        assert_eq!(decoded.height(), 80);
        std::fs::remove_dir_all(directory).expect("fixture directory removes");
    }
}
