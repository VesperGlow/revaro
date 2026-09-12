use std::io::Cursor;

use image::{DynamicImage, ImageReader, codecs::jpeg::JpegEncoder, imageops::FilterType};

use crate::{MAX_IMAGE_PIXELS, MAX_IMAGE_SIDE, MAX_THUMBNAIL_BYTES, MediaError};

/// Resize a supported still image to a JPEG thumbnail.
///
/// Dimensions are checked before decoding pixels.  A compressed image is an
/// untrusted input, so bounding its byte length alone would still allow a
/// decompression bomb to allocate an enormous bitmap.
pub fn resize_image_to_jpeg(data: &[u8], max_dimension: u32) -> Result<Vec<u8>, MediaError> {
    if max_dimension == 0 {
        return Err(MediaError::InvalidData(
            "thumbnail dimension must be positive".to_owned(),
        ));
    }

    let reader = ImageReader::new(Cursor::new(data))
        .with_guessed_format()
        .map_err(|error| MediaError::InvalidData(error.to_string()))?;
    let (width, height) = reader
        .into_dimensions()
        .map_err(|error| MediaError::InvalidData(error.to_string()))?;
    if width == 0
        || height == 0
        || u64::from(width) * u64::from(height) > MAX_IMAGE_PIXELS
        || width > MAX_IMAGE_SIDE
        || height > MAX_IMAGE_SIDE
    {
        return Err(MediaError::InvalidData(
            "image exceeds pixel dimensions".to_owned(),
        ));
    }

    let source = image::load_from_memory(data)
        .map_err(|error| MediaError::InvalidData(error.to_string()))?
        .to_rgba8();
    let (target_width, target_height) = fit(width, height, max_dimension);
    let pixels = if (target_width, target_height) == (width, height) {
        source
    } else {
        image::imageops::resize(&source, target_width, target_height, FilterType::CatmullRom)
    };
    let rgb = DynamicImage::ImageRgba8(pixels).to_rgb8();

    let mut jpeg =
        Vec::with_capacity((target_width as usize * target_height as usize / 2).max(1024));
    JpegEncoder::new_with_quality(&mut jpeg, 82)
        .encode(
            rgb.as_raw(),
            target_width,
            target_height,
            image::ExtendedColorType::Rgb8,
        )
        .map_err(|error| MediaError::InvalidData(error.to_string()))?;
    if jpeg.len() > MAX_THUMBNAIL_BYTES {
        return Err(MediaError::InvalidData(
            "thumbnail exceeds output limit".to_owned(),
        ));
    }
    Ok(jpeg)
}

fn fit(width: u32, height: u32, max_dimension: u32) -> (u32, u32) {
    if width <= max_dimension && height <= max_dimension {
        return (width, height);
    }
    if width >= height {
        (
            max_dimension,
            ((u64::from(height) * u64::from(max_dimension) / u64::from(width)).max(1)) as u32,
        )
    } else {
        (
            ((u64::from(width) * u64::from(max_dimension) / u64::from(height)).max(1)) as u32,
            max_dimension,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::GenericImageView as _;

    fn png(width: u32, height: u32) -> Vec<u8> {
        let image = image::RgbaImage::from_pixel(width, height, image::Rgba([20, 40, 80, 255]));
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgba8(image)
            .write_to(&mut Cursor::new(&mut bytes), image::ImageFormat::Png)
            .unwrap();
        bytes
    }

    #[test]
    fn resizes_and_emits_a_bounded_jpeg() {
        let jpeg = resize_image_to_jpeg(&png(800, 400), 640).unwrap();
        assert_eq!(&jpeg[..2], &[0xff, 0xd8]);
        let decoded = image::load_from_memory(&jpeg).unwrap();
        assert_eq!(decoded.dimensions(), (640, 320));
    }

    #[test]
    fn rejects_zero_and_decompression_bomb_dimensions() {
        assert!(resize_image_to_jpeg(&png(1, 1), 0).is_err());
        // The encoded fixture is tiny but its declared dimensions exceed the
        // limit before the decoder is allowed to allocate pixels.
        let mut header = png(1, 1);
        // PNG dimensions are the two big-endian u32 fields after the IHDR
        // length and type.  The CRC is irrelevant to `into_dimensions`.
        header[16..20].copy_from_slice(&(MAX_IMAGE_SIDE + 1).to_be_bytes());
        assert!(resize_image_to_jpeg(&header, 640).is_err());
    }
}
