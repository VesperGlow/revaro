//! RFC 4648 base32 without padding.
//!
//! Go's `encoding/base32` with `base32.NoPadding` is what the product uses for
//! TOTP secrets (20 random bytes → 32 characters) and recovery codes (10 random
//! bytes → 16 characters). Implementing it here keeps the derivation explicit:
//! the alphabet, the absence of padding and the rejection of malformed input are
//! all part of the stored format.
//!
//! Decoding is case-insensitive, matching Go's decoder, and `=` padding is
//! rejected because the product never emits it.

use super::AuthError;

/// The base32 alphabet.
const ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

/// Encode `data` as base32 without padding.
#[must_use]
pub(super) fn encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(5) * 8);
    for chunk in data.chunks(5) {
        let mut buffer = [0u8; 5];
        buffer[..chunk.len()].copy_from_slice(chunk);
        let bits = u64::from(buffer[0]) << 32
            | u64::from(buffer[1]) << 24
            | u64::from(buffer[2]) << 16
            | u64::from(buffer[3]) << 8
            | u64::from(buffer[4]);
        // 8 output characters per 5 input bytes; the remainder is dropped, which
        // is what "without padding" means.
        let characters = chunk.len() * 8 / 5 + usize::from(chunk.len() % 5 != 0);
        for index in 0..characters {
            let shift = 35 - index * 5;
            out.push(char::from(ALPHABET[(bits >> shift) as usize & 0x1f]));
        }
    }
    out
}

/// Decode unpadded base32.
///
/// # Errors
/// Returns [`AuthError::Corrupt`] for any character outside the alphabet, for
/// `=` padding, or for a length that cannot encode whole bytes.
pub(super) fn decode(input: &str) -> Result<Vec<u8>, AuthError> {
    let mut output = Vec::with_capacity(input.len() * 5 / 8);
    let mut buffer: u64 = 0;
    let mut bits: u32 = 0;
    for character in input.chars() {
        let upper = character.to_ascii_uppercase();
        if upper == '=' {
            return Err(AuthError::Corrupt(
                "invalid base32 padding in secret".to_owned(),
            ));
        }
        let value = ALPHABET
            .iter()
            .position(|candidate| char::from(*candidate) == upper)
            .ok_or_else(|| AuthError::Corrupt("invalid base32 character in secret".to_owned()))?
            as u64;
        buffer = (buffer << 5) | value;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            output.push((buffer >> bits) as u8);
            buffer &= (1 << bits) - 1;
        }
    }
    // Leftover bits must be zero padding in the final character; anything else
    // is a non-canonical encoding.
    if bits >= 5 || buffer != 0 {
        return Err(AuthError::Corrupt(
            "invalid base32 length in secret".to_owned(),
        ));
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_the_rfc_4648_vectors() {
        // The standard vectors, with padding stripped.
        let cases: &[(&[u8], &str)] = &[
            (b"", ""),
            (b"f", "MY"),
            (b"fo", "MZXQ"),
            (b"foo", "MZXW6"),
            (b"foob", "MZXW6YQ"),
            (b"fooba", "MZXW6YTB"),
            (b"foobar", "MZXW6YTBOI"),
        ];
        for (input, expected) in cases {
            assert_eq!(encode(input), *expected, "{input:?}");
            assert_eq!(decode(expected).unwrap(), *input, "{expected}");
        }
    }

    #[test]
    fn round_trips_the_product_shapes() {
        // 20-byte TOTP secret → 32 characters; 10-byte recovery entropy → 16.
        assert_eq!(encode(&[0u8; 20]).len(), 32);
        assert_eq!(encode(&[0xffu8; 10]).len(), 16);
        let secret = super::super::random_bytes(20);
        assert_eq!(decode(&encode(&secret)).unwrap(), secret);
    }

    #[test]
    fn decoding_is_case_insensitive_and_rejects_padding() {
        assert_eq!(decode("mzxw6ytboi").unwrap(), b"foobar");
        assert!(decode("MZXW6YTB=").is_err());
        assert!(decode("MZXW6YTB!").is_err());
        // A single trailing character carries fewer than 5 bits of information.
        assert!(decode("M").is_err());
    }
}
