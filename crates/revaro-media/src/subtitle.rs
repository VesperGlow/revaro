use std::collections::HashMap;
use std::io::{Read, Seek};

use ffmpeg::media::Type;
use ffmpeg_next as ffmpeg;
use tokio_util::sync::CancellationToken;

use crate::{
    MAX_CONVERTED_SUBTITLE_BYTES, MAX_SUBTITLE_BYTES, MediaError, check_cancel, init_ffmpeg,
    open_input,
};

pub fn subtitle<R: Read + Seek + Send + 'static>(
    reader: R,
    format: Option<&str>,
    stream_index: Option<usize>,
    cancel: CancellationToken,
) -> Result<Vec<u8>, MediaError> {
    if let Some(index) = stream_index {
        return embedded_subtitle(reader, index, cancel);
    }

    check_cancel(&cancel)?;
    let mut raw = Vec::new();
    reader
        .take((MAX_SUBTITLE_BYTES + 1) as u64)
        .read_to_end(&mut raw)
        .map_err(|error| MediaError::Input(error.to_string()))?;
    check_cancel(&cancel)?;
    if raw.len() > MAX_SUBTITLE_BYTES {
        return Err(MediaError::SubtitleTooLarge);
    }
    let text = String::from_utf8(raw).map_err(|_| MediaError::SubtitleNotUtf8)?;
    let format = format
        .unwrap_or("vtt")
        .trim_start_matches('.')
        .to_ascii_lowercase();
    let output = match format.as_str() {
        "vtt" => {
            if !text.trim_start().starts_with("WEBVTT") {
                return Err(MediaError::InvalidWebVtt);
            }
            text
        }
        "srt" => srt_to_vtt(&text),
        "ass" | "ssa" => ass_to_vtt(&text),
        _ => return Err(MediaError::UnsupportedSubtitleFormat),
    };
    if output.len() > MAX_CONVERTED_SUBTITLE_BYTES {
        return Err(MediaError::ConvertedSubtitleTooLarge);
    }
    Ok(output.into_bytes())
}

fn embedded_subtitle<R: Read + Seek + Send + 'static>(
    reader: R,
    index: usize,
    cancel: CancellationToken,
) -> Result<Vec<u8>, MediaError> {
    init_ffmpeg()?;
    let mut input = open_input(reader, cancel.clone())?;
    let stream = input.stream(index).ok_or_else(|| {
        MediaError::InvalidSubtitleStream("subtitle stream index is out of range".to_owned())
    })?;
    if stream.parameters().medium() != Type::Subtitle {
        return Err(MediaError::InvalidSubtitleStream(
            "selected stream is not a subtitle".to_owned(),
        ));
    }
    let base = stream.time_base();
    let mut decoder = ffmpeg::codec::context::Context::from_parameters(stream.parameters())
        .map_err(|error| MediaError::Input(error.to_string()))?
        .decoder()
        .subtitle()
        .map_err(|error| MediaError::Input(error.to_string()))?;
    let mut output = String::from("WEBVTT\n\n");

    for (packet_stream, packet) in input.packets() {
        check_cancel(&cancel)?;
        if packet_stream.index() != index {
            continue;
        }
        let packet_start = packet
            .pts()
            .map(|pts| pts as f64 * f64::from(base))
            .unwrap_or_default();
        let packet_duration = packet.duration() as f64 * f64::from(base);
        let mut decoded = ffmpeg::Subtitle::new();
        if !decoder
            .decode(&packet, &mut decoded)
            .map_err(|error| MediaError::Input(error.to_string()))?
        {
            continue;
        }
        let start = decoded
            .pts()
            .map(|pts| pts as f64 / 1_000_000.0)
            .unwrap_or(packet_start)
            + decoded.start() as f64 / 1000.0;
        let display_duration = decoded.end().saturating_sub(decoded.start()) as f64 / 1000.0;
        // Matroska ASS packets often leave the display times at zero. The
        // packet duration is the useful fallback in that case; one millisecond
        // would make an otherwise valid cue effectively invisible.
        let end = start + subtitle_cue_duration(display_duration, packet_duration);
        let mut lines = Vec::new();
        let mut settings = "";
        for rect in decoded.rects() {
            match rect {
                ffmpeg::subtitle::Rect::Text(value) => lines.push(value.get().to_owned()),
                ffmpeg::subtitle::Rect::Ass(value) => {
                    if settings.is_empty() {
                        settings = decoded_ass_vtt_settings(value.get());
                    }
                    lines.push(strip_decoded_ass(value.get()));
                }
                _ => {}
            }
        }
        if !lines.is_empty() {
            output.push_str(&vtt_time(start));
            output.push_str(" --> ");
            output.push_str(&vtt_time(end));
            output.push_str(settings);
            output.push('\n');
            output.push_str(&lines.join("\n"));
            output.push_str("\n\n");
        }
        if output.len() > MAX_CONVERTED_SUBTITLE_BYTES {
            return Err(MediaError::ConvertedSubtitleTooLarge);
        }
    }
    Ok(output.into_bytes())
}

