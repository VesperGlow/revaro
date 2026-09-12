//! Argon2id password hashing in Go's exact encoded format.
//!
//! The wire/storage format is a hard compatibility requirement: hashes already
//! in the `settings` table were produced by `golang.org/x/crypto/argon2` and
//! must keep verifying. The encoded string is
//!
//! ```text
//! $argon2id$v=19$m=65536,t=3,p=2$<salt>$<key>
//! ```
//!
//! where `<salt>` and `<key>` are **unpadded** standard-alphabet base64 (Go's
//! `base64.RawStdEncoding`).
//!
//! ## Why the PHC parser is not used
//!
//! The `argon2` crate's `PasswordHash` parser *does* accept this shape — PHC's
//! `B64` is the same unpadded standard alphabet — and a test below pins that
//! down. Parsing and emitting the string here anyway keeps the resource bounds
//! and the error semantics byte-for-byte identical to Go's `VerifyPassword`:
//! a 1024-character cap, `m in 1024..=262144`, `t in 1..=10`, `p in 1..=8`,
//! salt `8..=64` bytes and key `16..=64` bytes, all checked before any memory
//! is allocated for the KDF. The raw [`argon2::Argon2::hash_password_into`]
//! primitive is still used for the derivation itself.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD_NO_PAD;

use super::AuthError;

/// An Argon2id parameter set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Params {
    /// Memory cost in KiB.
    pub memory: u32,
    /// Time cost (passes).
    pub iterations: u32,
    /// Lanes.
    pub parallelism: u8,
    /// Salt length in bytes.
    pub salt_length: u32,
    /// Derived key length in bytes.
    pub key_length: u32,
}

/// The product default: `m=65536 KiB, t=3, p=2, salt=16 B, key=32 B`.
pub const DEFAULT_PARAMS: Params = Params {
    memory: 64 * 1024,
    iterations: 3,
    parallelism: 2,
    salt_length: 16,
    key_length: 32,
};

/// Smallest accepted memory cost, in KiB.
const MIN_MEMORY: u32 = 1024;
/// Largest accepted memory cost, in KiB.
const MAX_MEMORY: u32 = 256 * 1024;
/// Largest accepted time cost.
const MAX_ITERATIONS: u32 = 10;
/// Largest accepted lane count.
const MAX_PARALLELISM: u8 = 8;
/// Smallest accepted salt length, in bytes.
const MIN_SALT_LENGTH: u32 = 8;
/// Largest accepted salt length, in bytes.
const MAX_SALT_LENGTH: u32 = 64;
/// Smallest accepted derived key length, in bytes.
const MIN_KEY_LENGTH: u32 = 16;
/// Largest accepted derived key length, in bytes.
const MAX_KEY_LENGTH: u32 = 64;
/// Largest accepted encoded hash, matching Go's `len(encoded) > 1024` guard.
const MAX_ENCODED_LENGTH: usize = 1024;

/// The Argon2 version number both implementations encode (`v=19`).
const ARGON2_VERSION: i32 = 19;

/// Reject parameter sets that a corrupted or hand-edited row could use to make
/// login allocate arbitrary memory or burn arbitrary CPU.
///
/// # Errors
/// Returns [`AuthError::Corrupt`] when any bound is violated.
pub fn validate_params(params: Params) -> Result<(), AuthError> {
    let valid = (MIN_MEMORY..=MAX_MEMORY).contains(&params.memory)
        && (1..=MAX_ITERATIONS).contains(&params.iterations)
        && (1..=MAX_PARALLELISM).contains(&params.parallelism)
        && (MIN_SALT_LENGTH..=MAX_SALT_LENGTH).contains(&params.salt_length)
        && (MIN_KEY_LENGTH..=MAX_KEY_LENGTH).contains(&params.key_length);
    if valid {
        Ok(())
    } else {
        Err(AuthError::Corrupt(
            "argon2 parameters are outside supported limits".to_owned(),
        ))
    }
}

