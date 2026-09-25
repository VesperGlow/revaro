//! Media metadata: the probe result the media engine produces, how it is
//! persisted in `media_metadata`, and the response bodies the player consumes.
//!
//! `chapters_json` stores the JSON encoding of [`MediaChapter`].

use serde::{Deserialize, Serialize};

fn deserialize_nullable_f64<'de, D>(deserializer: D) -> Result<f64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<f64>::deserialize(deserializer)?.unwrap_or_default())
}

fn deserialize_nullable_string<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<String>::deserialize(deserializer)?.unwrap_or_default())
}

fn deserialize_nullable_bool<'de, D>(deserializer: D) -> Result<bool, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<bool>::deserialize(deserializer)?.unwrap_or(false))
}

fn deserialize_nullable_i32<'de, D>(deserializer: D) -> Result<i32, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<i32>::deserialize(deserializer)?.unwrap_or_default())
}

fn deserialize_nullable_vec<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Ok(Option::<Vec<T>>::deserialize(deserializer)?.unwrap_or_default())
}

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
    #[serde(default, deserialize_with = "deserialize_nullable_i32")]
    pub id: i32,
    /// Chapter title. The old player renders an empty title when it is absent.
    #[serde(default, deserialize_with = "deserialize_nullable_string")]
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
    #[serde(default, deserialize_with = "deserialize_nullable_f64")]
    pub duration: f64,
    /// Chapter marks.
    #[serde(default, deserialize_with = "deserialize_nullable_vec")]
    pub chapters: Vec<AudioChapter>,
    /// Thumbnail URL for the embedded cover, empty when there is none.
    #[serde(default, deserialize_with = "deserialize_nullable_string")]
    pub cover_url: String,
    /// Whether an embedded cover exists.
    #[serde(default, deserialize_with = "deserialize_nullable_bool")]
    pub has_cover: bool,
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
}
