//! Timestamps with one shared on-disk and on-the-wire representation.
//!
//! The existing SQLite database stores timestamps as `TEXT`, and the API sends
//! them as JSON strings. Historically those strings were produced with Go's
//! `time.RFC3339Nano`, which is RFC 3339 in UTC with trailing zeros removed
//! from the fractional second. Comparisons in SQL are *lexicographic*
//! (`expires_at <= ?`), so the representation has to keep ordering monotonic.
//!
//! [`Timestamp`] therefore serializes exactly like `time.RFC3339Nano` and
//! parses a superset of that format, including the `YYYY-MM-DD HH:MM:SS` shape
//! SQLite's `CURRENT_TIMESTAMP` produces (used by the initial root row).

use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, NaiveDateTime, SecondsFormat, TimeDelta, Utc};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A UTC instant with RFC 3339 string representation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Timestamp(DateTime<Utc>);

/// Failure modes of [`Timestamp::parse`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ParseTimestampError {
    /// The input was not a recognizable timestamp.
    #[error("invalid timestamp: {0:?}")]
    Invalid(String),
}

impl Timestamp {
    /// The current instant.
    #[must_use]
    pub fn now() -> Self {
        Self(Utc::now())
    }

    /// Wrap an existing `chrono` value.
    #[must_use]
    pub const fn from_datetime(value: DateTime<Utc>) -> Self {
        Self(value)
    }

    /// The Unix epoch.
    #[must_use]
    pub fn epoch() -> Self {
        Self(DateTime::<Utc>::from_timestamp(0, 0).expect("epoch is a valid instant"))
    }

    /// Sentinel used when a legacy file response omits a timestamp entirely.
    ///
    /// The old browser renders an omitted date as `—`, while an explicit JSON
    /// `null` is coerced by JavaScript to the Unix epoch. Keeping a dedicated
    /// value lets the response decoder preserve that distinction without
    /// making persisted timestamps optional.
    #[must_use]
    pub fn missing() -> Self {
        Self(DateTime::<Utc>::MIN_UTC)
    }

    /// Whether this value is the legacy-response missing-date sentinel.
    #[must_use]
    pub fn is_missing(self) -> bool {
        self.0 == DateTime::<Utc>::MIN_UTC
    }

    /// Build from milliseconds since the Unix epoch, saturating at the
    /// representable range.
    #[must_use]
    pub fn from_unix_millis(millis: i64) -> Self {
        match DateTime::<Utc>::from_timestamp_millis(millis) {
            Some(value) => Self(value),
            None if millis < 0 => Self(DateTime::<Utc>::MIN_UTC),
            None => Self(DateTime::<Utc>::MAX_UTC),
        }
    }

    /// Convert a filesystem or wall-clock timestamp.
    ///
    /// Used wherever the object store reports a modification time, so the
    /// server and the reader describe the same instant identically.
    #[must_use]
    pub fn from_system_time(value: std::time::SystemTime) -> Self {
        if let Ok(delta) = value.duration_since(std::time::UNIX_EPOCH) {
            Self(DateTime::<Utc>::from(std::time::UNIX_EPOCH + delta))
        } else if let Ok(delta) = std::time::UNIX_EPOCH.duration_since(value) {
            Self(DateTime::<Utc>::from(std::time::UNIX_EPOCH - delta))
        } else {
            // Only reachable on platforms that cannot represent the difference.
            Self::epoch()
        }
    }

    /// Milliseconds since the Unix epoch.
    #[must_use]
    pub fn unix_millis(&self) -> i64 {
        self.0.timestamp_millis()
    }

    /// Seconds since the Unix epoch.
    #[must_use]
    pub fn unix_seconds(&self) -> i64 {
        self.0.timestamp()
    }

    /// The underlying `chrono` value.
    #[must_use]
    pub const fn as_datetime(&self) -> DateTime<Utc> {
        self.0
    }

    /// Shift by a signed duration, saturating on overflow.
    #[must_use]
    pub fn saturating_add(&self, delta: TimeDelta) -> Self {
        Self(
            self.0
                .checked_add_signed(delta)
                .unwrap_or(DateTime::<Utc>::MAX_UTC),
        )
    }