/// Hash `password` with a fresh random salt, returning Go's encoded form.
///
/// # Errors
/// Returns [`AuthError::Corrupt`] for an out-of-bounds parameter set.
pub fn hash_password(password: &str, params: Params) -> Result<String, AuthError> {
    validate_params(params)?;
    let salt = super::random_bytes(params.salt_length as usize);
    encode(password, &salt, params)
}

/// Derive with a caller-supplied salt and encode the result. Used by tests to
/// pin the exact bytes of the format.
fn encode(password: &str, salt: &[u8], params: Params) -> Result<String, AuthError> {
    let key = derive_raw(
        password,
        salt,
        params.memory,
        params.iterations,
        params.parallelism,
        params.key_length as usize,
    )?;
    Ok(format!(
        "$argon2id$v={ARGON2_VERSION}$m={},t={},p={}${}${}",
        params.memory,
        params.iterations,
        params.parallelism,
        STANDARD_NO_PAD.encode(salt),
        STANDARD_NO_PAD.encode(&key),
    ))
}

/// Verify `password` against an encoded Go-format hash.
///
/// Returns `Ok(true)`/`Ok(false)` for a well-formed hash, and an error when the
/// stored value itself is unusable — which the caller treats as a failed login.
///
/// # Errors
/// Returns [`AuthError::Corrupt`] when the encoded value is malformed or its
/// parameters are outside the supported bounds.
pub fn verify_password(password: &str, encoded: &str) -> Result<bool, AuthError> {
    if encoded.len() > MAX_ENCODED_LENGTH {
        return Err(AuthError::Corrupt("password hash is too long".to_owned()));
    }
    let parts: Vec<&str> = encoded.split('$').collect();
    if parts.len() != 6 || parts[1] != "argon2id" {
        return Err(AuthError::Corrupt("invalid password hash".to_owned()));
    }
    let version = parts[2]
        .strip_prefix("v=")
        .and_then(|value| value.parse::<i32>().ok())
        .ok_or_else(|| AuthError::Corrupt("unsupported argon2 version".to_owned()))?;
    if version != ARGON2_VERSION {
        return Err(AuthError::Corrupt("unsupported argon2 version".to_owned()));
    }

    let fields: Vec<&str> = parts[3].split(',').collect();
    if fields.len() != 3 {
        return Err(AuthError::Corrupt("invalid argon2 parameters".to_owned()));
    }
    let memory = parse_uint(fields[0], "m=")?;
    let iterations = parse_uint(fields[1], "t=")?;
    let parallelism = parse_uint(fields[2], "p=")?;

    let salt = STANDARD_NO_PAD
        .decode(parts[4])
        .map_err(|_| AuthError::Corrupt("invalid password hash".to_owned()))?;
    let expected = STANDARD_NO_PAD
        .decode(parts[5])
        .map_err(|_| AuthError::Corrupt("invalid password hash".to_owned()))?;

    // Go narrows the parsed values to uint32/uint8 before validating; the
    // bounds below are tighter than either type, so a u64 parse is equivalent.
    let params = Params {
        memory: u32::try_from(memory)
            .map_err(|_| AuthError::Corrupt("argon2 parameters are invalid".to_owned()))?,
        iterations: u32::try_from(iterations)
            .map_err(|_| AuthError::Corrupt("argon2 parameters are invalid".to_owned()))?,
        parallelism: u8::try_from(parallelism)
            .map_err(|_| AuthError::Corrupt("argon2 parameters are invalid".to_owned()))?,
        salt_length: salt.len() as u32,
        key_length: expected.len() as u32,
    };
    validate_params(params)?;

    let derived = derive_raw(
        password,
        &salt,
        params.memory,
        params.iterations,
        params.parallelism,
        expected.len(),
    )?;
    Ok(bool::from(subtle::ConstantTimeEq::ct_eq(
        derived.as_slice(),
        expected.as_slice(),
    )))
}

