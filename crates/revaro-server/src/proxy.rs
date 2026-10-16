//! Client identity and public scheme behind trusted HTTP reverse proxies.
//!
//! Ported from the Go configuration, which used `netip.ParsePrefix` plus
//! `Prefix.Masked()`. Only the operations the product needs are implemented:
//! parse, mask to the network address, and test containment.

use std::fmt;
use std::net::IpAddr;
use std::net::SocketAddr;
use std::str::FromStr;
use std::sync::Arc;

use axum::extract::{ConnectInfo, FromRequestParts};
use http::{HeaderMap, request::Parts};

use crate::{config::Config, state::AppState};

/// Request metadata derived only from the socket peer and trusted proxy headers.
#[derive(Debug, Clone)]
pub struct RequestContext {
    pub client_ip: String,
    pub is_https: bool,
    pub secure_cookie: bool,
}

impl RequestContext {
    pub fn resolve(headers: &HeaderMap, config: &Config, peer: Option<SocketAddr>) -> Self {
        let peer = peer.map(|peer| unmap(peer.ip()));
        let trusted = |ip| {
            config
                .trusted_proxies
                .iter()
                .any(|prefix| prefix.contains(ip))
        };
        let proxy = peer.is_some_and(trusted);
        let mut client = peer;
        if proxy {
            // Walk from the immediate proxy towards the client. Never skip a
            // malformed hop to accept a possibly forged value farther left.
            if headers.contains_key("x-forwarded-for") {
                'chain: for header in headers.get_all("x-forwarded-for").iter().rev() {
                    let Ok(header) = header.to_str() else { break };
                    for candidate in header.split(',').rev() {
                        let Ok(address) = candidate.trim().parse::<IpAddr>() else {
                            break 'chain;
                        };
                        let address = unmap(address);
                        client = Some(address);
                        if !trusted(address) {
                            break 'chain;
                        }
                    }
                }
            } else if let Some(address) = headers
                .get("x-real-ip")
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.trim().parse::<IpAddr>().ok())
            {
                client = Some(unmap(address));
            }
        }
        // Proxies must overwrite this header with one scheme, not append a
        // client-supplied list. APP_BASE_URL remains the canonical public URL.
        let forwarded_https = proxy
            && headers.get_all("x-forwarded-proto").iter().count() == 1
            && headers
                .get("x-forwarded-proto")
                .and_then(|value| value.to_str().ok())
                .is_some_and(|value| value.eq_ignore_ascii_case("https"));
        Self {
            client_ip: client.map_or_else(String::new, |ip| ip.to_string()),
            is_https: forwarded_https || config.base_url.starts_with("https://"),
            secure_cookie: config.cookie_secure || forwarded_https,
        }
    }
}

impl FromRequestParts<Arc<AppState>> for RequestContext {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &Arc<AppState>,
    ) -> Result<Self, Self::Rejection> {
        if let Some(context) = parts.extensions.get::<Self>() {
            return Ok(context.clone());
        }
        let peer = parts
            .extensions
            .get::<ConnectInfo<SocketAddr>>()
            .map(|peer| peer.0);
        Ok(Self::resolve(&parts.headers, &state.config, peer))
    }
}

fn unmap(address: IpAddr) -> IpAddr {
    match address {
        IpAddr::V6(address) => address
            .to_ipv4_mapped()
            .map_or(IpAddr::V6(address), IpAddr::V4),
        address => address,
    }
}

/// A CIDR network.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Prefix {
    network: IpAddr,
    bits: u8,
}

/// Failure modes of [`Prefix::parse`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PrefixError {
    /// The value had no `/`, or had more than one.
    #[error("invalid CIDR {0:?}")]
    Shape(String),
    /// The address half was not an IP address.
    #[error("invalid address in CIDR {0:?}")]
    Address(String),
    /// The prefix length was not a number, or was out of range for the family.
    #[error("invalid prefix length in CIDR {0:?}")]
    Length(String),
}

