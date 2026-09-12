//! Identifier rules.
//!
//! Revaro uses canonical lowercase RFC 4122 version-4 UUIDs as primary keys.
//! Generating them needs randomness and therefore lives in `revaro-server`;
//! the shared crate owns the *shape* so both ends agree on what a valid
//! identifier is and can reject malformed path parameters before touching the
//! database.

/// The identifier of the virtual root directory.
///
/// The root row is inserted by the initial migration and is never a real
/// directory on disk.
pub const ROOT_ID: &str = "00000000-0000-0000-0000-000000000000";

/// Length of a canonical hyphenated UUID.
pub const ID_LEN: usize = 36;

/// True when `value` is a canonical hyphenated UUID in any letter case.
#[must_use]
pub fn is_uuid(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != ID_LEN {
        return false;
    }
    for (index, byte) in bytes.iter().enumerate() {
        match index {
            8 | 13 | 18 | 23 => {
                if *byte != b'-' {
                    return false;
                }
            }
            _ => {
                if !byte.is_ascii_hexdigit() {
                    return false;
                }
            }
        }
    }
    true
}

/// True when `value` is already the canonical lowercase form.
#[must_use]
pub fn is_canonical_uuid(value: &str) -> bool {
    is_uuid(value) && !value.bytes().any(|byte| byte.is_ascii_uppercase())
}

/// Normalize a UUID to its canonical lowercase form.
///
/// Returns `None` when the input is not a UUID at all.
#[must_use]
pub fn normalize_uuid(value: &str) -> Option<String> {
    if !is_uuid(value) {
        return None;
    }
    if is_canonical_uuid(value) {
        return Some(value.to_owned());
    }
    Some(value.to_ascii_lowercase())
}

/// True when `value` names the virtual root directory.
#[must_use]
pub fn is_root(value: &str) -> bool {
    value.eq_ignore_ascii_case(ROOT_ID)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_canonical_identifiers() {
        assert!(is_uuid(ROOT_ID));
        assert!(is_canonical_uuid(ROOT_ID));
        assert!(is_uuid("0190f8f0-1c2b-7c3d-9e4f-5a6b7c8d9e0f"));
    }

    #[test]
    fn rejects_malformed_identifiers() {
        for value in [
            "",
            "not-a-uuid",
            "0190f8f01c2b7c3d9e4f5a6b7c8d9e0f",
            "0190f8f0-1c2b-7c3d-9e4f-5a6b7c8d9e0",
            "0190f8f0-1c2b-7c3d-9e4f-5a6b7c8d9e0ff",
            "0190f8f0_1c2b_7c3d_9e4f_5a6b7c8d9e0f",
            "zzzzzzzz-1c2b-7c3d-9e4f-5a6b7c8d9e0f",
        ] {
            assert!(!is_uuid(value), "{value:?} must not be a UUID");
        }
    }

    #[test]
    fn normalizes_letter_case() {
        assert_eq!(
            normalize_uuid("0190F8F0-1C2B-7C3D-9E4F-5A6B7C8D9E0F").as_deref(),
            Some("0190f8f0-1c2b-7c3d-9e4f-5a6b7c8d9e0f")
        );
        assert_eq!(normalize_uuid("nope"), None);
    }

    #[test]
    fn recognizes_the_root_identifier() {
        assert!(is_root(ROOT_ID));
        assert!(is_root("00000000-0000-0000-0000-000000000000"));
        assert!(!is_root("0190f8f0-1c2b-7c3d-9e4f-5a6b7c8d9e0f"));
    }
}
