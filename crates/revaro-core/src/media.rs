//! Media metadata: the probe result the media engine produces, how it is
//! persisted in `media_metadata`, and the response bodies the player consumes.
//!
//! `chapters_json` and `subtitles_json` columns are exactly the JSON encoding of
//! [`MediaChapter`] and [`EmbeddedSubtitle`] respectively, so the shared crate
//! owns their shape too.

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

/// One subtitle stream embedded in a container.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmbeddedSubtitle {
    /// Zero-based stream index inside the container.
    #[serde(default)]
    pub index: i32,
    /// Codec short name, for example `subrip` or `ass`.
    #[serde(default)]
    pub codec: String,
    /// ISO language tag, omitted when the container does not declare one.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub language: String,
    /// Stream title, omitted when the container does not declare one.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title: String,
    /// Whether the container marks this stream as the default.
    #[serde(default)]
    pub default: bool,
    /// Whether this stream only covers forced-narrative sections.
    #[serde(default)]
    pub forced: bool,
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
    /// Embedded subtitle streams.
    #[serde(default)]
    pub subtitles: Vec<EmbeddedSubtitle>,
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
    pub id: i32,
    /// Chapter title.
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
    pub duration: f64,
    /// Chapter marks.
    #[serde(default)]
    pub chapters: Vec<AudioChapter>,
    /// Thumbnail URL for the embedded cover, empty when there is none.
    #[serde(default)]
    pub cover_url: String,
    /// Whether an embedded cover exists.
    pub has_cover: bool,
}

/// One selectable subtitle track for the video player.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoSubtitleTrack {
    /// Stable identifier used in the subtitle URL.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Short label shown in the track menu.
    pub label: String,
    /// ISO language tag, empty when unknown.
    #[serde(default)]
    pub language: String,
    /// WebVTT URL.
    pub url: String,
    /// Whether the track should be selected by default.
    #[serde(default)]
    pub default: bool,
    /// Whether the track only covers forced-narrative sections.
    #[serde(default)]
    pub forced: bool,
}

/// Body of `GET /api/files/{id}/video`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoMedia {
    /// Available subtitle tracks.
    #[serde(default)]
    pub subtitles: Vec<VideoSubtitleTrack>,
}

/// Body of `POST /api/files/{id}/media/reanalyze`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReanalyzeResult {
    /// Always `ready` on success.
    pub status: String,
    /// Number of subtitle streams found.
    pub subtitles: usize,
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
            subtitles: vec![EmbeddedSubtitle {
                index: 2,
                codec: "subrip".into(),
                language: "eng".into(),
                title: String::new(),
                default: true,
                forced: false,
            }],
        };
        let json = serde_json::to_value(&probe).unwrap();
        assert_eq!(json["duration_ms"], 1500);
        assert_eq!(
            json["chapters"][0],
            serde_json::json!({"title": "Intro", "start_ms": 0, "end_ms": 1500})
        );
        // Empty optional strings are omitted, matching the historical tags.
        assert!(json["subtitles"][0].get("title").is_none());
        assert_eq!(json["subtitles"][0]["language"], "eng");
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
        };
        let json = serde_json::to_value(&body).unwrap();
        assert_eq!(json["chapters"][0]["id"], 1);
        assert_eq!(json["has_cover"], true);
    }
}