impl Prefix {
    /// Parse `address/length`, masking away host bits.
    ///
    /// # Errors
    /// Returns a [`PrefixError`] describing which half was malformed. The Go
    /// server rejected the whole configuration in that case, and so do we.
    pub fn parse(value: &str) -> Result<Self, PrefixError> {
        let (address, length) = value
            .split_once('/')
            .ok_or_else(|| PrefixError::Shape(value.to_owned()))?;
        if length.contains('/') {
            return Err(PrefixError::Shape(value.to_owned()));
        }
        let address =
            IpAddr::from_str(address.trim()).map_err(|_| PrefixError::Address(value.to_owned()))?;
        let length: u8 = length
            .trim()
            .parse()
            .map_err(|_| PrefixError::Length(value.to_owned()))?;
        let max = if address.is_ipv4() { 32 } else { 128 };
        if length > max {
            return Err(PrefixError::Length(value.to_owned()));
        }
        Ok(Self {
            network: mask(address, length),
            bits: length,
        })
    }

    /// The masked network address.
    #[must_use]
    pub fn network(&self) -> IpAddr {
        self.network
    }

    /// The prefix length in bits.
    #[must_use]
    pub fn bits(&self) -> u8 {
        self.bits
    }

    /// True when `candidate` belongs to this network.
    ///
    /// Addresses of a different family never match.
    #[must_use]
    pub fn contains(&self, candidate: IpAddr) -> bool {
        match (self.network, candidate) {
            (IpAddr::V4(_), IpAddr::V4(_)) | (IpAddr::V6(_), IpAddr::V6(_)) => {
                mask(candidate, self.bits) == self.network
            }
            _ => false,
        }
    }
}

impl fmt::Display for Prefix {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.network, self.bits)
    }
}

/// Zero every bit below the prefix length.
fn mask(address: IpAddr, bits: u8) -> IpAddr {
    match address {
        IpAddr::V4(value) => {
            let raw = u32::from(value);
            let masked = if bits == 0 {
                0
            } else {
                raw & (u32::MAX << (32 - u32::from(bits)))
            };
            IpAddr::V4(masked.into())
        }
        IpAddr::V6(value) => {
            let raw = u128::from(value);
            let masked = if bits == 0 {
                0
            } else {
                raw & (u128::MAX << (128 - u32::from(bits)))
            };
            IpAddr::V6(masked.into())
        }
    }
}