    /// Shift backwards by a signed duration, saturating on overflow.
    #[must_use]
    pub fn saturating_sub(&self, delta: TimeDelta) -> Self {
        Self(
            self.0
                .checked_sub_signed(delta)
                .unwrap_or(DateTime::<Utc>::MIN_UTC),
        )
    }

    /// True when `self` is at or before `deadline`, i.e. a stored `expires_at`
    /// has been reached.
    #[must_use]
    pub fn is_expired_at(&self, now: Timestamp) -> bool {
        self.0 <= now.0
    }

    /// Format like Go's `time.RFC3339Nano`.
    ///
    /// Examples: `2024-05-06T07:08:09Z`, `2024-05-06T07:08:09.5Z`,
    /// `2024-05-06T07:08:09.123456Z`.
    #[must_use]
    pub fn to_rfc3339(&self) -> String {
        let nanos = self.0.timestamp_subsec_nanos();
        if nanos == 0 {
            // SecondsFormat::Secs is not used directly because it would render
            // the offset as `+00:00`; we always want the `Z` suffix.
            return self.0.format("%Y-%m-%dT%H:%M:%SZ").to_string();
        }
        let base = self.0.format("%Y-%m-%dT%H:%M:%S").to_string();
        let fraction = format!("{nanos:09}");
        let trimmed = fraction.trim_end_matches('0');
        format!("{base}.{trimmed}Z")
    }

    /// Round-trippable RFC 3339 with a fixed nanosecond fraction. Useful when a
    /// caller needs a stable, fixed-width form (for example a cache key).
    #[must_use]
    pub fn to_rfc3339_fixed(&self) -> String {
        self.0.to_rfc3339_opts(SecondsFormat::Nanos, true)
    }

    /// Parse the representations this project can emit or encounter in SQLite.
    #[must_use = "the parse result must be handled"]
    pub fn parse(value: &str) -> Result<Self, ParseTimestampError> {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return Err(ParseTimestampError::Invalid(value.to_owned()));
        }
        if let Ok(parsed) = DateTime::parse_from_rfc3339(trimmed) {
            return Ok(Self(parsed.with_timezone(&Utc)));
        }
        // SQLite `CURRENT_TIMESTAMP` and `strftime('%Y-%m-%dT%H:%M:%fZ','now')`
        // both appear in the existing database, in a handful of shapes.
        for format in [
            "%Y-%m-%d %H:%M:%S%.f",
            "%Y-%m-%dT%H:%M:%S%.f",
            "%Y-%m-%d %H:%M",
        ] {
            if let Ok(naive) = NaiveDateTime::parse_from_str(trimmed, format) {
                return Ok(Self(naive.and_utc()));
            }
        }
        Err(ParseTimestampError::Invalid(value.to_owned()))
    }
}

impl Default for Timestamp {
    fn default() -> Self {
        Self::epoch()
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_rfc3339())
    }
}

impl From<DateTime<Utc>> for Timestamp {
    fn from(value: DateTime<Utc>) -> Self {
        Self(value)
    }
}

impl From<Timestamp> for DateTime<Utc> {
    fn from(value: Timestamp) -> Self {
        value.0
    }
}

impl FromStr for Timestamp {
    type Err = ParseTimestampError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

impl Serialize for Timestamp {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_rfc3339())
    }
}

impl<'de> Deserialize<'de> for Timestamp {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::parse(&raw).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_like_go_rfc3339_nano() {
        let cases: &[(i64, u32, &str)] = &[
            (1_714_982_889, 0, "2024-05-06T08:08:09Z"),
            (1_714_982_889, 500_000_000, "2024-05-06T08:08:09.5Z"),
            (1_714_982_889, 123_456_000, "2024-05-06T08:08:09.123456Z"),
            (1_714_982_889, 1, "2024-05-06T08:08:09.000000001Z"),
        ];
        for (seconds, nanos, expected) in cases {
            let value = DateTime::<Utc>::from_timestamp(*seconds, *nanos).unwrap();
            assert_eq!(Timestamp::from_datetime(value).to_rfc3339(), *expected);
        }
    }

    #[test]
    fn round_trips_every_fraction_width() {
        for nanos in [0u32, 5_000_000, 123_456_000, 1, 999_999_999] {
            let value = DateTime::<Utc>::from_timestamp(1_714_982_889, nanos).unwrap();
            let stamp = Timestamp::from_datetime(value);
            assert_eq!(Timestamp::parse(&stamp.to_rfc3339()).unwrap(), stamp);
        }
    }

