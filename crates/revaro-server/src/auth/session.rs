//! Session tokens and their storage form.
//!
//! The token handed to the browser is 32 random bytes in Go's
//! `base64.RawURLEncoding` (43 characters). Only
//! `base64 RawURL(sha256(token))` is persisted, so a database leak does not
//! hand out live sessions. The row id is a UUIDv4 and the lifetime is fixed at
//! 30 days: expiry is never refreshed and the token is never rotated.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

/// Session cookie name, matching the Go server exactly.
pub const SESSION_COOKIE: &str = "revaro_session";

/// Fixed session lifetime: 30 days in milliseconds.
pub const SESSION_LIFETIME_MILLIS: i64 = 30 * 24 * 60 * 60 * 1000;

/// The stored form of a session token.
#[must_use]
pub fn token_hash(token: &str) -> String {
    URL_SAFE_NO_PAD.encode(revaro_core::hash::sha256(token.as_bytes()))
}

/// Mint a fresh session token (32 random bytes, base64url without padding).
#[must_use]
pub fn new_token() -> String {
    URL_SAFE_NO_PAD.encode(super::random_bytes(32))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_43_character_url_safe_strings() {
        let token = new_token();
        assert_eq!(token.len(), 43);
        assert!(
            token
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_'),
            "{token}"
        );
        assert_ne!(new_token(), token);
    }

    #[test]
    fn token_hashes_match_go_raw_url_base64_sha256() {
        // sha256("abc"), base64url without padding. Pinned against Python's
        // hashlib/base64 so a change in engine or alphabet is caught.
        assert_eq!(
            token_hash("abc"),
            "ungWv48Bz-pBQUDeXa4iI7ADYaOWF3qctBD_YfIAFa0"
        );
        assert_eq!(token_hash("abc"), token_hash("abc"));
        assert_ne!(token_hash("abc"), token_hash("abd"));
    }

    #[test]
    fn the_lifetime_is_thirty_days() {
        assert_eq!(SESSION_LIFETIME_MILLIS, 2_592_000_000);
    }
}