fn srt_to_vtt(text: &str) -> String {
    let normalized = text.trim_start_matches('\u{feff}').replace("\r\n", "\n");
    let mut output = String::from("WEBVTT\n\n");
    for line in normalized.lines() {
        if line.contains(" --> ") {
            output.push_str(&line.replace(',', "."));
        } else {
            output.push_str(line);
        }
        output.push('\n');
    }
    output
}

fn ass_to_vtt(text: &str) -> String {
    let mut output = String::from("WEBVTT\n\n");
    let mut style_alignments = HashMap::new();
    let mut style_format = Vec::new();
    let mut in_styles = false;
    let mut legacy_ssa_styles = false;

    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            legacy_ssa_styles = trimmed.eq_ignore_ascii_case("[V4 Styles]");
            in_styles = trimmed.eq_ignore_ascii_case("[V4+ Styles]") || legacy_ssa_styles;
        } else if in_styles {
            if let Some(value) = trimmed.strip_prefix("Format:") {
                style_format = value
                    .split(',')
                    .map(|field| field.trim().to_ascii_lowercase())
                    .collect();
            } else if let Some(value) = trimmed.strip_prefix("Style:") {
                let fields: Vec<_> = value.split(',').map(str::trim).collect();
                let name = style_format
                    .iter()
                    .position(|field| field == "name")
                    .and_then(|index| fields.get(index));
                let alignment = style_format
                    .iter()
                    .position(|field| field == "alignment")
                    .and_then(|index| fields.get(index))
                    .and_then(|field| field.parse::<u8>().ok());
                if let (Some(name), Some(alignment)) = (name, alignment) {
                    let alignment = if legacy_ssa_styles {
                        match alignment {
                            5..=7 => alignment + 2,
                            9..=11 => alignment - 5,
                            _ => alignment,
                        }
                    } else {
                        alignment
                    };
                    style_alignments.insert(name.to_ascii_lowercase(), alignment);
                }
            }
        }
    }

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
        let (body, override_alignment) = clean_ass_text(fields[9]);
        if body.is_empty() {
            continue;
        }
        let alignment = override_alignment.or_else(|| {
            style_alignments
                .get(&fields[3].trim().to_ascii_lowercase())
                .copied()
        });
        output.push_str(&vtt_time(start));
        output.push_str(" --> ");
        output.push_str(&vtt_time(end));
        output.push_str(ass_vtt_settings(alignment));
        output.push('\n');
        output.push_str(&body);
        output.push_str("\n\n");
    }
    output
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
    clean_ass_text(body).0
}

fn clean_ass_text(body: &str) -> (String, Option<u8>) {
    let mut result = String::new();
    let mut tag = false;
    let mut tag_body = String::new();
    let mut alignment = None;
    let mut drawing = false;
    for ch in body.chars() {
        match ch {
            '{' if !tag => {
                tag = true;
                tag_body.clear();
            }
            '}' if tag => {
                for candidate in 1..=9 {
                    if tag_body.contains(&format!(r"\an{candidate}")) {
                        alignment = Some(candidate);
                    }
                }
                for command in tag_body.split('\\').map(str::trim) {
                    if let Some(level) = command
                        .strip_prefix('p')
                        .and_then(|value| value.split_whitespace().next())
                        .and_then(|value| value.parse::<u8>().ok())
                    {
                        drawing = level > 0;
                    }
                }
                tag = false;
            }
            _ if tag => tag_body.push(ch),
            _ if drawing => {}
            '&' => result.push_str("&amp;"),
            '<' => result.push_str("&lt;"),
            '>' => result.push_str("&gt;"),
            _ => result.push(ch),
        }
    }
    (
        result
            .replace("\\N", "\n")
            .replace("\\n", "\n")
            .replace("\\h", " "),
        alignment,
    )
}

fn ass_vtt_settings(alignment: Option<u8>) -> &'static str {
    match alignment {
        Some(7..=9) => " line:10%",
        Some(4..=6) => " line:50%",
        _ => "",
    }
}

fn strip_decoded_ass(value: &str) -> String {
    // AVSubtitleRect::ass omits Dialogue, Start and End. Its nine fields are
    // ReadOrder, Layer, Style, Name, three margins, Effect and Text.
    let fields: Vec<_> = value.splitn(9, ',').collect();
    strip_ass(if fields.len() == 9 { fields[8] } else { value })
}

