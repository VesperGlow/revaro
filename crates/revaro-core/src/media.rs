//! Media metadata: the probe result the media engine produces, how it is
//! persisted in `media_metadata`, and the response bodies the player consumes.
//!
//! `chapters_json` stores the JSON encoding of [`MediaChapter`].

use crate::serde_helpers::null_default;

use serde::{Deserialize, Serialize};

/// One chapter mark inside an audio or video file.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaChapter {
    /// Chapter title, possibly empty.
    #[serde(default)]
    pub title: String,
    /// Start offset in milliseconds.
    #[serde(default)]
    pub start_ms: i64,
    /// End offset in milliseconds.
    #[serde(default)]
    pub end_ms: i64,
}

/// Everything the media engine learns about one file.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaProbe {
    /// Duration in milliseconds, `0` when unknown.
    #[serde(default)]
    pub duration_ms: i64,
    /// Container format short name.
    #[serde(default)]
    pub container: String,
    /// Primary video codec, empty for audio-only files.
    #[serde(default)]
    pub video_codec: String,
    /// Primary audio codec, empty for silent files.
    #[serde(default)]
    pub audio_codec: String,
    /// Video width in pixels, `0` when unknown.
    #[serde(default)]
    pub width: i32,
    /// Video height in pixels, `0` when unknown.
    #[serde(default)]
    pub height: i32,
    /// Overall bitrate in bits per second, `0` when unknown.
    #[serde(default)]
    pub bitrate: i64,
    /// Frame rate as a rational string such as `30000/1001`.
    #[serde(default)]
    pub frame_rate: String,
    /// Video profile name.
    #[serde(default)]
    pub video_profile: String,
    /// Video level as an integer.
    #[serde(default)]
    pub video_level: i32,
    /// Chapter marks.
    #[serde(default)]
    pub chapters: Vec<MediaChapter>,
}

impl MediaProbe {
    /// True when the file has a video stream, which is also how the product
    /// decides whether a generated cover exists for an audio file.
    #[must_use]
    pub fn has_video_stream(&self) -> bool {
        !self.video_codec.is_empty()
    }

    /// Duration as fractional seconds, the unit the player API uses.
    #[must_use]
    pub fn duration_seconds(&self) -> f64 {
        self.duration_ms as f64 / 1000.0
    }
}

/// One chapter as the audio player consumes it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AudioChapter {
    /// 1-based chapter number.
    #[serde(default, deserialize_with = "null_default")]
    pub id: i32,
    /// Chapter title. The old player renders an empty title when it is absent.
    #[serde(default, deserialize_with = "null_default")]
    pub title: String,
    /// Start offset in seconds.
    pub start: f64,
    /// End offset in seconds.
    pub end: f64,
}

/// Body of `GET /api/files/{id}/audio`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AudioMedia {
    /// Duration in seconds.
    #[serde(default, deserialize_with = "null_default")]
    pub duration: f64,
    /// Chapter marks.
    #[serde(default, deserialize_with = "null_default")]
    pub chapters: Vec<AudioChapter>,
    /// Same-name external WebVTT cues; the frontend synchronizes to its audio clock.
    #[serde(default, deserialize_with = "null_default")]
    pub subtitles: Vec<VttCue>,
    /// Thumbnail URL for the embedded cover, empty when there is none.
    #[serde(default, deserialize_with = "null_default")]
    pub cover_url: String,
    /// Whether an embedded cover exists.
    #[serde(default, deserialize_with = "null_default")]
    pub has_cover: bool,
}

/// A timed cue within one track, never a separate audio file.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct VttCue {
    pub start: f64,
    pub end: f64,
    pub text: String,
}

/// Parse UTF-8 WebVTT, allowing cue IDs, settings, BOM and CRLF. Invalid cues
/// and NOTE/STYLE/REGION blocks are skipped independently of valid cues.
#[must_use]
pub fn parse_webvtt(source: &str) -> Vec<VttCue> {
    let normalized = source
        .trim_start_matches('\u{feff}')
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    let mut lines = normalized.lines();
    if !lines.next().is_some_and(|line| {
        line == "WEBVTT" || line.starts_with("WEBVTT ") || line.starts_with("WEBVTT\t")
    }) {
        return Vec::new();
    }
    let mut cues = Vec::new();
    let mut block = Vec::new();
    for line in lines.chain(std::iter::once("")) {
        if line.trim().is_empty() {
            if let Some(cue) = parse_vtt_block(&block) {
                cues.push(cue);
            }
            block.clear();
        } else {
            block.push(line);
        }
    }
    cues.sort_by(|a, b| a.start.total_cmp(&b.start));
    cues
}

fn parse_vtt_block(lines: &[&str]) -> Option<VttCue> {
    let first = *lines.first()?;
    if first == "NOTE"
        || first.starts_with("NOTE ")
        || first.starts_with("NOTE\t")
        || matches!(first, "STYLE" | "REGION")
    {
        return None;
    }
    let index = if first.contains("-->") { 0 } else { 1 };
    let (start, end) = lines.get(index)?.split_once("-->")?;
    let start = vtt_timestamp(start.trim())?;
    let end = vtt_timestamp(end.split_whitespace().next()?)?;
    if end <= start {
        return None;
    }
    Some(VttCue {
        start,
        end,
        text: vtt_plain_text(&lines[index + 1..].join("\n")),
    })
}

