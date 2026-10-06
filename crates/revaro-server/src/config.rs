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

/// Default local object storage directory.
pub const DEFAULT_OBJECTS_DIR: &str = "/objects";

/// Default rebuildable cache directory.
pub const DEFAULT_CACHES_DIR: &str = "/caches";

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
    /// Root of the persistent database and configuration directory.
    pub data_dir: PathBuf,
    /// Root of the persistent local object store.
    pub objects_dir: PathBuf,
    /// Root for rebuildable on-disk caches.
    pub caches_dir: PathBuf,
    /// Browser bundle directory served for non-API routes.
    pub web_dir: PathBuf,
    /// Public base URL, used for share links and the same-origin check.
    pub base_url: String,
    /// Optional HTTPS authority on the same hostname, serving HTTP/2 only.
    pub http2_origin: Option<String>,
    /// Optional native TLS and QUIC listeners, in addition to APP_ADDR.
    pub tls: Option<crate::quic::TlsConfig>,
    pub quic: Option<crate::quic::QuicConfig>,
    /// Direct public ingress and automatically managed certificates.
    pub acme: Option<crate::ingress::AcmeSettings>,
    /// Public UDP port; independent of container/NAT listener addresses.
    pub quic_public_port: Option<u16>,
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
    /// Maximum gap between upload body frames.
    pub upload_idle_timeout: Duration,
    /// Maximum duration of a single upload request.
    pub upload_request_timeout: Duration,
    /// Free space to keep after accepting bytes.
    pub upload_min_free_bytes: i64,
    /// Maximum simultaneous upload writes and assemblies across all sessions.
    pub upload_concurrency: usize,
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
        let mut env: HashMap<String, String> = std::env::vars().collect();
        match dotenvy::from_path_iter(".env") {
            Ok(values) => {
                for value in values {
                    let (name, value) = value
                        .map_err(|error| ConfigError::new(format!("invalid .env: {error}")))?;
                    env.entry(name).or_insert(value);
                }
            }
            Err(dotenvy::Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(ConfigError::new(format!("could not read .env: {error}"))),
        }
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
        let acme =
            crate::ingress::AcmeSettings::load(lookup, &data_dir).map_err(ConfigError::new)?;
        let default_base = acme.as_ref().map_or_else(
            || DEFAULT_BASE_URL.to_owned(),
            |a| format!("https://{}", a.domain),
        );
        let base_url = value("APP_BASE_URL", &default_base)
            .trim()
            .trim_end_matches('/')
            .to_owned();
        if !is_absolute_http_url(&base_url) {
            return Err(ConfigError::new(
                "APP_BASE_URL must be an absolute http(s) URL",
            ));
        }

        let base_url = url::Url::parse(&base_url)
            .map_err(|_| ConfigError::new("invalid APP_BASE_URL"))?
            .as_str()
            .trim_end_matches('/')
            .to_owned();
        let http2_origin = lookup("APP_HTTP2_BASE_URL").filter(|v| !v.trim().is_empty())
            .map(|raw| {
                let fallback = url::Url::parse(raw.trim()).map_err(|_| ConfigError::new("invalid APP_HTTP2_BASE_URL"))?;
                let primary = url::Url::parse(&base_url).expect("validated base URL");
                if primary.scheme() != "https" || fallback.scheme() != "https"
                    || fallback.host_str() != primary.host_str() || fallback.origin() == primary.origin()
                    || !fallback.username().is_empty() || fallback.password().is_some()
                    || fallback.query().is_some() || fallback.fragment().is_some()
                    || fallback.path() != "/" {
                    return Err(ConfigError::new("APP_HTTP2_BASE_URL must be a distinct HTTPS port on the APP_BASE_URL hostname"));
                }
                Ok(fallback.origin().ascii_serialization())
            }).transpose()?;
        let native_lookup = |key: &str| {
            lookup(key).filter(|v| !v.trim().is_empty()).or_else(|| {
                acme.as_ref().and_then(|_| match key {
                    "APP_TLS_ADDR" | "APP_QUIC_ADDR" => Some("0.0.0.0:443".into()),
                    _ => None,
                })
            })
        };
        let tls = crate::quic::TlsConfig::load(&native_lookup, acme.is_some())
            .map_err(ConfigError::new)?;
        let quic = crate::quic::QuicConfig::load(&native_lookup).map_err(ConfigError::new)?;
        if quic.is_some() && tls.is_none() {
            return Err(ConfigError::new(
                "APP_QUIC_ADDR requires native APP_TLS_ADDR and TLS certificates",
            ));
        }
        if let Some(tls) = &tls {
            let primary = url::Url::parse(&base_url).expect("validated base URL");
            if primary.scheme() != "https" {
                return Err(ConfigError::new(
                    "APP_BASE_URL must use HTTPS with native TLS",
                ));
            }
            if let Some(addr) = tls.http2_addr
                && (addr == tls.addr || http2_origin.is_none())
            {
                return Err(ConfigError::new(
                    "APP_HTTP2_ADDR requires a distinct listener and APP_HTTP2_BASE_URL",
                ));
            }
        }
        let cookie_secure = match lookup("COOKIE_SECURE") {
            Some(raw) if !raw.is_empty() => parse_bool("COOKIE_SECURE", &raw)?,
            _ => base_url.starts_with("https://"),
        };
        if let Some(settings) = &acme {
            let primary = url::Url::parse(&base_url).expect("validated base URL");
            if primary.host_str() != Some(settings.domain.as_str())
                || primary.path() != "/"
                || !primary.username().is_empty()
                || primary.password().is_some()
                || primary.query().is_some()
                || primary.fragment().is_some()
            {
                return Err(ConfigError::new(
                    "APP_BASE_URL must be the HTTPS origin of APP_DOMAIN",
                ));
            }
            if !cookie_secure {
                return Err(ConfigError::new(
                    "COOKIE_SECURE must be true with APP_DOMAIN",
                ));
            }
        }
        let quic_public_port = quic
            .as_ref()
            .map(|_| {
                let raw = lookup("APP_QUIC_PUBLIC_PORT").filter(|v| !v.trim().is_empty());
                let port = match raw {
                    Some(raw) => raw
                        .parse::<u16>()
                        .map_err(|_| ConfigError::new("invalid APP_QUIC_PUBLIC_PORT"))?,
                    None => url::Url::parse(&base_url)
                        .expect("validated base URL")
                        .port_or_known_default()
                        .expect("HTTP port"),
                };
                if port == 0 {
                    return Err(ConfigError::new("APP_QUIC_PUBLIC_PORT must be nonzero"));
                }
                Ok(port)
            })
            .transpose()?;

        let caches_dir_raw = value("APP_CACHES_DIR", DEFAULT_CACHES_DIR);
        if caches_dir_raw.trim().is_empty() {
            return Err(ConfigError::new("APP_CACHES_DIR must not be empty"));
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

        let upload_expires =
            parse_duration_env("UPLOAD_EXPIRES", lookup, Duration::from_secs(24 * 3600))?;
        if upload_expires.is_zero() {
            return Err(ConfigError::new("UPLOAD_EXPIRES must be positive"));
        }

        let upload_idle_timeout =
            parse_duration_env("UPLOAD_IDLE_TIMEOUT", lookup, Duration::from_secs(60))?;
        let upload_request_timeout = parse_duration_env(
            "UPLOAD_REQUEST_TIMEOUT",
            lookup,
            Duration::from_secs(24 * 3600),
        )?;
        let upload_min_free_bytes = parse_int("UPLOAD_MIN_FREE_BYTES", lookup, 64 << 20)?;
        let upload_concurrency = parse_int("UPLOAD_CONCURRENCY", lookup, 8)?;
        if !(1..=64).contains(&upload_concurrency) {
            return Err(ConfigError::new(
                "UPLOAD_CONCURRENCY must be between 1 and 64",
            ));
        }
        if upload_idle_timeout.is_zero()
            || upload_request_timeout.is_zero()
            || upload_min_free_bytes < 0
        {
            return Err(ConfigError::new(
                "upload timeouts must be positive and free-space reserve nonnegative",
            ));
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
            addr: value(
                "APP_ADDR",
                if acme.is_some() {
                    "0.0.0.0:80"
                } else {
                    DEFAULT_ADDR
                },
            ),
            data_dir,
            objects_dir: PathBuf::from(value("APP_OBJECTS_DIR", DEFAULT_OBJECTS_DIR)),
            caches_dir: PathBuf::from(caches_dir_raw),
            web_dir: PathBuf::from(value("APP_WEB_DIR", DEFAULT_WEB_DIR)),
            base_url,
            http2_origin,
            tls,
            quic,
            acme,
            quic_public_port,
            cookie_secure,
            admin_username: lookup("ADMIN_USERNAME").unwrap_or_default(),
            admin_password: lookup("ADMIN_PASSWORD").unwrap_or_default(),
            media_cache_capacity,
            upload_expires,
            upload_idle_timeout,
            upload_request_timeout,
            upload_min_free_bytes,
            upload_concurrency: upload_concurrency as usize,
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
        self.objects_dir.clone()
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
            "addr={} data={} objects={} work={} web={} base_url={} secure_cookie={}",
            self.addr,
            self.data_dir.display(),
            self.objects_dir.display(),
            self.caches_dir.display(),
            self.web_dir.display(),
            self.base_url,
            self.cookie_secure
        )
    }
}

