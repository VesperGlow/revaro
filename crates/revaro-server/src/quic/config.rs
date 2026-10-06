use std::{net::SocketAddr, path::PathBuf};

/// Native HTTPS listeners. The fallback listener never advertises QUIC.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TlsConfig {
    pub addr: SocketAddr,
    pub http2_addr: Option<SocketAddr>,
    pub identity: TlsIdentity,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TlsIdentity {
    Acme,
    Pem { cert: PathBuf, key: PathBuf },
}

/// Sender policy; Cubic remains available for compatibility and diagnosis.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CongestionMode {
    Standard,
    #[default]
    Aggressive,
}

/// Mbps means decimal megabits/second, shared by every stream on a connection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuicConfig {
    pub addr: SocketAddr,
    pub mode: CongestionMode,
    /// None estimates the available bandwidth; Some pins an operator's target.
    pub target_mbps: Option<u32>,
    pub max_mbps: u32,
    pub global_max_mbps: u32,
    pub max_compensation_percent: u32,
    pub max_window_mib: u32,
    pub max_connections: u32,
}

fn number(
    lookup: &dyn Fn(&str) -> Option<String>,
    key: &str,
    default: u32,
    min: u32,
    max: u32,
) -> Result<u32, String> {
    let value = lookup(key)
        .filter(|s| !s.is_empty())
        .map_or(Ok(default), |s| {
            s.parse::<u32>().map_err(|_| format!("invalid {key}"))
        })?;
    if !(min..=max).contains(&value) {
        return Err(format!("{key} must be between {min} and {max}"));
    }
    Ok(value)
}

fn address(
    lookup: &dyn Fn(&str) -> Option<String>,
    key: &str,
) -> Result<Option<SocketAddr>, String> {
    lookup(key)
        .filter(|s| !s.trim().is_empty())
        .map(|s| {
            let addr: SocketAddr = s
                .parse()
                .map_err(|_| format!("{key} must be an IP address and port"))?;
            if addr.port() == 0 {
                return Err(format!("{key} must use a nonzero port"));
            }
            Ok(addr)
        })
        .transpose()
}

impl TlsConfig {
    pub(crate) fn load(
        lookup: &dyn Fn(&str) -> Option<String>,
        managed: bool,
    ) -> Result<Option<Self>, String> {
        let Some(addr) = address(lookup, "APP_TLS_ADDR")? else {
            if address(lookup, "APP_HTTP2_ADDR")?.is_some() {
                return Err("APP_HTTP2_ADDR requires APP_TLS_ADDR".into());
            }
            return Ok(None);
        };
        let path = |key| {
            lookup(key)
                .filter(|s| !s.trim().is_empty())
                .map(PathBuf::from)
                .ok_or_else(|| format!("{key} is required with APP_TLS_ADDR"))
        };
        Ok(Some(Self {
            addr,
            http2_addr: address(lookup, "APP_HTTP2_ADDR")?,
            identity: if managed {
                if lookup("APP_TLS_CERT").is_some_and(|v| !v.trim().is_empty())
                    || lookup("APP_TLS_KEY").is_some_and(|v| !v.trim().is_empty())
                {
                    return Err(
                        "APP_DOMAIN cannot be combined with manual APP_TLS_CERT/APP_TLS_KEY".into(),
                    );
                }
                TlsIdentity::Acme
            } else {
                TlsIdentity::Pem {
                    cert: path("APP_TLS_CERT")?,
                    key: path("APP_TLS_KEY")?,
                }
            },
        }))
    }
}

