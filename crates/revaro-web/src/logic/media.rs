//! Target-independent media-player rules.
//!
//! The DOM components are responsible for event wiring and playback APIs. The
//! rules that decide which chapter is active and how a seek competes with saved
//! progress stay here so native tests can exercise edge cases without a browser.

use revaro_core::media::{AudioChapter, VttCue};

/// Support overlapping cues and leave subtitle gaps blank, including after seek.
#[must_use]
pub fn subtitle_text(cues: &[VttCue], time: f64) -> String {
    active_subtitle_indices(cues, time)
        .into_iter()
        .map(|index| cues[index].text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Nearby lyric lines follow seeks and preserve blank gaps and overlapping cues.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct SubtitleContext {
    pub previous: String,
    pub current: String,
    pub next: String,
}

#[must_use]
pub fn subtitle_context(cues: &[VttCue], time: f64) -> SubtitleContext {
    if !time.is_finite() {
        return SubtitleContext::default();
    }
    let active = active_subtitle_indices(cues, time);
    let cursor = cues.partition_point(|cue| cue.start <= time);
    let previous = active.first().copied().unwrap_or(cursor).checked_sub(1);
    let next = active.last().map_or(cursor, |index| index + 1);
    SubtitleContext {
        previous: previous
            .and_then(|index| cues.get(index))
            .map(|cue| cue.text.clone())
            .unwrap_or_default(),
        current: active
            .into_iter()
            .map(|index| cues[index].text.as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        next: cues
            .get(next)
            .map(|cue| cue.text.clone())
            .unwrap_or_default(),
    }
}

/// The transcript can highlight multiple speakers while leaving pauses unlit.
#[must_use]
pub fn active_subtitle_indices(cues: &[VttCue], time: f64) -> Vec<usize> {
    if !time.is_finite() {
        return Vec::new();
    }
    cues[..cues.partition_point(|cue| cue.start <= time)]
        .iter()
        .enumerate()
        .filter_map(|(index, cue)| (time < cue.end).then_some(index))
        .collect()
}

/// Clamp a visual progress value to the range accepted by CSS widths.
#[must_use]
pub fn clamp_percent(value: f64) -> f64 {
    if !value.is_finite() {
        return 0.0;
    }
    value.clamp(0.0, 100.0)
}

/// Return the chapter containing `time`, keeping the last chapter active at its
/// exact end so a duration rounded by the media element does not render no
/// chapter at all.
#[must_use]
pub fn active_chapter_index(chapters: &[AudioChapter], time: f64) -> usize {
    if chapters.is_empty() {
        return 0;
    }
    let time = if time.is_finite() { time.max(0.0) } else { 0.0 };
    chapters
        .iter()
        .rposition(|chapter| time >= chapter.start)
        .unwrap_or(0)
}

/// Explicit source transitions preserve their target, including zero and the
/// final seconds. Only historical progress uses the completed-media policy.
#[derive(Clone, Copy)]
pub enum PlaybackPosition {
    Resume(f64),
    Seek(f64),
}

impl PlaybackPosition {
    #[must_use]
    pub fn resolve(self, duration: f64, local: Option<f64>) -> f64 {
        match self {
            Self::Resume(saved) => {
                let saved = if saved > 0.0 {
                    saved
                } else {
                    local.unwrap_or(0.0)
                };
                resume_time(saved, duration)
            }
            Self::Seek(target) => media_element_time(target).min(media_element_time(duration)),
        }
    }
}

/// Only reaching the duration counts as complete for legacy saved positions.
#[must_use]
pub fn resume_time(saved: f64, duration: f64) -> f64 {
    if saved.is_finite() && saved > 0.0 && saved < duration {
        saved
    } else {
        0.0
    }
}

/// Ignore uninitialized decoder clocks and seeked events for an older target.
/// A spontaneous zero is a source/decoder reset, not a user seek to the start.
#[must_use]
pub fn stable_media_clock(
    value: f64,
    ready: bool,
    previous: f64,
    pending: Option<f64>,
) -> Option<f64> {
    if !ready || !value.is_finite() || value < 0.0 {
        return None;
    }
    if let Some(target) = pending {
        if (target - value).abs() > 0.5 {
            return None;
        }
    } else if value == 0.0 && previous > 0.5 {
        return None;
    }
    Some(value)
}

/// Native media elements are the clock; invalid values only occur during load.
#[must_use]
pub fn media_element_time(value: f64) -> f64 {
    if value.is_finite() {
        value.max(0.0)
    } else {
        0.0
    }
}

/// The browser's finite duration defines its seek clock. The server can supply
/// the length while native metadata is unavailable (NaN/Infinity/zero).
#[must_use]
pub fn audio_duration(native: f64, backend: f64) -> f64 {
    let native = media_element_time(native);
    if native > 0.0 {
        native
    } else {
        media_element_time(backend)
    }
}

/// Keep invalid/out-of-track chapter starts out of the seek controls.
#[must_use]
pub fn timeline_chapters(chapters: &[AudioChapter], duration: f64) -> Vec<AudioChapter> {
    if !duration.is_finite() || duration <= 0.0 {
        return Vec::new();
    }
    chapters
        .iter()
        .filter(|chapter| {
            chapter.start.is_finite() && chapter.start >= 0.0 && chapter.start < duration
        })
        .cloned()
        .collect()
}

/// Hide the pointer only while playback is unobstructed by a state overlay.
#[must_use]
pub fn should_hide_video_cursor(
    playing: bool,
    controls_visible: bool,
    starting: bool,
    buffering: bool,
    has_error: bool,
) -> bool {
    playing && !controls_visible && !starting && !buffering && !has_error
}

/// A paused teardown event during startup must not overwrite the playback
/// clock, while an ordinary pause after startup should still sync it.
#[must_use]
pub fn should_sync_media_clock(starting: bool, paused: bool) -> bool {
    !starting || !paused
}

/// Keep the requestAnimationFrame sampler alive while its element exists.
#[must_use]
pub fn should_continue_media_clock(element_present: bool) -> bool {
    element_present
}

#[cfg(test)]
mod tests {
    #[test]
    fn decoder_resets_and_old_seek_events_do_not_rewind_progress() {
        use super::stable_media_clock;
        assert_eq!(stable_media_clock(0.0, false, 50.0, None), None);
        assert_eq!(stable_media_clock(f64::NAN, true, 50.0, None), None);
        assert_eq!(stable_media_clock(0.0, true, 50.0, None), None);
        assert_eq!(stable_media_clock(0.0, true, 50.0, Some(50.0)), None);
        assert_eq!(stable_media_clock(0.0, true, 50.0, Some(0.0)), Some(0.0));
        assert_eq!(stable_media_clock(50.1, true, 50.0, Some(50.0)), Some(50.1));
    }
    use super::*;

    fn chapter(start: f64, end: f64) -> AudioChapter {
        AudioChapter {
            id: 1,
            title: String::new(),
            start,
            end,
        }
    }

    #[test]
    fn subtitles_follow_clock_gaps_overlap_and_backwards_seek() {
        let cues = revaro_core::media::parse_webvtt(
            "WEBVTT\n\n00:01.000 --> 00:03.000\nOne\n\n00:02.000 --> 00:04.000\nTwo\n\n00:06.000 --> 00:07.000\nThree\n",
        );
        assert_eq!(subtitle_text(&cues, 0.0), "");
        assert_eq!(subtitle_text(&cues, 2.5), "One\nTwo");
        assert_eq!(subtitle_text(&cues, 3.0), "Two");
        assert_eq!(subtitle_text(&cues, 5.0), "");
        assert_eq!(subtitle_text(&cues, 6.1), "Three");
        assert_eq!(subtitle_text(&cues, 1.5), "One");
        assert_eq!(subtitle_text(&cues, f64::NAN), "");
        assert!(active_subtitle_indices(&cues, 0.0).is_empty());
        assert_eq!(active_subtitle_indices(&cues, 2.5), [0, 1]);
        assert_eq!(active_subtitle_indices(&cues, 3.0), [1]);
        assert!(active_subtitle_indices(&cues, 5.0).is_empty());
        assert_eq!(active_subtitle_indices(&cues, 6.1), [2]);
        assert_eq!(active_subtitle_indices(&cues, 1.5), [0]);
        assert!(active_subtitle_indices(&cues, f64::INFINITY).is_empty());
        assert_eq!(
            active_chapter_index(&[chapter(0.0, 1.0), chapter(2.0, 3.0)], 4.0),
            1
        );
    }

    #[test]
    fn lyric_neighbors_preserve_gaps_overlap_and_seek_direction() {
        let cues = revaro_core::media::parse_webvtt(
            "WEBVTT\n\n00:01.000 --> 00:03.000\nOne\n\n00:02.000 --> 00:04.000\nTwo\n\n00:06.000 --> 00:07.000\nThree\n",
        );
        let context = |previous: &str, current: &str, next: &str| SubtitleContext {
            previous: previous.to_owned(),
            current: current.to_owned(),
            next: next.to_owned(),
        };
        assert_eq!(subtitle_context(&cues, 0.0), context("", "", "One"));
        assert_eq!(
            subtitle_context(&cues, 2.5),
            context("", "One\nTwo", "Three")
        );
        assert_eq!(subtitle_context(&cues, 3.0), context("One", "Two", "Three"));
        assert_eq!(subtitle_context(&cues, 5.0), context("Two", "", "Three"));
        assert_eq!(subtitle_context(&cues, 6.5), context("Two", "Three", ""));
        assert_eq!(subtitle_context(&cues, 1.5), context("", "One", "Two"));
        assert_eq!(subtitle_context(&cues, 8.0), context("Three", "", ""));
        assert_eq!(
            subtitle_context(&cues, f64::NAN),
            SubtitleContext::default()
        );
        assert_eq!(subtitle_context(&[], 5.0), SubtitleContext::default());
    }

    #[test]
    fn clamps_invalid_and_out_of_range_progress() {
        assert_eq!(clamp_percent(f64::NAN), 0.0);
        assert_eq!(clamp_percent(f64::NEG_INFINITY), 0.0);
        assert_eq!(clamp_percent(-1.0), 0.0);
        assert_eq!(clamp_percent(42.5), 42.5);
        assert_eq!(clamp_percent(101.0), 100.0);
    }

    #[test]
    fn audio_duration_uses_one_finite_clock_and_server_fallback() {
        for native in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert_eq!(audio_duration(native, 7200.25), 7200.25);
        }
        assert_eq!(audio_duration(30.125, 30.0), 30.125);
        assert_eq!(audio_duration(30.0, f64::NAN), 30.0);
        assert_eq!(audio_duration(f64::NAN, f64::INFINITY), 0.0);
        assert_eq!(audio_duration(0.0, -1.0), 0.0);
    }

    #[test]
    fn chapter_nodes_require_a_valid_start_inside_the_audio() {
        let chapters = [
            chapter(0.0, 10.0),
            chapter(10.0, 30.0),
            chapter(-1.0, 0.0),
            chapter(f64::NAN, 30.0),
            chapter(30.0, 40.0),
        ];
        assert_eq!(timeline_chapters(&chapters, 30.0).len(), 2);
        assert!(timeline_chapters(&chapters, f64::INFINITY).is_empty());
        assert!(timeline_chapters(&chapters, 0.0).is_empty());
    }

    #[test]
    fn keeps_the_last_chapter_visible_at_duration_boundary() {
        let chapters = [chapter(0.0, 20.0), chapter(20.0, 40.0)];
        assert_eq!(active_chapter_index(&chapters, 0.0), 0);
        assert_eq!(active_chapter_index(&chapters, 20.0), 1);
        assert_eq!(active_chapter_index(&chapters, 40.0), 1);
        assert_eq!(active_chapter_index(&chapters, f64::NAN), 0);
    }

    #[test]
    fn explicit_seeks_ignore_local_resume_and_completion_rules() {
        assert_eq!(PlaybackPosition::Seek(0.0).resolve(90.0, Some(60.0)), 0.0);
        assert_eq!(PlaybackPosition::Seek(88.0).resolve(90.0, None), 88.0);
        assert_eq!(PlaybackPosition::Seek(100.0).resolve(90.0, None), 90.0);
        assert_eq!(PlaybackPosition::Seek(f64::NAN).resolve(90.0, None), 0.0);
        assert_eq!(
            PlaybackPosition::Resume(0.0).resolve(90.0, Some(60.0)),
            60.0
        );
        assert_eq!(PlaybackPosition::Resume(88.0).resolve(90.0, None), 88.0);
    }

    #[test]
    fn resume_positions_restart_completed_media_and_keep_explicit_zero_seeks() {
        assert_eq!(resume_time(84.9, 90.0), 84.9);
        assert_eq!(resume_time(85.0, 90.0), 85.0);
        assert_eq!(resume_time(90.0, 90.0), 0.0);
        assert_eq!(resume_time(f64::NAN, 90.0), 0.0);
        assert_eq!(resume_time(60.0, 0.0), 0.0);
    }

    #[test]
    fn keeps_media_clock_rules_stable() {
        assert_eq!(media_element_time(12.25), 12.25);
        assert_eq!(media_element_time(f64::NAN), 0.0);
        assert!(!should_sync_media_clock(true, true));
        assert!(should_sync_media_clock(true, false));
        assert!(should_sync_media_clock(false, true));
        assert!(should_continue_media_clock(true));
        assert!(!should_continue_media_clock(false));
        assert!(should_hide_video_cursor(true, false, false, false, false));
        assert!(!should_hide_video_cursor(true, true, false, false, false));
    }
}