/// Parse a comma-separated `TRUSTED_PROXIES` value.
///
/// # Errors
/// Returns the offending entry when any element is not a valid CIDR.
pub fn parse_list(value: &str) -> Result<Vec<Prefix>, String> {
    let mut prefixes = Vec::new();
    for item in value.split(',') {
        let item = item.trim();
        if item.is_empty() {
            continue;
        }
        let prefix = Prefix::parse(item)
            .map_err(|_| format!("TRUSTED_PROXIES contains invalid CIDR {item:?}"))?;
        prefixes.push(prefix);
    }
    Ok(prefixes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context(peer: Option<&str>, forwarded: &[(&str, &str)]) -> RequestContext {
        let config = Config::from_lookup(&|name| match name {
            "TRUSTED_PROXIES" => Some("127.0.0.1/32,::1/128,10.0.0.0/8".into()),
            _ => None,
        })
        .unwrap();
        let mut headers = HeaderMap::new();
        for (name, value) in forwarded {
            headers.append(
                http::header::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                value.parse().unwrap(),
            );
        }
        RequestContext::resolve(&headers, &config, peer.map(|peer| peer.parse().unwrap()))
    }

    #[test]
    fn untrusted_or_missing_peers_cannot_spoof_client_or_scheme() {
        let headers = [
            ("x-forwarded-for", "192.0.2.5"),
            ("x-real-ip", "192.0.2.6"),
            ("x-forwarded-proto", "https"),
        ];
        let direct = context(Some("198.51.100.1:12345"), &headers);
        assert_eq!(direct.client_ip, "198.51.100.1");
        assert!(!direct.is_https);
        assert!(!direct.secure_cookie);
        let missing = context(None, &headers);
        assert!(missing.client_ip.is_empty());
        assert!(!missing.is_https);
    }

    #[test]
    fn trusted_proxy_walks_the_chain_and_recognizes_https() {
        let forwarded = [
            ("x-forwarded-for", "192.0.2.99, 198.51.100.7, 10.1.2.3"),
            ("x-forwarded-proto", "https"),
        ];
        let result = context(Some("127.0.0.1:12345"), &forwarded);
        assert_eq!(result.client_ip, "198.51.100.7");
        assert!(result.is_https);
        assert!(result.secure_cookie);
        let mapped = context(Some("[::ffff:127.0.0.1]:12345"), &forwarded);
        assert_eq!(mapped.client_ip, result.client_ip);
        let v6 = context(Some("[::1]:12345"), &[("x-forwarded-for", "2001:db8::1")]);
        assert_eq!(v6.client_ip, "2001:db8::1");
    }

    #[test]
    fn malformed_hops_and_duplicate_headers_preserve_the_trust_boundary() {
        let invalid = context(
            Some("127.0.0.1:12345"),
            &[("x-forwarded-for", "192.0.2.99, invalid, 10.1.2.3")],
        );
        assert_eq!(invalid.client_ip, "10.1.2.3");
        let repeated = context(
            Some("127.0.0.1:12345"),
            &[
                ("x-forwarded-for", "192.0.2.99"),
                ("x-forwarded-for", "198.51.100.7, 10.1.2.3"),
            ],
        );
        assert_eq!(repeated.client_ip, "198.51.100.7");
        for headers in [
            vec![("x-forwarded-proto", "https, http")],
            vec![
                ("x-forwarded-proto", "https"),
                ("x-forwarded-proto", "http"),
            ],
        ] {
            assert!(!context(Some("127.0.0.1:12345"), &headers).is_https);
        }
    }

    #[test]
    fn real_ip_is_a_fallback_only_when_the_forwarded_chain_is_absent() {
        let real = context(Some("127.0.0.1:12345"), &[("x-real-ip", "198.51.100.9")]);
        assert_eq!(real.client_ip, "198.51.100.9");
        let invalid = context(
            Some("127.0.0.1:12345"),
            &[
                ("x-forwarded-for", "invalid"),
                ("x-real-ip", "198.51.100.9"),
            ],
        );
        assert_eq!(invalid.client_ip, "127.0.0.1");
    }

    fn ip(value: &str) -> IpAddr {
        value.parse().unwrap()
    }

    #[test]
    fn parses_and_masks_ipv4() {
        let prefix = Prefix::parse("10.0.0.5/8").unwrap();
        assert_eq!(prefix.network(), ip("10.0.0.0"));
        assert_eq!(prefix.bits(), 8);
        assert_eq!(prefix.to_string(), "10.0.0.0/8");
    }

    #[test]
    fn parses_and_masks_ipv6() {
        let prefix = Prefix::parse("2001:db8::1/32").unwrap();
        assert_eq!(prefix.network(), ip("2001:db8::"));
        assert_eq!(prefix.bits(), 32);
    }

    #[test]
    fn matches_addresses_inside_the_network() {
        let prefix = Prefix::parse("10.0.0.0/8").unwrap();
        assert!(prefix.contains(ip("10.1.2.3")));
        assert!(prefix.contains(ip("10.255.255.255")));
        assert!(!prefix.contains(ip("11.0.0.1")));
        assert!(!prefix.contains(ip("::1")));
    }

    #[test]
    fn a_zero_length_prefix_matches_the_whole_family() {
        let prefix = Prefix::parse("0.0.0.0/0").unwrap();
        assert!(prefix.contains(ip("1.2.3.4")));
        assert!(prefix.contains(ip("255.255.255.255")));
        assert!(!prefix.contains(ip("::1")));

        let prefix = Prefix::parse("::/0").unwrap();
        assert!(prefix.contains(ip("::1")));
        assert!(!prefix.contains(ip("1.2.3.4")));
    }

    #[test]
    fn a_host_prefix_matches_only_itself() {
        let prefix = Prefix::parse("192.168.1.5/32").unwrap();
        assert!(prefix.contains(ip("192.168.1.5")));
        assert!(!prefix.contains(ip("192.168.1.6")));
    }

    #[test]
    fn rejects_malformed_prefixes() {
        for value in [
            "10.0.0.0",
            "10.0.0.0/8/8",
            "not-an-ip/8",
            "10.0.0.0/33",
            "::1/129",
            "10.0.0.0/x",
        ] {
            assert!(
                Prefix::parse(value).is_err(),
                "{value:?} should be rejected"
            );
        }
    }

    #[test]
    fn parses_lists_and_skips_blanks() {
        let list = parse_list(" 10.0.0.0/8 , 192.168.0.0/16 , ").unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].to_string(), "10.0.0.0/8");
        assert_eq!(list[1].to_string(), "192.168.0.0/16");
        assert!(parse_list("").unwrap().is_empty());
    }

    #[test]
    fn reports_the_offending_list_entry() {
        let error = parse_list("10.0.0.0/8,nope").unwrap_err();
        assert!(error.contains("nope"), "{error}");
    }
}
