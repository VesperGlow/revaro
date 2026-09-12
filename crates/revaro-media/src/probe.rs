use std::io::{Read, Seek};

use ffmpeg::{Rational, media::Type};
use ffmpeg_next as ffmpeg;
use tokio_util::sync::CancellationToken;

use crate::{
    EmbeddedSubtitle, MediaChapter, MediaError, MediaProbe, check_cancel, init_ffmpeg, open_input,
};

pub fn probe<R: Read + Seek + Send + 'static>(
    reader: R,
    cancel: CancellationToken,
) -> Result<MediaProbe, MediaError> {
    init_ffmpeg()?;
    let input = open_input(reader, cancel.clone())?;
    let mut result = MediaProbe {
        duration_ms: (input.duration() / 1000).max(0),
        container: input.format().name().to_ascii_lowercase(),
        bitrate: input.bit_rate().max(0),
        ..MediaProbe::default()
    };

    for stream in input.streams() {
        check_cancel(&cancel)?;
        let parameters = stream.parameters();
        let codec = parameters.id().name().to_ascii_lowercase();
        match parameters.medium() {
            Type::Video if result.video_codec.is_empty() => {
                result.video_codec = codec;
                result.frame_rate = stream.avg_frame_rate().to_string();
                result.video_level = codec_level(&parameters);

                if let Ok(context) = ffmpeg::codec::context::Context::from_parameters(parameters)
                    && let Ok(decoder) = context.decoder().video()
                {
                    result.width = to_i32(decoder.width());
                    result.height = to_i32(decoder.height());
                    result.video_profile = profile_name(decoder.profile()).to_owned();
                }
            }
            Type::Audio if result.audio_codec.is_empty() => result.audio_codec = codec,
            Type::Subtitle => {
                let metadata = stream.metadata();
                let disposition = stream.disposition();
                result.subtitles.push(EmbeddedSubtitle {
                    index: i32::try_from(stream.index()).unwrap_or(i32::MAX),
                    codec,
                    language: metadata.get("language").unwrap_or_default().to_owned(),
                    title: metadata.get("title").unwrap_or_default().to_owned(),
                    default: disposition.contains(ffmpeg::format::stream::Disposition::DEFAULT),
                    forced: disposition.contains(ffmpeg::format::stream::Disposition::FORCED),
                });
            }
            _ => {}
        }
    }

    for (index, chapter) in input.chapters().enumerate() {
        check_cancel(&cancel)?;
        let title = chapter
            .metadata()
            .get("title")
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| format!("Chapter {}", index + 1));
        result.chapters.push(MediaChapter {
            title,
            start_ms: millis(chapter.start(), chapter.time_base()),
            end_ms: millis(chapter.end(), chapter.time_base()),
        });
    }

    Ok(result)
}

fn to_i32(value: u32) -> i32 {
    i32::try_from(value).unwrap_or(i32::MAX)
}

fn millis(value: i64, base: Rational) -> i64 {
    if value < 0 {
        return 0;
    }
    let numerator = i128::from(value) * i128::from(base.numerator()) * 1000;
    let denominator = i128::from(base.denominator()).max(1);
    (numerator / denominator).clamp(0, i128::from(i64::MAX)) as i64
}

fn profile_name(profile: ffmpeg::codec::Profile) -> &'static str {
    use ffmpeg::codec::profile::{H264, HEVC, Profile};

    match profile {
        Profile::H264(H264::Baseline) => "Baseline",
        Profile::H264(H264::Main) => "Main",
        Profile::H264(H264::High) => "High",
        Profile::HEVC(HEVC::Main10) => "Main 10",
        Profile::HEVC(HEVC::Main) => "Main",
        _ => "",
    }
}

fn codec_level(parameters: &ffmpeg::codec::Parameters) -> i32 {
    // `ffmpeg-next` exposes codec profile through the safe decoder wrapper but
    // does not expose AVCodecParameters.level. The pointer is owned by the
    // stream for the duration of this call and is only read here.
    //
    // SAFETY: `parameters` is a live stream-owned AVCodecParameters value from
    // the same FFmpeg context, and this function performs one read-only field
    // access before that context can be dropped.
    unsafe { (*parameters.as_ptr()).level }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn chapter_timestamps_use_saturating_milliseconds() {
        assert_eq!(millis(90, Rational(1, 10)), 9000);
        assert_eq!(millis(-1, Rational(1, 1)), 0);
        assert_eq!(millis(i64::MAX, Rational(1, 1)), i64::MAX);
    }

    #[test]
    fn only_legacy_profile_labels_are_exposed() {
        use ffmpeg::codec::profile::{H264, HEVC, Profile};

        assert_eq!(profile_name(Profile::H264(H264::Baseline)), "Baseline");
        assert_eq!(profile_name(Profile::H264(H264::High)), "High");
        assert_eq!(profile_name(Profile::HEVC(HEVC::Main10)), "Main 10");
        assert_eq!(profile_name(Profile::Unknown), "");
    }

    #[test]
    fn probes_a_real_pcm_wav_without_a_sidecar_process() {
        let sample_rate = 8_000u32;
        let sample_count = sample_rate;
        let pcm = vec![0u8; sample_count as usize * 2];
        let data_size = u32::try_from(pcm.len()).unwrap();
        let riff_size = 36 + data_size;
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&riff_size.to_le_bytes());
        wav.extend_from_slice(b"WAVE");
        wav.extend_from_slice(b"fmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&sample_rate.to_le_bytes());
        wav.extend_from_slice(&(sample_rate * 2).to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&data_size.to_le_bytes());
        wav.extend_from_slice(&pcm);

        let probe = crate::MediaEngine.probe(Cursor::new(wav), CancellationToken::new());
        let probe = probe.expect("FFmpeg probes a valid WAV");
        assert_eq!(probe.container, "wav");
        assert_eq!(probe.audio_codec, "pcm_s16le");
        assert_eq!(probe.duration_ms, 1_000);
        assert!(!probe.has_video_stream());
    }
}
