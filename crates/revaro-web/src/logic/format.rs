//! Byte, date and media-time formatting.
//!
//! Port of `web/src/format.ts`. The TypeScript originals run in the browser and
//! therefore return strings the whole UI has already been laid out around, so
//! the exact output shapes are part of the contract rather than cosmetic:
//!
//! * [`format_size`] mirrors `formatSize` — one decimal above bytes, e.g.
//!   `1.5 KB`, and a bare integer below them, e.g. `999 B`.
//! * [`format_media_time`] mirrors `formatMediaTime` — `0:00`, `1:05` or
//!   `1:01:01`, with every invalid timeline value normalised to `0:00`.
//! * [`format_date`] mirrors `formatDate`'s zh-CN rendering
//!   (`月`/`日` plus a 24-hour clock), but see the note on it below: it is the
//!   one function here whose exact output depends on browser-local time, which
//!   a target-independent module cannot know.

use revaro_core::Timestamp;

/// Format a byte count the way `formatSize` does.
///
/// Zero is special-cased so the logarithm never runs; everything else picks the
/// largest binary unit that fits (capped at `TB`, matching the original array)
/// and prints one decimal place except for plain bytes.
#[must_use]
pub fn format_size(bytes: u64) -> String {
    if bytes == 0 {
        return "0 B".to_owned();
    }
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    // `log(1024)` rather than `log2(...)/10` to stay literally identical to the
    // TypeScript (`Math.log(bytes)/Math.log(1024)`).
    let index = ((bytes as f64).log(1024.0).floor() as usize).min(UNITS.len() - 1);
    let value = bytes as f64 / 1024f64.powi(index as i32);
    if index == 0 {
        format!("{value} {}", UNITS[index])
    } else {
        format!("{value:.1} {}", UNITS[index])
    }
}

/// Format a timestamp for the file list, e.g. `10月5日 14:30`.
///
/// The TypeScript original delegates to
/// `Intl.DateTimeFormat('zh-CN', { month: 'short', day: 'numeric', hour:
/// '2-digit', minute: '2-digit' })`. The month and day are not zero-padded but
/// the clock is, so this reproduces that shape from the shared
/// [`Timestamp`], which normalises to UTC.
///
/// **Known deviation:** the browser's `Intl` renders in the *local* time zone,
/// while this target-independent function renders UTC. Local-time rendering
/// needs `js-sys`/`Date` and will be added in the wasm-only layer once other
/// features depend on it; keeping the pure function deterministic is what lets
/// it be unit-tested natively. An unparseable value yields `—`, exactly as the
/// original's `Number.isNaN` branch does.
#[must_use]
pub fn format_date(value: &str) -> String {
    let Ok(stamp) = Timestamp::parse(value) else {
        return "—".to_owned();
    };
    // `to_rfc3339` is fixed-width for the fields used here: `YYYY-MM-DDTHH:MM:SS`.
    let rendered = stamp.to_rfc3339();
    let month = rendered.get(5..7).and_then(|part| part.parse::<u32>().ok());
    let day = rendered
        .get(8..10)
        .and_then(|part| part.parse::<u32>().ok());
    let (Some(month), Some(day)) = (month, day) else {
        return "—".to_owned();
    };
    let (Some(hour), Some(minute)) = (rendered.get(11..13), rendered.get(14..16)) else {
        return "—".to_owned();
    };
    format!("{month}月{day}日 {hour}:{minute}")
}

/// Format a playback position or duration the way `formatMediaTime` does.
///
/// Negative, NaN and infinite inputs all collapse to `0:00` rather than
/// producing `NaN:NaN`: media elements report those values while metadata is
/// still loading.
#[must_use]
pub fn format_media_time(seconds: f64) -> String {
    if !seconds.is_finite() || seconds < 0.0 {
        return "0:00".to_owned();
    }
    let value = seconds.floor();
    let hours = (value / 3600.0).floor();
    let minutes = ((value % 3600.0) / 60.0).floor();
    let secs = value % 60.0;
    if hours > 0.0 {
        format!("{}:{:02}:{:02}", hours as u64, minutes as u64, secs as u64)
    } else {
        format!("{}:{:02}", minutes as u64, secs as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uses_one_duration_format_for_audio_and_video_controls() {
        // Ported verbatim from `web/src/format.test.ts`.
        assert_eq!(format_media_time(0.0), "0:00");
        assert_eq!(format_media_time(65.9), "1:05");
        assert_eq!(format_media_time(3661.0), "1:01:01");
    }

    #[test]
    fn normalizes_invalid_timeline_values() {
        // Ported verbatim from `web/src/format.test.ts`.
        assert_eq!(format_media_time(-1.0), "0:00");
        assert_eq!(format_media_time(f64::NAN), "0:00");
        assert_eq!(format_media_time(f64::INFINITY), "0:00");
    }

    #[test]
    fn picks_the_largest_binary_unit() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(1), "1 B");
        assert_eq!(format_size(999), "999 B");
        assert_eq!(format_size(1024), "1.0 KB");
        assert_eq!(format_size(1536), "1.5 KB");
        assert_eq!(format_size(1024 * 1024), "1.0 MB");
        assert_eq!(format_size(1024 * 1024 * 1024), "1.0 GB");
        assert_eq!(format_size(1024_u64.pow(4)), "1.0 TB");
    }

    #[test]
    fn caps_the_unit_at_terabytes() {
        // `formatSize` indexes a five-element array, so petabytes stay in TB.
        assert_eq!(format_size(1024_u64.pow(5)), "1024.0 TB");
    }

    #[test]
    fn renders_zh_cn_dates_from_the_shared_timestamp() {
        assert_eq!(format_date("2024-10-05T14:30:00Z"), "10月5日 14:30");
        assert_eq!(format_date("2024-01-09T00:05:00Z"), "1月9日 00:05");
        // Non-RFC3339 shapes the shared `Timestamp` accepts (SQLite timestamps).
        assert_eq!(format_date("2024-05-06 07:08:09"), "5月6日 07:08");
        // Offsets normalise to UTC, matching how the API stores instants.
        assert_eq!(format_date("2024-05-06T09:08:09+02:00"), "5月6日 07:08");
    }

    #[test]
    fn invalid_dates_render_the_em_dash() {
        assert_eq!(format_date("nothing"), "—");
        assert_eq!(format_date(""), "—");
    }
}
