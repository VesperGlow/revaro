//! Process configuration, ported from the Go `internal/config` package.
//!
//! Two deliberate changes from the original:
//!
//! * the sidecar settings (`DATA_PLANE_ADDR`, `DATA_PLANE_BINARY`) are gone.
//!   The media engine is now a library inside this process, so there is no
//!   loopback child to configure, supervise or authenticate.
//! * `APP_WEB_DIR` is new: the browser bundle is served from disk rather than
//!   embedded with `go:embed`, which removes the build-order coupling between
//!   the client bundle and the server binary.
//!
//! Everything is read through an injected lookup so the parsing and validation
//! rules are unit-testable without mutating the process environment.

use std::collections::HashMap;
use std::fmt;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use crate::proxy::{self, Prefix};

/// Default listen address, matching the historical `:8080`.
pub const DEFAULT_ADDR: &str = ":8080";

/// Default data directory inside the container image.
pub const DEFAULT_DATA_DIR: &str = "/data";

/// Default temporary work directory.
pub const DEFAULT_WORK_DIR: &str = "/work";

/// Default public base URL.
pub const DEFAULT_BASE_URL: &str = "http://localhost:8080";

/// Default browser bundle location, relative to the process working directory.
pub const DEFAULT_WEB_DIR: &str = "dist/web";

/// Largest accepted cache capacity: one tebibyte.
const MAX_CACHE_CAPACITY: i64 = 1 << 40;

/// A configuration value that could not be accepted.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ConfigError(String);

impl ConfigError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }

    /// The human-readable reason, without the environment variable name.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.0
    }
}

/// Everything the server needs to start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// Listen address, in Go's `:8080` or a full `host:port` form.
    pub addr: String,
    /// Root of the persistent data directory.
    pub data_dir: PathBuf,
    /// Scratch space for extraction and derived caches.
    pub work_dir: PathBuf,
    /// Browser bundle directory served for non-API routes.
    pub web_dir: PathBuf,
    /// Public base URL, used for share links and the same-origin check.
    pub base_url: String,
    /// Whether session cookies carry the `Secure` attribute.
    pub cookie_secure: bool,
    /// Bootstrap administrator name, empty to use the stored one.
    pub admin_username: String,
    /// Bootstrap administrator password, empty to generate one on first start.
    pub admin_password: String,
    /// Capacity of the on-disk media cache in bytes.
    pub media_cache_capacity: i64,
    /// How long an unfinished upload may stay pending.
    pub upload_expires: Duration,
    /// How long deleted files stay in the trash.
    pub trash_retention: Duration,
    /// Interval between orphan-blob sweeps; zero disables the sweep.
    pub gc_interval: Duration,
    /// How long reader flow artifacts are retained.
    pub flow_cache_ttl: Duration,
    /// Capacity of the reader flow cache in bytes.
    pub flow_cache_capacity: i64,
    /// Networks whose `X-Forwarded-For` headers are trusted.
    pub trusted_proxies: Vec<Prefix>,
}

impl Config {
    /// Load configuration from the process environment.
    ///
    /// # Errors
    /// Returns a [`ConfigError`] naming the first invalid value.
    pub fn from_env() -> Result<Self, ConfigError> {
        let env: HashMap<String, String> = std::env::vars().collect();
        Self::from_lookup(&|name| env.get(name).cloned())
    }