fn decoded_ass_vtt_settings(value: &str) -> &'static str {
    let fields: Vec<_> = value.splitn(9, ',').collect();
    ass_vtt_settings(clean_ass_text(if fields.len() == 9 { fields[8] } else { value }).1)
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::process::Command;

    #[test]
    fn srt_conversion_normalizes_bom_and_fraction_separator() {
        let source = "\u{feff}1\r\n00:00:01,250 --> 00:00:02,500\r\nA <tag>\r\n";
        let output = subtitle(
            Cursor::new(source.as_bytes().to_vec()),
            Some(".srt"),
            None,
            CancellationToken::new(),
        )
        .unwrap();
        let output = String::from_utf8(output).unwrap();
        assert!(output.starts_with("WEBVTT\n\n"));
        assert!(output.contains("00:00:01.250 --> 00:00:02.500"));
        assert!(output.contains("A <tag>"));
    }

    #[test]
    fn ass_conversion_escapes_text_and_preserves_alignment() {
        let source = "[Script Info]\n[V4+ Styles]\nFormat: Name, Alignment\nStyle: Default, 8\n[Events]\nDialogue: 0,0:00:01.00,0:00:02.00,Default,,,,,0,{\\an8}A & B\\Nline\n";
        let output = ass_to_vtt(source);
        assert!(output.contains("00:00:01.000 --> 00:00:02.000 line:10%"));
        assert!(output.contains("A &amp; B\nline"));
    }

    #[test]
    fn malformed_external_subtitles_have_stable_errors() {
        let invalid_vtt = subtitle(
            Cursor::new(b"not vtt".to_vec()),
            Some("vtt"),
            None,
            CancellationToken::new(),
        )
        .unwrap_err();
        assert!(matches!(invalid_vtt, MediaError::InvalidWebVtt));

        let unsupported = subtitle(
            Cursor::new(b"anything".to_vec()),
            Some("ssa2"),
            None,
            CancellationToken::new(),
        )
        .unwrap_err();
        assert!(matches!(unsupported, MediaError::UnsupportedSubtitleFormat));
    }

    #[test]
    fn time_and_duration_helpers_are_bounded() {
        assert_eq!(vtt_time(-1.0), "00:00:00.000");
        assert_eq!(subtitle_cue_duration(0.0, 1.5), 1.5);
        assert_eq!(subtitle_cue_duration(f64::NAN, 0.0), 0.001);
    }

    #[test]
    fn converts_a_real_embedded_subrip_stream_to_webvtt() {
        let Ok(version) = Command::new("ffmpeg").arg("-version").output() else {
            return;
        };
        assert!(
            version.status.success(),
            "ffmpeg is required for this integration fixture"
        );

        let directory = std::env::temp_dir().join(format!(
            "revaro-media-subtitle-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock is after the unix epoch")
                .as_nanos()
        ));
        std::fs::create_dir_all(&directory).expect("fixture directory creates");
        let subtitle_path = directory.join("subtitle.srt");
        let video_path = directory.join("movie.mkv");
        std::fs::write(
            &subtitle_path,
            b"1\n00:00:00,500 --> 00:00:01,500\nHello embedded\n",
        )
        .expect("subtitle fixture writes");
        let output = Command::new("ffmpeg")
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "color=c=blue:s=160x90:r=1:d=2",
                "-f",
                "srt",
                "-i",
            ])
            .arg(&subtitle_path)
            .args([
                "-map",
                "0:v:0",
                "-map",
                "1:0",
                "-c:v",
                "mpeg4",
                "-c:s",
                "srt",
                "-metadata:s:s:0",
                "language=eng",
                "-shortest",
            ])
            .arg(&video_path)
            .output()
            .expect("embedded subtitle fixture command starts");
        assert!(
            output.status.success(),
            "embedded subtitle fixture failed: {output:?}"
        );
        let bytes = std::fs::read(&video_path).expect("embedded subtitle fixture reads");
        let engine = crate::MediaEngine;
        let probe = engine
            .probe(Cursor::new(bytes.clone()), CancellationToken::new())
            .expect("FFmpeg probes the embedded subtitle");
        assert_eq!(probe.subtitles.len(), 1);
        assert_eq!(probe.subtitles[0].index, 1);
        assert_eq!(probe.subtitles[0].codec, "subrip");
        assert_eq!(probe.subtitles[0].language, "eng");
        let converted = engine
            .subtitle(Cursor::new(bytes), None, Some(1), CancellationToken::new())
            .expect("FFmpeg converts the embedded subtitle");
        let converted = String::from_utf8(converted).expect("converted subtitle is utf8");
        assert!(converted.starts_with("WEBVTT\n\n"));
        assert!(converted.contains("00:00:00.500 --> 00:00:01.500"));
        assert!(converted.contains("Hello embedded"));
        std::fs::remove_dir_all(directory).expect("fixture directory removes");
    }
}
