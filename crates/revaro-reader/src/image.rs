//! Intrinsic image dimensions, sniffed from the bytes.
//!
//! The reader needs width/height to reserve layout space before an image loads
//! (otherwise every page reflows when a picture decodes). Pulling in an image
//! decoder for this would be wasteful, so the four formats an EPUB actually
//! ships — PNG, GIF, JPEG and WebP — are read directly from their headers.
//! Anything unrecognised returns `(0, 0, false)` and is emitted without
//! dimensions.

/// Read a big-endian `u16` from a 2-byte slice.
fn be_u16(bytes: &[u8]) -> u32 {
    u32::from(u16::from_be_bytes([bytes[0], bytes[1]]))
}

/// Read a big-endian `u32` from a 4-byte slice.
fn be_u32(bytes: &[u8]) -> u32 {
    u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

/// Read a little-endian `u16` from a 2-byte slice.
fn le_u16(bytes: &[u8]) -> u32 {
    u32::from(u16::from_le_bytes([bytes[0], bytes[1]]))
}

/// Read a little-endian `u32` from a 4-byte slice.
fn le_u32(bytes: &[u8]) -> u32 {
    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

/// Sniff `(width, height, recognised)` from PNG, GIF, JPEG or WebP bytes.
///
/// Every slice access is bounds-checked up front; a truncated header simply
/// reports "not recognised" instead of panicking.
#[must_use]
pub(crate) fn image_dims(data: &[u8]) -> (u32, u32, bool) {
    if data.starts_with(b"\x89PNG\r\n\x1a\n") && data.len() >= 24 {
        return (be_u32(&data[16..20]), be_u32(&data[20..24]), true);
    }
    if data.len() >= 10 && (data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a")) {
        return (le_u16(&data[6..8]), le_u16(&data[8..10]), true);
    }
    if data.len() >= 4 && data[0] == 0xFF && data[1] == 0xD8 {
        let mut index = 2usize;
        while index + 9 < data.len() {
            if data[index] != 0xFF {
                index += 1;
                continue;
            }
            let marker = data[index + 1];
            // Start-of-frame markers carry the dimensions; C4/C8/CC are DHT,
            // JPG and DAC, which share the 0xC0-0xCF range but are not frames.
            if (0xC0..=0xCF).contains(&marker) && !matches!(marker, 0xC4 | 0xC8 | 0xCC) {
                return (
                    be_u16(&data[index + 7..index + 9]),
                    be_u16(&data[index + 5..index + 7]),
                    true,
                );
            }
            let segment_len = be_u16(&data[index + 2..index + 4]) as usize;
            index += 2 + segment_len;
        }
    }
    if data.len() >= 30 && data.starts_with(b"RIFF") && &data[8..12] == b"WEBP" {
        match &data[12..16] {
            b"VP8X" => {
                let width =
                    u32::from(data[24]) | u32::from(data[25]) << 8 | u32::from(data[26]) << 16;
                let height =
                    u32::from(data[27]) | u32::from(data[28]) << 8 | u32::from(data[29]) << 16;
                return (width + 1, height + 1, true);
            }
            b"VP8 " => {
                return (
                    le_u16(&data[26..28]) & 0x3FFF,
                    le_u16(&data[28..30]) & 0x3FFF,
                    true,
                );
            }
            b"VP8L" => {
                let bits = le_u32(&data[21..25]);
                return ((bits & 0x3FFF) + 1, ((bits >> 14) & 0x3FFF) + 1, true);
            }
            _ => {}
        }
    }
    (0, 0, false)
}

/// Build a minimal PNG header with the given dimensions (test helper).
#[cfg(test)]
pub(crate) fn fake_png(width: u32, height: u32) -> Vec<u8> {
    let mut data = vec![0u8; 33];
    data[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
    data[8..12].copy_from_slice(&13u32.to_be_bytes());
    data[12..16].copy_from_slice(b"IHDR");
    data[16..20].copy_from_slice(&width.to_be_bytes());
    data[20..24].copy_from_slice(&height.to_be_bytes());
    data
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_dimensions_are_read_from_ihdr() {
        let (width, height, ok) = image_dims(&fake_png(300, 400));
        assert!(ok);
        assert_eq!((width, height), (300, 400));
    }

    #[test]
    fn gif_dimensions_are_little_endian() {
        let mut gif = b"GIF89a".to_vec();
        gif.extend_from_slice(&[0u8; 6]);
        gif[6..8].copy_from_slice(&64u16.to_le_bytes());
        gif[8..10].copy_from_slice(&32u16.to_le_bytes());
        let (width, height, ok) = image_dims(&gif);
        assert!(ok);
        assert_eq!((width, height), (64, 32));
    }

    #[test]
    fn webp_vp8x_dimensions_are_minus_one_encoded() {
        let mut webp = vec![0u8; 30];
        webp[..4].copy_from_slice(b"RIFF");
        webp[8..12].copy_from_slice(b"WEBP");
        webp[12..16].copy_from_slice(b"VP8X");
        webp[24] = 99;
        webp[27] = 49;
        let (width, height, ok) = image_dims(&webp);
        assert!(ok);
        assert_eq!((width, height), (100, 50));
    }

    #[test]
    fn webp_vp8l_dimensions_are_bit_packed() {
        let mut webp = vec![0u8; 30];
        webp[..4].copy_from_slice(b"RIFF");
        webp[8..12].copy_from_slice(b"WEBP");
        webp[12..16].copy_from_slice(b"VP8L");
        // width-1 = 9, height-1 = 4.
        let bits: u32 = 9 | (4 << 14);
        webp[21..25].copy_from_slice(&bits.to_le_bytes());
        let (width, height, ok) = image_dims(&webp);
        assert!(ok);
        assert_eq!((width, height), (10, 5));
    }

    #[test]
    fn garbage_and_truncated_headers_are_not_images() {
        assert!(!image_dims(b"not an image").2);
        assert!(!image_dims(b"").2);
        // A PNG signature without a complete IHDR must not panic.
        assert!(!image_dims(b"\x89PNG\r\n\x1a\n").2);
    }
}
