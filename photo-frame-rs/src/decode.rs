use eframe::egui::ColorImage;
use image::{ImageDecoder, ImageFormat, ImageReader, Limits};
use std::io::Cursor;
pub const MAX_IMAGE_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_DIMENSION: u32 = 8192;
pub const MAX_PIXELS: u64 = 24_000_000;
pub const MAX_DECODE_ALLOC: u64 = 128 * 1024 * 1024;
pub const UPLOAD_WIDTH: u32 = 1600;
pub const UPLOAD_HEIGHT: u32 = 1200;

pub struct DecodedPhoto {
    pub image: ColorImage,
    pub backdrop: Option<ColorImage>,
    pub aspect: f32,
}
fn color(image: &image::DynamicImage) -> ColorImage {
    let rgba = image.to_rgba8();
    ColorImage::from_rgba_unmultiplied(
        [rgba.width() as usize, rgba.height() as usize],
        rgba.as_raw(),
    )
}
pub fn decode(bytes: &[u8], backdrop: bool, camera: bool) -> Result<DecodedPhoto, &'static str> {
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err("image_bytes_limit");
    }
    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|_| "image_format")?;
    if !matches!(reader.format(), Some(ImageFormat::Jpeg | ImageFormat::Png)) {
        return Err("unsupported_image_format");
    }
    if camera && reader.format() != Some(ImageFormat::Jpeg) {
        return Err("camera_requires_jpeg");
    }
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_DIMENSION);
    limits.max_image_height = Some(MAX_DIMENSION);
    limits.max_alloc = Some(MAX_DECODE_ALLOC);
    reader.limits(limits);
    let mut decoder = reader.into_decoder().map_err(|_| "image_decoder")?;
    let (w, h) = decoder.dimensions();
    if w == 0 || h == 0 || u64::from(w) * u64::from(h) > MAX_PIXELS {
        return Err("image_pixel_limit");
    }
    // Respect EXIF orientation for fully visible native photo rendering.
    let orientation = decoder.orientation().map_err(|_| "image_orientation")?;
    let mut original = image::DynamicImage::from_decoder(decoder).map_err(|_| "image_decode")?;
    original.apply_orientation(orientation);
    let aspect = original.width() as f32 / original.height() as f32;
    let (uw, uh) = if camera {
        (1280, 720)
    } else {
        (UPLOAD_WIDTH, UPLOAD_HEIGHT)
    };
    let resized = original.thumbnail(uw.min(original.width()), uh.min(original.height()));
    drop(original);
    let background = backdrop.then(|| {
        color(
            &resized
                .thumbnail(160.min(resized.width()), 160.min(resized.height()))
                .blur(5.0),
        )
    });
    Ok(DecodedPhoto {
        image: color(&resized),
        backdrop: background,
        aspect,
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    fn png(w: u32, h: u32) -> Vec<u8> {
        let i = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            w,
            h,
            image::Rgb([12, 22, 32]),
        ));
        let mut out = Cursor::new(Vec::new());
        i.write_to(&mut out, ImageFormat::Png).unwrap();
        out.into_inner()
    }
    #[test]
    fn jpeg_photo_and_camera_decode_stay_in_memory() {
        let image = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            16,
            8,
            image::Rgb([20, 30, 40]),
        ));
        let mut bytes = Cursor::new(Vec::new());
        image.write_to(&mut bytes, ImageFormat::Jpeg).unwrap();
        let photo = decode(bytes.get_ref(), true, false).unwrap();
        assert_eq!(photo.aspect, 2.);
        assert_eq!(photo.image.size, [16, 8]);
        assert!(photo.backdrop.is_some());
        let camera = decode(bytes.get_ref(), false, true).unwrap();
        assert_eq!(camera.image.size, [16, 8]);
        assert!(camera.backdrop.is_none());
    }
    #[test]
    fn preserves_aspect_and_upload_limits() {
        let p = decode(&png(2000, 1000), true, false).unwrap();
        assert_eq!(p.aspect, 2.0);
        assert_eq!(p.image.size, [1600, 800]);
        assert!(p.backdrop.unwrap().size.iter().all(|n| *n <= 160));
    }
    #[test]
    fn rejects_invalid_unsupported_and_byte_limit() {
        assert!(decode(b"not an image", false, false).is_err());
        assert!(decode(b"GIF89a", false, false).is_err());
        assert!(decode(&vec![0; MAX_IMAGE_BYTES + 1], false, false).is_err());
        assert!(decode(&png(4, 4), false, true).is_err());
    }
    #[test]
    fn decoder_dimension_and_pixel_limits() {
        // Highly compressed synthetic PNGs stay RAM-only: exceed width or total pixels.
        assert!(decode(&png(MAX_DIMENSION + 1, 1), false, false).is_err());
        assert!(decode(&png(5000, 5000), false, false).is_err());
    }
}