/// True when `value` is an absolute `http(s)` URL with a host.
fn is_absolute_http_url(value: &str) -> bool {
    url::Url::parse(value).is_ok_and(|u| {
        matches!(u.scheme(), "http" | "https")
            && u.host_str().is_some()
            && u.username().is_empty()
            && u.password().is_none()
            && u.query().is_none()
            && u.fragment().is_none()
            && u.path() == "/"
    })
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
/// Negative durations are rejected; explicit zero is checked by each setting.
///
/// # Errors
/// Returns a human-readable reason when the value cannot be parsed.
pub fn parse_duration(value: &str) -> Result<Duration, String> {
    let raw = value.trim();
    if raw.is_empty() {
        return Err("empty duration".to_owned());
    }
    if raw.starts_with('-') {
        return Err("duration must not be negative".to_owned());
    }
    let body = raw.strip_prefix('+').unwrap_or(raw);
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
    Ok(Duration::from_nanos(nanos as u64))
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
    fn http2_fallback_preserves_host_only_cookie_scope() {
        let config = Config::from_lookup(&|name| match name {
            "APP_BASE_URL" => Some("https://files.example.test".into()),
            "APP_HTTP2_BASE_URL" => Some("https://files.example.test:8443".into()),
            _ => None,
        })
        .unwrap();
        assert_eq!(
            config.http2_origin.as_deref(),
            Some("https://files.example.test:8443")
        );
        for bad in [
            "http://files.example.test:8443",
            "https://evil.example.test",
            "https://files.example.test",
            "https://files.example.test:8443/path",
        ] {
            assert!(
                Config::from_lookup(&|name| match name {
                    "APP_BASE_URL" => Some("https://files.example.test".into()),
                    "APP_HTTP2_BASE_URL" => Some(bad.into()),
                    _ => None,
                })
                .is_err()
            );
        }
    }

    #[test]
    fn defaults_match_the_documented_values() {
        let config = config_from(&[]).unwrap();
        assert_eq!(config.addr, DEFAULT_ADDR);
        assert_eq!(config.data_dir, PathBuf::from(DEFAULT_DATA_DIR));
        assert_eq!(config.objects_dir, PathBuf::from(DEFAULT_OBJECTS_DIR));
        assert_eq!(config.caches_dir, PathBuf::from(DEFAULT_CACHES_DIR));
        assert_eq!(config.web_dir, PathBuf::from(DEFAULT_WEB_DIR));
        assert_eq!(config.base_url, DEFAULT_BASE_URL);
        assert!(!config.cookie_secure);
        assert!(config.tls.is_none());
        assert!(config.quic.is_none());
        assert_eq!(config.media_cache_capacity, 2 * 1024 * 1024 * 1024);
        assert_eq!(config.upload_expires, Duration::from_secs(24 * 3600));
        assert_eq!(config.trash_retention, Duration::from_secs(30 * 24 * 3600));
        assert_eq!(config.gc_interval, Duration::from_secs(3600));
        assert_eq!(config.flow_cache_ttl, Duration::from_secs(720 * 3600));
        assert_eq!(config.flow_cache_capacity, 1 << 30);
        assert!(config.trusted_proxies.is_empty());
    }

    #[test]
    fn native_quic_requires_https_and_a_matching_native_fallback_authority() {
        let valid = [
            ("APP_BASE_URL", "https://files.example.test:8443"),
            ("APP_TLS_ADDR", "0.0.0.0:8443"),
            ("APP_TLS_CERT", "/tls/cert.pem"),
            ("APP_TLS_KEY", "/tls/key.pem"),
            ("APP_QUIC_ADDR", "0.0.0.0:8443"),
            ("APP_HTTP2_ADDR", "0.0.0.0:8444"),
            ("APP_HTTP2_BASE_URL", "https://files.example.test:8444"),
        ];
        assert!(config_from(&valid).is_ok());
        for omitted in ["APP_TLS_ADDR", "APP_TLS_CERT", "APP_HTTP2_BASE_URL"] {
            assert!(
                config_from(
                    &valid
                        .iter()
                        .copied()
                        .filter(|(key, _)| *key != omitted)
                        .collect::<Vec<_>>()
                )
                .is_err()
            );
        }
        let mut values = valid.to_vec();
        values.push(("APP_BASE_URL", "http://files.example.test:8443"));
        assert!(config_from(&values).is_err());
        let mut mapped = valid.to_vec();
        mapped.push(("APP_BASE_URL", "https://files.example.test"));
        let mapped = config_from(&mapped).unwrap();
        assert_eq!(mapped.quic_public_port, Some(443));
    }

    #[test]
    fn public_domain_enables_acme_and_all_three_public_ports() {
        let config = config_from(&[
            ("APP_DOMAIN", "Files.Example.Test."),
            ("APP_DATA_DIR", "/state"),
        ])
        .unwrap();
        assert_eq!(config.base_url, "https://files.example.test");
        assert_eq!(config.listen_addr().unwrap().port(), 80);
        assert_eq!(config.tls.as_ref().unwrap().addr.port(), 443);
        assert_eq!(
            config.tls.as_ref().unwrap().identity,
            crate::quic::TlsIdentity::Acme
        );
        assert_eq!(config.quic.as_ref().unwrap().addr.port(), 443);
        assert_eq!(config.quic_public_port, Some(443));
        assert_eq!(
            config.quic.as_ref().unwrap().mode,
            crate::quic::CongestionMode::Aggressive
        );
        assert_eq!(config.quic.as_ref().unwrap().target_mbps, None);
        assert_eq!(config.acme.unwrap().cache_dir, PathBuf::from("/state/acme"));
        assert!(config.cookie_secure);
    }

    #[test]
    fn public_domain_rejects_ambiguous_or_insecure_settings() {
        for domain in [
            "localhost",
            "127.0.0.1",
            "https://files.example.test",
            "files.example.test:443",
            "*.example.test",
            "bad_.example.test",
            "a..example.test",
        ] {
            assert!(config_from(&[("APP_DOMAIN", domain)]).is_err(), "{domain}");
        }
        for (key, value) in [
            ("APP_BASE_URL", "https://other.example.test"),
            ("APP_BASE_URL", "http://files.example.test"),
            ("APP_BASE_URL", "https://files.example.test/path"),
            ("APP_TLS_CERT", "/cert.pem"),
            ("APP_TLS_KEY", "/key.pem"),
            ("ACME_DIRECTORY_URL", "http://ca.example.test/directory"),
            ("ACME_STAGING", "yes"),
            ("COOKIE_SECURE", "false"),
            ("APP_QUIC_PUBLIC_PORT", "0"),
        ] {
            assert!(
                config_from(&[("APP_DOMAIN", "files.example.test"), (key, value)]).is_err(),
                "{key}"
            );
        }
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
    fn upload_concurrency_is_bounded() {
        assert_eq!(
            config_from(&[("UPLOAD_CONCURRENCY", "4")])
                .unwrap()
                .upload_concurrency,
            4
        );
        assert!(config_from(&[("UPLOAD_CONCURRENCY", "0")]).is_err());
        assert!(config_from(&[("UPLOAD_CONCURRENCY", "65")]).is_err());
    }

    #[test]
    fn rejects_negative_durations_for_every_setting() {
        for name in [
            "TRASH_RETENTION",
            "GC_INTERVAL",
            "FLOW_CACHE_TTL",
            "UPLOAD_EXPIRES",
        ] {
            for raw in ["-1h", " -1h ", "-0"] {
                assert!(config_from(&[(name, raw)]).is_err(), "{name}={raw}");
            }
        }
        for name in ["TRASH_RETENTION", "GC_INTERVAL", "FLOW_CACHE_TTL"] {
            assert!(config_from(&[(name, "0")]).is_ok());
        }
    }

    #[test]
    fn parses_trusted_proxies() {
        let config = config_from(&[("TRUSTED_PROXIES", "10.0.0.0/8, 192.168.1.0/24")]).unwrap();
        assert_eq!(config.trusted_proxies.len(), 2);
        assert!(config_from(&[("TRUSTED_PROXIES", "nope")]).is_err());
    }

    #[test]
    fn database_and_objects_can_live_on_different_volumes() {
        let config = config_from(&[
            ("APP_DATA_DIR", "/srv/revaro"),
            ("APP_OBJECTS_DIR", "/mnt/objects"),
        ])
        .unwrap();
        assert_eq!(
            config.database_path(),
            PathBuf::from("/srv/revaro/revaro.db")
        );
        assert_eq!(config.objects_dir(), PathBuf::from("/mnt/objects"));
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
        assert!(parse_duration("-1h").is_err());
        // A trailing minus is not a sign and must be rejected outright.
        assert!(parse_duration("1h-").is_err());
    }
    #[test]
    fn url_validation_normalizes_origin_and_rejects_ambiguous_settings() {
        let config = config_from(&[("APP_BASE_URL", "  HTTPS://Example.COM:443/  ")]).unwrap();
        assert_eq!(config.base_url, "https://example.com");
        assert!(config.cookie_secure);
        for raw in [
            "http://example.com:invalid",
            "http://user:pass@example.com",
            "https://example.com/path",
            "https://example.com?query=1",
            "https://example.com#fragment",
        ] {
            assert!(config_from(&[("APP_BASE_URL", raw)]).is_err(), "{raw}");
        }
        assert!(config_from(&[("APP_BASE_URL", "http://[::1]:8080")]).is_ok());
    }
}