impl QuicConfig {
    pub(crate) fn load(lookup: &dyn Fn(&str) -> Option<String>) -> Result<Option<Self>, String> {
        let Some(addr) = address(lookup, "APP_QUIC_ADDR")? else {
            return Ok(None);
        };
        let mode = match lookup("QUIC_CC_MODE").as_deref().unwrap_or("aggressive") {
            "standard" => CongestionMode::Standard,
            "aggressive" | "" => CongestionMode::Aggressive,
            _ => return Err("QUIC_CC_MODE must be standard or aggressive".into()),
        };
        let target_mbps = match lookup("QUIC_TARGET_MBPS").as_deref().map(str::trim) {
            None | Some("" | "auto") => None,
            _ => Some(number(lookup, "QUIC_TARGET_MBPS", 4, 1, 1000)?),
        };
        let config = Self {
            addr,
            mode,
            target_mbps,
            max_mbps: number(lookup, "QUIC_MAX_MBPS", 250, 1, 1000)?,
            global_max_mbps: number(lookup, "QUIC_GLOBAL_MAX_MBPS", 1000, 1, 1000)?,
            max_compensation_percent: number(
                lookup,
                "QUIC_MAX_COMPENSATION_PERCENT",
                125,
                100,
                200,
            )?,
            max_window_mib: number(lookup, "QUIC_MAX_WINDOW_MIB", 8, 1, 64)?,
            max_connections: number(lookup, "QUIC_MAX_CONNECTIONS", 32, 1, 128)?,
        };
        if (mode == CongestionMode::Aggressive
            && config
                .target_mbps
                .is_some_and(|target| target > config.max_mbps))
            || config.max_mbps > config.global_max_mbps
        {
            return Err(
                "QUIC_TARGET_MBPS (aggressive) <= QUIC_MAX_MBPS <= QUIC_GLOBAL_MAX_MBPS is required".into(),
            );
        }
        Ok(Some(config))
    }

    pub(crate) fn target_bytes(&self) -> Option<u64> {
        self.target_mbps.map(|target| u64::from(target) * 125_000)
    }
    pub(crate) fn initial_rate(&self) -> u64 {
        self.target_bytes()
            .map_or(4 * 125_000, |target| target / 4)
            .min(self.max_bytes())
    }
    pub(crate) fn max_bytes(&self) -> u64 {
        u64::from(self.max_mbps) * 125_000
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn lookup(values: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let values: std::collections::HashMap<_, _> = values
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |key| values.get(key).cloned()
    }

    #[test]
    fn optional_endpoint_defaults_to_aggressive_auto_and_keeps_manual_modes() {
        assert!(QuicConfig::load(&lookup(&[])).unwrap().is_none());
        let config = QuicConfig::load(&lookup(&[("APP_QUIC_ADDR", "127.0.0.1:443")]))
            .unwrap()
            .unwrap();
        assert_eq!(config.mode, CongestionMode::Aggressive);
        assert_eq!(config.target_mbps, None);
        assert_eq!(config.max_compensation_percent, 125);
        assert!(
            QuicConfig::load(&lookup(&[
                ("APP_QUIC_ADDR", "127.0.0.1:443"),
                ("QUIC_MAX_MBPS", "2")
            ]))
            .is_ok()
        );
        for target in ["", "auto", "12"] {
            let config = QuicConfig::load(&lookup(&[
                ("APP_QUIC_ADDR", "127.0.0.1:443"),
                ("QUIC_CC_MODE", "standard"),
                ("QUIC_TARGET_MBPS", target),
            ]))
            .unwrap()
            .unwrap();
            assert_eq!(config.mode, CongestionMode::Standard);
            assert_eq!(
                config.target_mbps,
                if target == "12" { Some(12) } else { None }
            );
        }
    }

    #[test]
    fn rejects_unbounded_and_inconsistent_limits_and_invalid_ports() {
        for (key, value) in [
            ("QUIC_MAX_COMPENSATION_PERCENT", "201"),
            ("QUIC_MAX_WINDOW_MIB", "65"),
            ("QUIC_GLOBAL_MAX_MBPS", "1001"),
            ("QUIC_MAX_CONNECTIONS", "129"),
            ("QUIC_CC_MODE", "brutal"),
            ("QUIC_TARGET_MBPS", "51"),
            ("APP_QUIC_ADDR", "127.0.0.1:0"),
        ] {
            assert!(
                QuicConfig::load(&lookup(&[
                    ("APP_QUIC_ADDR", "127.0.0.1:443"),
                    ("QUIC_CC_MODE", "aggressive"),
                    ("QUIC_TARGET_MBPS", "10"),
                    ("QUIC_MAX_MBPS", "50"),
                    (key, value)
                ]))
                .is_err(),
                "{key}={value}"
            );
        }
        assert!(TlsConfig::load(&lookup(&[("APP_HTTP2_ADDR", "127.0.0.1:4434")]), false).is_err());
        assert!(TlsConfig::load(&lookup(&[("APP_TLS_ADDR", "127.0.0.1:443")]), false).is_err());
    }
}