/// Parse one `name=value` field. Go used `strings.TrimPrefix`, which tolerates a
/// missing prefix, so `65536` and `m=65536` are both accepted.
fn parse_uint(field: &str, prefix: &str) -> Result<u64, AuthError> {
    field
        .strip_prefix(prefix)
        .unwrap_or(field)
        .parse::<u64>()
        .map_err(|_| AuthError::Corrupt("invalid argon2 parameters".to_owned()))
}

/// Run the raw Argon2id KDF.
pub(super) fn derive_raw(
    password: &str,
    salt: &[u8],
    memory: u32,
    iterations: u32,
    parallelism: u8,
    key_length: usize,
) -> Result<Vec<u8>, AuthError> {
    use argon2::{Algorithm, Argon2, Version};

    let params = argon2::Params::new(memory, iterations, u32::from(parallelism), Some(key_length))
        .map_err(|error| AuthError::Corrupt(error.to_string()))?;
    let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut key = vec![0u8; key_length];
    argon
        .hash_password_into(password.as_bytes(), salt, &mut key)
        .map_err(|error| AuthError::Corrupt(error.to_string()))?;
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parameters small enough for the test suite, matching Go's `testParams`.
    const TEST_PARAMS: Params = Params {
        memory: 8 * 1024,
        iterations: 1,
        parallelism: 1,
        salt_length: 16,
        key_length: 32,
    };

    /// A hash produced independently by OpenSSL 3.5's `ARGON2ID` KDF for
    /// `correct horse battery staple`, salt `00 01 .. 0f`, and the Go defaults
    /// `m=65536,t=3,p=2`, then encoded with Go's `base64.RawStdEncoding`.
    ///
    /// It is a hard compatibility assertion: if the emitter, the parser or the
    /// KDF drifts, an existing installation can no longer log in.
    const GO_DEFAULT_HASH: &str = "$argon2id$v=19$m=65536,t=3,p=2$AAECAwQFBgcICQoLDA0ODw$wrFQiwrdeIehGpqW8aRyJctAv6BudXD/c6QUGLKhXWE";

    /// The same construction for the test parameters.
    const GO_TEST_HASH: &str = "$argon2id$v=19$m=8192,t=1,p=1$AAECAwQFBgcICQoLDA0ODw$RdqhNlQiI0mATcTJPhZS+/9WuYFCTSbL4C3p+YUXUbo";

    #[test]
    fn default_parameters_match_the_documented_values() {
        assert_eq!(DEFAULT_PARAMS.memory, 65536);
        assert_eq!(DEFAULT_PARAMS.iterations, 3);
        assert_eq!(DEFAULT_PARAMS.parallelism, 2);
        assert_eq!(DEFAULT_PARAMS.salt_length, 16);
        assert_eq!(DEFAULT_PARAMS.key_length, 32);
    }

    #[test]
    fn verifies_hashes_produced_with_the_exact_go_format() {
        assert!(
            verify_password("correct horse battery staple", GO_DEFAULT_HASH).unwrap(),
            "a Go-format hash must verify"
        );
        assert!(!verify_password("wrong password", GO_DEFAULT_HASH).unwrap());
        assert!(verify_password("a-secure-test-password", GO_TEST_HASH).unwrap());
    }

    #[test]
    fn emits_the_exact_go_format_for_a_known_salt() {
        let encoded = encode(
            "correct horse battery staple",
            &(0u8..16).collect::<Vec<u8>>(),
            DEFAULT_PARAMS,
        )
        .unwrap();
        assert_eq!(encoded, GO_DEFAULT_HASH);
    }

    #[test]
    fn hashes_are_salted() {
        let a = hash_password("correct horse battery staple", TEST_PARAMS).unwrap();
        let b = hash_password("correct horse battery staple", TEST_PARAMS).unwrap();
        assert_ne!(a, b, "password hashes must use independent salts");
        assert!(verify_password("correct horse battery staple", &a).unwrap());
        assert!(verify_password("correct horse battery staple", &b).unwrap());
    }

    #[test]
    fn the_phc_parser_accepts_go_unpadded_base64() {
        // Documents *why* parsing is done by hand anyway: the RustCrypto PHC
        // parser understands the same `B64` alphabet Go uses, so a future
        // refactor could switch to it without changing the stored format.
        use argon2::password_hash::{PasswordHash, PasswordVerifier as _};

        let parsed = PasswordHash::new(GO_DEFAULT_HASH).expect("PHC parser accepts the Go string");
        assert_eq!(parsed.algorithm.as_str(), "argon2id");
        assert_eq!(parsed.version, Some(19));
        assert_eq!(parsed.salt.unwrap().as_str(), "AAECAwQFBgcICQoLDA0ODw");
        argon2::Argon2::default()
            .verify_password(b"correct horse battery staple", &parsed)
            .expect("the PHC verifier agrees with our derivation");
    }

    #[test]
    fn rejects_resource_exhaustion_parameters() {
        let salt = STANDARD_NO_PAD.encode([0u8; 16]);
        let key = STANDARD_NO_PAD.encode([0u8; 32]);
        for encoded in [
            format!("$argon2id$v=19$m=4294967295,t=3,p=2${salt}${key}"),
            format!("$argon2id$v=19$m=65536,t=4294967295,p=2${salt}${key}"),
            format!("$argon2id$v=19$m=65536,t=3,p=255${salt}${key}"),
            format!("$argon2id$v=19$m=1023,t=3,p=2${salt}${key}"),
            format!("$argon2id$v=19$m=262145,t=3,p=2${salt}${key}"),
        ] {
            assert!(
                verify_password("password", &encoded).is_err(),
                "unsafe parameters accepted: {encoded}"
            );
        }
        assert!(
            hash_password(
                "password",
                Params {
                    memory: MAX_MEMORY + 1,
                    ..TEST_PARAMS
                }
            )
            .is_err(),
            "unsafe hash parameters accepted"
        );
    }

    #[test]
    fn rejects_malformed_hashes() {
        let mut cases: Vec<String> = [
            "",
            "not-a-hash",
            "$argon2i$v=19$m=65536,t=3,p=2$c2FsdA$AAAAAAAAAAAAAAAAAAAAAA",
            "$argon2id$v=18$m=65536,t=3,p=2$c2FsdA$AAAAAAAAAAAAAAAAAAAAAA",
            "$argon2id$v=19$m=65536,t=3$c2FsdA$AAAAAAAAAAAAAAAAAAAAAA",
            "$argon2id$v=19$m=65536,t=3,p=2$c2FsdA$AAAA",
            "$argon2id$v=19$m=65536,t=3,p=2$not base64!$AAAAAAAAAAAAAAAAAAAAAA",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        // Over the 1024-character cap.
        cases.push(format!(
            "$argon2id$v=19$m=65536,t=3,p=2${}",
            "A".repeat(1100)
        ));
        for encoded in cases {
            assert!(
                verify_password("password", &encoded).is_err(),
                "malformed hash accepted: {encoded}"
            );
        }
    }

    #[test]
    fn rejects_short_salts_and_keys_before_deriving() {
        let short_salt = STANDARD_NO_PAD.encode([0u8; 7]);
        let key = STANDARD_NO_PAD.encode([0u8; 32]);
        assert!(
            verify_password(
                "password",
                &format!("$argon2id$v=19$m=8192,t=1,p=1${short_salt}${key}")
            )
            .is_err()
        );
        let salt = STANDARD_NO_PAD.encode([0u8; 16]);
        let short_key = STANDARD_NO_PAD.encode([0u8; 15]);
        assert!(
            verify_password(
                "password",
                &format!("$argon2id$v=19$m=8192,t=1,p=1${salt}${short_key}")
            )
            .is_err()
        );
    }

    #[test]
    fn tolerates_parameter_fields_without_their_prefix() {
        // Go used `strings.TrimPrefix`, which is a no-op when the prefix is
        // absent, so `8192` parses exactly like `m=8192`.
        let without_prefix = GO_TEST_HASH.replace("m=8192,t=1,p=1", "8192,1,1");
        assert!(verify_password("a-secure-test-password", &without_prefix).unwrap());
    }
}