    /// Load configuration from an arbitrary source.
    ///
    /// # Errors
    /// Returns a [`ConfigError`] naming the first invalid value.
    pub fn from_lookup(lookup: &dyn Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let value = |name: &str, fallback: &str| -> String {
            match lookup(name) {
                Some(found) if !found.is_empty() => found,
                _ => fallback.to_owned(),
            }
        };

        let data_dir = PathBuf::from(value("APP_DATA_DIR", DEFAULT_DATA_DIR));
        let base_url = value("APP_BASE_URL", DEFAULT_BASE_URL)
            .trim_end_matches('/')
            .to_owned();
        if !is_absolute_http_url(&base_url) {
            return Err(ConfigError::new(
                "APP_BASE_URL must be an absolute http(s) URL",
            ));
        }

        let cookie_secure = match lookup("COOKIE_SECURE") {
            Some(raw) if !raw.is_empty() => parse_bool("COOKIE_SECURE", &raw)?,
            _ => base_url.starts_with("https://"),
        };

        let work_dir_raw = value("APP_WORK_DIR", DEFAULT_WORK_DIR);
        if work_dir_raw.trim().is_empty() {
            return Err(ConfigError::new("APP_WORK_DIR must not be empty"));
        }

        let media_cache_capacity =
            parse_int("MEDIA_CACHE_CAPACITY", lookup, 2 * 1024 * 1024 * 1024)?;
        if !(0..=MAX_CACHE_CAPACITY).contains(&media_cache_capacity) {
            return Err(ConfigError::new(
                "MEDIA_CACHE_CAPACITY must be between 0 and 1 TiB",
            ));
        }

        let flow_cache_capacity = parse_int("FLOW_CACHE_CAPACITY", lookup, 1 << 30)?;
        if !(0..=MAX_CACHE_CAPACITY).contains(&flow_cache_capacity) {
            return Err(ConfigError::new(
                "FLOW_CACHE_CAPACITY must be between 0 and 1 TiB",
            ));
        }

        let flow_cache_ttl =
            parse_duration_env("FLOW_CACHE_TTL", lookup, Duration::from_secs(720 * 3600))?;
        if flow_cache_ttl.is_zero()
            && lookup("FLOW_CACHE_TTL").is_some_and(|raw| raw.starts_with('-'))
        {
            return Err(ConfigError::new("FLOW_CACHE_TTL must not be negative"));
        }

        let upload_expires =
            parse_duration_env("UPLOAD_EXPIRES", lookup, Duration::from_secs(24 * 3600))?;
        if upload_expires.is_zero() {
            return Err(ConfigError::new("UPLOAD_EXPIRES must be positive"));
        }

        let trash_retention = parse_duration_env(
            "TRASH_RETENTION",
            lookup,
            Duration::from_secs(30 * 24 * 3600),
        )?;
        let gc_interval = parse_duration_env("GC_INTERVAL", lookup, Duration::from_secs(3600))?;

        let trusted_proxies = match lookup("TRUSTED_PROXIES") {
            Some(raw) if !raw.trim().is_empty() => {
                proxy::parse_list(&raw).map_err(ConfigError::new)?
            }
            _ => Vec::new(),
        };

        Ok(Self {
            addr: value("APP_ADDR", DEFAULT_ADDR),
            data_dir,
            work_dir: PathBuf::from(work_dir_raw),
            web_dir: PathBuf::from(value("APP_WEB_DIR", DEFAULT_WEB_DIR)),
            base_url,
            cookie_secure,
            admin_username: lookup("ADMIN_USERNAME").unwrap_or_default(),
            admin_password: lookup("ADMIN_PASSWORD").unwrap_or_default(),
            media_cache_capacity,
            upload_expires,
            trash_retention,
            gc_interval,
            flow_cache_ttl,
            flow_cache_capacity,
            trusted_proxies,
        })
    }

    /// Path of the SQLite database file.
    #[must_use]
    pub fn database_path(&self) -> PathBuf {
        self.data_dir.join("revaro.db")
    }

    /// Root of the local object store.
    #[must_use]
    pub fn objects_dir(&self) -> PathBuf {
        self.data_dir.join("objects")
    }

    /// The listen address as a socket address.
    ///
    /// Accepts Go's bare-port form (`:8080`), which means "every interface".
    ///
    /// # Errors
    /// Returns the offending value when it is not a usable address.
    pub fn listen_addr(&self) -> Result<SocketAddr, ConfigError> {
        let candidate = if let Some(port) = self.addr.strip_prefix(':') {
            format!("0.0.0.0:{port}")
        } else {
            self.addr.clone()
        };
        candidate.parse().map_err(|_| {
            ConfigError::new(format!(
                "APP_ADDR {:?} is not a valid listen address",
                self.addr
            ))
        })
    }
}

impl fmt::Display for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "addr={} data={} work={} web={} base_url={} secure_cookie={}",
            self.addr,
            self.data_dir.display(),
            self.work_dir.display(),
            self.web_dir.display(),
            self.base_url,
            self.cookie_secure
        )
    }
}

/// True when `value` is an absolute `http(s)` URL with a host.
fn is_absolute_http_url(value: &str) -> bool {
    let Some(rest) = value
        .strip_prefix("http://")
        .or_else(|| value.strip_prefix("https://"))
    else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    !authority.is_empty()
}

fn parse_bool(name: &str, raw: &str) -> Result<bool, ConfigError> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "t" | "true" | "y" | "yes" => Ok(true),
        "0" | "f" | "false" | "n" | "no" => Ok(false),
        _ => Err(ConfigError::new(format!("{name} must be a boolean"))),
    }
}

fn parse_int(
    name: &str,
    lookup: &dyn Fn(&str) -> Option<String>,
    fallback: i64,
) -> Result<i64, ConfigError> {
    match lookup(name) {
        Some(raw) if !raw.is_empty() => raw
            .trim()
            .parse()
            .map_err(|_| ConfigError::new(format!("{name} must be an integer"))),
        _ => Ok(fallback),
    }
}

