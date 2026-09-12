//! Target-independent media-player rules.
//!
//! The DOM components are responsible for event wiring and playback APIs. The
//! rules that decide which chapter is active, how a subtitle is placed inside
//! a letterboxed video, and how a seek competes with saved progress stay here
//! so native tests can exercise the edge cases without a browser.

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

/// Calculate the inset around a contained video image.
///
/// `bottom` is the vertical letterbox on a landscape video and `horizontal`
/// is the horizontal letterbox on a portrait video. Invalid measurements are
/// deliberately neutral because browsers report zero dimensions before media
/// metadata has loaded.
#[must_use]
pub fn contained_video_insets(
    container_width: f64,
    container_height: f64,
    video_width: f64,
    video_height: f64,
) -> (f64, f64) {
    if container_width <= 0.0
        || container_height <= 0.0
        || video_width <= 0.0
        || video_height <= 0.0
    {
        return (0.0, 0.0);
    }
    let container_ratio = container_width / container_height;
    let video_ratio = video_width / video_height;
    if video_ratio > container_ratio {
        (
            (container_height - container_width / video_ratio) / 2.0,
            0.0,
        )
    } else {
        (
            0.0,
            (container_width - container_height * video_ratio) / 2.0,
        )
    }
}

/// Select the server's preferred subtitle track.
#[must_use]
pub fn initial_subtitle_index(defaults: &[(bool, bool)]) -> Option<usize> {
    defaults
        .iter()
        .position(|(is_default, _)| *is_default)
        .or_else(|| defaults.iter().position(|(_, forced)| *forced))
        .or_else(|| (!defaults.is_empty()).then_some(0))
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

/// The first subtitle line receives the normal size; later lines are smaller.
#[must_use]
pub fn subtitle_line_is_secondary(index: usize) -> bool {
    index > 0
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
    fn computes_contain_letterboxes() {
        assert_eq!(
            contained_video_insets(1000.0, 1000.0, 1920.0, 1080.0),
            (218.75, 0.0)
        );
        assert_eq!(
            contained_video_insets(1200.0, 600.0, 1080.0, 1920.0),
            (0.0, 431.25)
        );
        assert_eq!(
            contained_video_insets(0.0, 600.0, 1920.0, 1080.0),
            (0.0, 0.0)
        );
    }

    #[test]
    fn honours_default_then_forced_then_first_subtitle() {
        assert_eq!(
            initial_subtitle_index(&[(false, false), (true, false)]),
            Some(1)
        );
        assert_eq!(
            initial_subtitle_index(&[(false, false), (false, true)]),
            Some(1)
        );
        assert_eq!(
            initial_subtitle_index(&[(false, false), (false, false)]),
            Some(0)
        );
        assert_eq!(initial_subtitle_index(&[]), None);
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
        assert!(subtitle_line_is_secondary(1));
        assert!(!subtitle_line_is_secondary(0));
    }
}
