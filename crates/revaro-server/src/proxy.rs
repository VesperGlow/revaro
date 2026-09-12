//! Trusted-proxy prefixes for `X-Forwarded-For` handling.
//!
//! Ported from the Go configuration, which used `netip.ParsePrefix` plus
//! `Prefix.Masked()`. Only the operations the product needs are implemented:
//! parse, mask to the network address, and test containment.

use std::fmt;
use std::net::IpAddr;
use std::str::FromStr;

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