fn vtt_timestamp(value: &str) -> Option<f64> {
    let parts = value.split(':').collect::<Vec<_>>();
    if !(2..=3).contains(&parts.len()) {
        return None;
    }
    let number = |s: &str| -> Option<u64> {
        if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        s.parse().ok()
    };
    let (seconds, millis) = parts.last()?.split_once('.')?;
    if seconds.len() != 2 || millis.len() != 3 {
        return None;
    }
    let seconds = number(seconds)?;
    let minutes_text = parts[parts.len() - 2];
    let minutes = number(minutes_text)?;
    let hours = if parts.len() == 3 {
        number(parts[0])?
    } else {
        0
    };
    if minutes_text.len() != 2 || minutes >= 60 || seconds >= 60 {
        return None;
    }
    let total = hours
        .checked_mul(3600)?
        .checked_add(minutes * 60 + seconds)?;
    Some(total as f64 + number(millis)? as f64 / 1000.0)
}

fn vtt_plain_text(value: &str) -> String {
    // Render as text, never HTML. Strip cue styling/voice/timestamp tags.
    let mut result = String::new();
    let mut tag = false;
    for ch in value.chars() {
        match ch {
            '<' => tag = true,
            '>' if tag => tag = false,
            _ if !tag => result.push(ch),
            _ => (),
        }
    }
    result
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&nbsp;", "\u{a0}")
        .replace("&lrm;", "\u{200e}")
        .replace("&rlm;", "\u{200f}")
        .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_matches_the_data_plane_wire_format() {
        let probe = MediaProbe {
            duration_ms: 1500,
            container: "mov,mp4".into(),
            video_codec: "h264".into(),
            audio_codec: "aac".into(),
            width: 1920,
            height: 1080,
            bitrate: 4_000_000,
            frame_rate: "30000/1001".into(),
            video_profile: "High".into(),
            video_level: 41,
            chapters: vec![MediaChapter {
                title: "Intro".into(),
                start_ms: 0,
                end_ms: 1500,
            }],
        };
        let json = serde_json::to_value(&probe).unwrap();
        assert_eq!(json["duration_ms"], 1500);
        assert_eq!(
            json["chapters"][0],
            serde_json::json!({"title": "Intro", "start_ms": 0, "end_ms": 1500})
        );
    }

    #[test]
    fn probe_helpers() {
        let audio = MediaProbe {
            duration_ms: 2500,
            ..MediaProbe::default()
        };
        assert!(!audio.has_video_stream());
        assert!((audio.duration_seconds() - 2.5).abs() < f64::EPSILON);
        let video = MediaProbe {
            video_codec: "vp9".into(),
            ..MediaProbe::default()
        };
        assert!(video.has_video_stream());
    }

    #[test]
    fn audio_response_uses_seconds() {
        let body = AudioMedia {
            duration: 12.5,
            chapters: vec![AudioChapter {
                id: 1,
                title: "One".into(),
                start: 0.0,
                end: 12.5,
            }],
            cover_url: "/api/files/x/thumbnail?v=1".into(),
            has_cover: true,
            subtitles: Vec::new(),
        };
        let json = serde_json::to_value(&body).unwrap();
        assert_eq!(json["chapters"][0]["id"], 1);
        assert_eq!(json["has_cover"], true);
    }

    #[test]
    fn audio_metadata_defaults_missing_duration_and_cover_fields() {
        let media: AudioMedia = serde_json::from_value(serde_json::json!({
            "chapters": [
                {"id": 1, "start": 0.0, "end": 12.5},
                {"id": 2, "title": null, "start": 12.5, "end": 25.0}
            ]
        }))
        .unwrap();

        assert_eq!(media.duration, 0.0);
        assert!(!media.has_cover);
        assert!(media.cover_url.is_empty());
        assert_eq!(media.chapters.len(), 2);
        assert!(media.chapters[0].title.is_empty());
        assert!(media.chapters[1].title.is_empty());

        let sparse_ids: AudioMedia = serde_json::from_value(serde_json::json!({
            "chapters": [
                {"title": "missing", "start": 0.0, "end": 12.5},
                {"id": null, "title": "null", "start": 12.5, "end": 25.0}
            ]
        }))
        .unwrap();
        assert_eq!(sparse_ids.chapters[0].id, 0);
        assert_eq!(sparse_ids.chapters[1].id, 0);

        let invalid = serde_json::from_value::<AudioMedia>(serde_json::json!({
            "chapters": [{"id": 1, "title": 42, "start": 0.0, "end": 12.5}]
        }));
        assert!(invalid.is_err());
    }

    #[test]
    fn media_responses_treat_nullable_top_level_fields_as_defaults() {
        let audio: AudioMedia = serde_json::from_value(serde_json::json!({
            "duration": null,
            "chapters": null,
            "cover_url": null,
            "has_cover": null
        }))
        .unwrap();
        assert_eq!(audio.duration, 0.0);
        assert!(audio.chapters.is_empty());
        assert!(audio.cover_url.is_empty());
        assert!(!audio.has_cover);
    }

    #[test]
    fn webvtt_parses_ids_settings_multiline_and_skips_invalid_blocks() {
        let cues = parse_webvtt(
            "\u{feff}WEBVTT\r\n\r\nNOTE ignored\r\n00:00.000 --> 00:01.000\r\nno\r\n\r\nsecond\r\n00:01:00.100 --> 00:01:02.250 align:start\r\n<v voice><b>Second</b> &amp; &lt;text&gt;\r\nline 2\r\n\r\n00:00.500 --> 00:02.000\r\nFirst\r\n\r\n00:99.000 --> 00:02.000\r\nbad\r\n\r\n00:02.000 --> 00:01.000\r\nbad\r\n",
        );
        assert_eq!(cues.len(), 2);
        assert_eq!(cues[0].start, 0.5);
        assert_eq!(cues[1].start, 60.1);
        assert_eq!(cues[1].end, 62.25);
        assert_eq!(cues[1].text, "Second & <text>\nline 2");
        assert!(parse_webvtt("00:00.000 --> 00:01.000\nmissing header").is_empty());
    }
}