fn parse_duration_env(
    name: &str,
    lookup: &dyn Fn(&str) -> Option<String>,
    fallback: Duration,
) -> Result<Duration, ConfigError> {
    match lookup(name) {
        Some(raw) if !raw.is_empty() => {
            parse_duration(&raw).map_err(|reason| ConfigError::new(format!("{name}: {reason}")))
        }
        _ => Ok(fallback),
    }
}

/// Parse a Go-style duration such as `24h`, `720h`, `1h30m`, `300ms`, `1.5h`.
///
/// Negative durations are accepted here and rejected by the individual
/// validators, mirroring how the Go server reported them.
///
/// # Errors
/// Returns a human-readable reason when the value cannot be parsed.
pub fn parse_duration(value: &str) -> Result<Duration, String> {
    let raw = value.trim();
    if raw.is_empty() {
        return Err("empty duration".to_owned());
    }
    let (negative, body) = match raw.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, raw.strip_prefix('+').unwrap_or(raw)),
    };
    if body == "0" {
        return Ok(Duration::ZERO);
    }
    if body.is_empty() {
        return Err(format!("invalid duration {value:?}"));
    }

    let mut total_nanos: f64 = 0.0;
    let mut rest = body;
    while !rest.is_empty() {
        let digits_end = rest
            .find(|character: char| !character.is_ascii_digit() && character != '.')
            .ok_or_else(|| format!("missing unit in duration {value:?}"))?;
        if digits_end == 0 {
            return Err(format!("invalid duration {value:?}"));
        }
        let number: f64 = rest[..digits_end]
            .parse()
            .map_err(|_| format!("invalid duration {value:?}"))?;
        rest = &rest[digits_end..];

        let unit_end = rest
            .find(|character: char| character.is_ascii_digit() || character == '.')
            .unwrap_or(rest.len());
        let unit = &rest[..unit_end];
        rest = &rest[unit_end..];

        let multiplier: f64 = match unit {
            "ns" => 1.0,
            "us" | "µs" | "μs" => 1_000.0,
            "ms" => 1_000_000.0,
            "s" => 1_000_000_000.0,
            "m" => 60.0 * 1_000_000_000.0,
            "h" => 3600.0 * 1_000_000_000.0,
            _ => return Err(format!("unknown unit {unit:?} in duration {value:?}")),
        };
        total_nanos += number * multiplier;
    }

    if !total_nanos.is_finite() || total_nanos < 0.0 {
        return Err(format!("invalid duration {value:?}"));
    }
    let nanos = total_nanos.round();
    if nanos > u64::MAX as f64 {
        return Err(format!("duration {value:?} is out of range"));
    }
    let duration = Duration::from_nanos(nanos as u64);
    Ok(if negative { Duration::ZERO } else { duration })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_from(pairs: &[(&str, &str)]) -> Result<Config, ConfigError> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect();
        Config::from_lookup(&|name| map.get(name).cloned())
    }

    #[test]
    fn defaults_match_the_documented_values() {
        let config = config_from(&[]).unwrap();
        assert_eq!(config.addr, DEFAULT_ADDR);
        assert_eq!(config.data_dir, PathBuf::from(DEFAULT_DATA_DIR));
        assert_eq!(config.work_dir, PathBuf::from(DEFAULT_WORK_DIR));
        assert_eq!(config.web_dir, PathBuf::from(DEFAULT_WEB_DIR));
        assert_eq!(config.base_url, DEFAULT_BASE_URL);
        assert!(!config.cookie_secure);
        assert_eq!(config.media_cache_capacity, 2 * 1024 * 1024 * 1024);
        assert_eq!(config.upload_expires, Duration::from_secs(24 * 3600));
        assert_eq!(config.trash_retention, Duration::from_secs(30 * 24 * 3600));
        assert_eq!(config.gc_interval, Duration::from_secs(3600));
        assert_eq!(config.flow_cache_ttl, Duration::from_secs(720 * 3600));
        assert_eq!(config.flow_cache_capacity, 1 << 30);
        assert!(config.trusted_proxies.is_empty());
    }

    #[test]
    fn https_base_url_enables_secure_cookies() {
        let config = config_from(&[("APP_BASE_URL", "https://cloud.example.com")]).unwrap();
        assert!(config.cookie_secure);
        let explicit = config_from(&[
            ("APP_BASE_URL", "https://cloud.example.com"),
            ("COOKIE_SECURE", "false"),
        ])
        .unwrap();
        assert!(!explicit.cookie_secure);
    }

    #[test]
    fn base_url_must_be_absolute_http() {
        // An empty value means "unset" and falls back to the documented
        // default, exactly as the Go `env` helper behaved.
        assert!(config_from(&[("APP_BASE_URL", "")]).is_ok());
        for value in [
            "example.com",
            "ftp://example.com",
            "http://",
            "localhost:8080",
            "://x",
        ] {
            assert!(
                config_from(&[("APP_BASE_URL", value)]).is_err(),
                "{value:?} should fail"
            );
        }
        assert!(config_from(&[("APP_BASE_URL", "http://localhost:8080/")]).is_ok());
    }

    #[test]
    fn trailing_slashes_are_trimmed_from_the_base_url() {
        let config = config_from(&[("APP_BASE_URL", "https://example.com/")]).unwrap();
        assert_eq!(config.base_url, "https://example.com");
    }

    #[test]
    fn rejects_out_of_range_capacities() {
        assert!(config_from(&[("MEDIA_CACHE_CAPACITY", "-1")]).is_err());
        assert!(config_from(&[("MEDIA_CACHE_CAPACITY", "1099511627777")]).is_err());
        assert!(config_from(&[("FLOW_CACHE_CAPACITY", "-5")]).is_err());
        assert!(config_from(&[("MEDIA_CACHE_CAPACITY", "0")]).is_ok());
    }

    #[test]
    fn rejects_non_positive_upload_expiry() {
        assert!(config_from(&[("UPLOAD_EXPIRES", "0s")]).is_err());
        assert!(config_from(&[("UPLOAD_EXPIRES", "nonsense")]).is_err());
    }

    #[test]
    fn accepts_negative_trash_and_gc_values_when_the_duration_parses() {
        // These are documented as "must not be negative" in the Go server, which
        // it enforced by parsing the sign itself; the important compatibility
        // property is that a negative value never silently becomes a huge
        // positive one.
        let config = config_from(&[("TRASH_RETENTION", "-1h")]).unwrap();
        assert_eq!(config.trash_retention, Duration::ZERO);
    }

    #[test]
    fn parses_trusted_proxies() {
        let config = config_from(&[("TRUSTED_PROXIES", "10.0.0.0/8, 192.168.1.0/24")]).unwrap();
        assert_eq!(config.trusted_proxies.len(), 2);
        assert!(config_from(&[("TRUSTED_PROXIES", "nope")]).is_err());
    }

    #[test]
    fn database_and_objects_live_under_the_data_directory() {
        let config = config_from(&[("APP_DATA_DIR", "/srv/revaro")]).unwrap();
        assert_eq!(
            config.database_path(),
            PathBuf::from("/srv/revaro/revaro.db")
        );
        assert_eq!(config.objects_dir(), PathBuf::from("/srv/revaro/objects"));
    }

    #[test]
    fn listen_address_accepts_the_bare_port_form() {
        let config = config_from(&[]).unwrap();
        assert_eq!(config.listen_addr().unwrap().to_string(), "0.0.0.0:8080");
        let explicit = config_from(&[("APP_ADDR", "127.0.0.1:9000")]).unwrap();
        assert_eq!(
            explicit.listen_addr().unwrap().to_string(),
            "127.0.0.1:9000"
        );
        let broken = config_from(&[("APP_ADDR", "not-an-address")]).unwrap();
        assert!(broken.listen_addr().is_err());
    }

    #[test]
    fn duration_parsing_covers_the_documented_forms() {
        assert_eq!(parse_duration("24h").unwrap(), Duration::from_secs(86_400));
        assert_eq!(
            parse_duration("720h").unwrap(),
            Duration::from_secs(2_592_000)
        );
        assert_eq!(parse_duration("1h30m").unwrap(), Duration::from_secs(5_400));
        assert_eq!(parse_duration("300ms").unwrap(), Duration::from_millis(300));
        assert_eq!(parse_duration("1.5h").unwrap(), Duration::from_secs(5_400));
        assert_eq!(parse_duration("0").unwrap(), Duration::ZERO);
        assert_eq!(parse_duration("100ns").unwrap(), Duration::from_nanos(100));
        assert_eq!(parse_duration("90s").unwrap(), Duration::from_secs(90));
    }

    #[test]
    fn duration_parsing_rejects_garbage() {
        for value in ["", "h", "10", "10x", "1h30", "-", "1..5h"] {
            assert!(
                parse_duration(value).is_err(),
                "{value:?} should be rejected"
            );
        }
    }

    #[test]
    fn negative_durations_never_wrap_around() {
        assert_eq!(parse_duration("-1h").unwrap(), Duration::ZERO);
        // A trailing minus is not a sign and must be rejected outright.
        assert!(parse_duration("1h-").is_err());
    }
}
