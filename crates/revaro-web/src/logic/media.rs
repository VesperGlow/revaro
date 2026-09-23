//! Target-independent media-player rules.
//!
//! The DOM components are responsible for event wiring and playback APIs. The
//! rules that decide which chapter is active and how a seek competes with saved
//! progress stay here so native tests can exercise edge cases without a browser.

use revaro_core::media::AudioChapter;

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
        .enumerate()
        .find(|(index, chapter)| {
            time >= chapter.start && (time < chapter.end || *index == chapters.len() - 1)
        })
        .map_or(0, |(index, _)| index)
}

/// Resolve the first seek that wins over a stored resume position.
#[must_use]
pub fn authoritative_seek_target(current: f64, saved: f64, user_seeked: bool) -> f64 {
    if user_seeked && current.is_finite() {
        return current.max(0.0);
    }
    if current.is_finite() && current > 0.0 {
        current
    } else if saved.is_finite() {
        saved.max(0.0)
    } else {
        0.0
    }
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
    fn clamps_invalid_and_out_of_range_progress() {
        assert_eq!(clamp_percent(f64::NAN), 0.0);
        assert_eq!(clamp_percent(f64::NEG_INFINITY), 0.0);
        assert_eq!(clamp_percent(-1.0), 0.0);
        assert_eq!(clamp_percent(42.5), 42.5);
        assert_eq!(clamp_percent(101.0), 100.0);
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
    fn explicit_zero_seek_beats_saved_resume_position() {
        assert_eq!(authoritative_seek_target(0.0, 86.0, true), 0.0);
        assert_eq!(authoritative_seek_target(0.0, 86.0, false), 86.0);
        assert_eq!(authoritative_seek_target(80.0, 86.0, false), 80.0);
        assert_eq!(authoritative_seek_target(0.0, f64::NAN, false), 0.0);
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
