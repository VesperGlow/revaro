//! Identifier generation.
//!
//! The shared crate owns the *shape* of an identifier ([`revaro_core::ids`]);
//! minting one needs a random source, which is a native concern.

/// A fresh canonical lowercase RFC 4122 version-4 UUID.
///
/// Mirrors the Go `ids.New` helper, including the version and variant bits, so
/// identifiers produced by either server are indistinguishable.
#[must_use]
pub fn new_id() -> String {
    uuid::Uuid::new_v4().hyphenated().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use revaro_core::ids;

    #[test]
    fn identifiers_are_canonical_uuids() {
        for _ in 0..64 {
            let id = new_id();
            assert!(ids::is_canonical_uuid(&id), "{id} is not a canonical UUID");
            assert_eq!(id.len(), ids::ID_LEN);
            // Version 4, variant 0b10xx.
            assert_eq!(id.as_bytes()[14], b'4');
            assert!(matches!(id.as_bytes()[19], b'8' | b'9' | b'a' | b'b'));
        }
    }

    #[test]
    fn identifiers_do_not_collide() {
        let mut seen = std::collections::HashSet::new();
        for _ in 0..10_000 {
            assert!(seen.insert(new_id()), "duplicate identifier generated");
        }
    }
}