    #[test]
    fn parses_sqlite_current_timestamp() {
        let parsed = Timestamp::parse("2024-05-06 07:08:09").unwrap();
        assert_eq!(parsed.to_rfc3339(), "2024-05-06T07:08:09Z");
    }

    #[test]
    fn parses_offsets_by_normalizing_to_utc() {
        let parsed = Timestamp::parse("2024-05-06T09:08:09+02:00").unwrap();
        assert_eq!(parsed.to_rfc3339(), "2024-05-06T07:08:09Z");
    }

    #[test]
    fn rejects_garbage() {
        assert!(Timestamp::parse("not a time").is_err());
        assert!(Timestamp::parse("").is_err());
    }

    #[test]
    fn lexicographic_order_matches_chronological_order_for_generated_values() {
        // SQL compares these strings directly (`expires_at <= ?`), so the
        // textual order has to agree with the temporal order. Every timestamp
        // this product writes comes from a nanosecond clock, so it carries a
        // fractional part and the two orders coincide.
        let earlier = Timestamp::parse("2024-05-06T07:08:09.1Z").unwrap();
        let later = Timestamp::parse("2024-05-06T07:08:09.5Z").unwrap();
        let next_second = Timestamp::parse("2024-05-06T07:08:10Z").unwrap();
        assert!(earlier.to_rfc3339() < later.to_rfc3339());
        assert!(later.to_rfc3339() < next_second.to_rfc3339());
        assert!(earlier < later && later < next_second);
    }

    #[test]
    fn documents_the_whole_second_ordering_caveat() {
        // Inherited from the historical RFC3339Nano encoding: within the same
        // second, a whole-second value sorts *after* a fractional one as text,
        // because `Z` (0x5A) is greater than `.` (0x2E).
        //
        // This is left as-is on purpose. Emitting a fixed nine-digit fraction
        // would fix the ordering but break comparisons against rows already in
        // the database, which were written in the trimmed form. In practice a
        // zero fraction essentially cannot occur, because timestamps come from
        // `Timestamp::now()`.
        let whole = Timestamp::parse("2024-05-06T07:08:09Z").unwrap();
        let fraction = Timestamp::parse("2024-05-06T07:08:09.5Z").unwrap();
        // Chronological order is correct...
        assert!(whole < fraction);
        // ...but the text disagrees, which is exactly the caveat.
        assert!(whole.to_rfc3339() > fraction.to_rfc3339());
    }

    #[test]
    fn converts_system_time_on_both_sides_of_the_epoch() {
        use std::time::{Duration, UNIX_EPOCH};

        let after = UNIX_EPOCH + Duration::new(1_714_982_889, 500_000_000);
        assert_eq!(
            Timestamp::from_system_time(after).to_rfc3339(),
            "2024-05-06T08:08:09.5Z"
        );

        let before = UNIX_EPOCH - Duration::new(1, 500_000_000);
        let converted = Timestamp::from_system_time(before);
        // 1.5 seconds before the epoch, expressed without losing the fraction.
        assert_eq!(converted.unix_millis(), -1500);
        assert_eq!(
            Timestamp::from_system_time(UNIX_EPOCH).to_rfc3339(),
            "1970-01-01T00:00:00Z"
        );
    }

    #[test]
    fn serializes_as_a_json_string() {
        let stamp = Timestamp::parse("2024-05-06T07:08:09Z").unwrap();
        assert_eq!(
            serde_json::to_string(&stamp).unwrap(),
            "\"2024-05-06T07:08:09Z\""
        );
        let back: Timestamp = serde_json::from_str("\"2024-05-06T07:08:09Z\"").unwrap();
        assert_eq!(back, stamp);
    }

    #[test]
    fn expiry_uses_an_inclusive_deadline() {
        let deadline = Timestamp::parse("2024-05-06T07:08:09Z").unwrap();
        assert!(deadline.is_expired_at(deadline));
        assert!(deadline.is_expired_at(deadline.saturating_add(TimeDelta::seconds(1))));
        assert!(!deadline.is_expired_at(deadline.saturating_sub(TimeDelta::seconds(1))));
    }
}
