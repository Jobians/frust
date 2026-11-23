//! Pixel work: decoding the sample and picked photos, the 2048 px cap, and turning results into
//! overlay images. Everything here is blocking and runs inside `frust::spawn_blocking`.

use std::path::Path;
use std::sync::Arc;

use frust::ImageSource;
use frust_onnx::Plane;
use frust_photo_library::{DecodeOptions, MediaFilter, PhotoError, PhotoLibrary, PickOptions};

/// The bundled sample, made by `assets/make_sample.py` (see the README).
pub const SAMPLE_PNG: &[u8] = include_bytes!("../assets/sample.png");

/// Photos are downscaled to at most this many pixels on the long side before inference.
pub const MAX_SIDE: u32 = 2048;

/// Overlays are drawn from images at most this large; they are stretched over the photo.
pub const OVERLAY_SIDE: u32 = 1024;

/// The mask tint (a Material purple) and its strongest alpha.
pub const MASK_TINT: [u8; 3] = [0x67, 0x50, 0xA4];
const MASK_ALPHA: f32 = 170.0;

/// An RGBA8 image ready for inference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rgba {
    /// Straight-alpha RGBA8, rows tightly packed.
    pub pixels: Vec<u8>,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}

impl Rgba {
    /// The pixels as the shared buffer `VisionHandle` takes, and a display image of them.
    pub fn into_parts(self) -> (Arc<[u8]>, ImageSource, u32, u32) {
        let image = ImageSource::from_rgba8(self.pixels.clone(), self.width, self.height);
        (self.pixels.into(), image, self.width, self.height)
    }
}

/// Scales `rgba` down so neither side exceeds `max_side`, averaging each output pixel's source
/// area (a box filter). An image already small enough comes back unchanged.
pub fn fit_within(rgba: Rgba, max_side: u32) -> Rgba {
    let Rgba {
        pixels,
        width,
        height,
    } = rgba;
    let long = width.max(height);
    if long <= max_side || width == 0 || height == 0 {
        return Rgba {
            pixels,
            width,
            height,
        };
    }
    let scale = f64::from(max_side) / f64::from(long);
    let out_w = ((f64::from(width) * scale).round() as u32).max(1);
    let out_h = ((f64::from(height) * scale).round() as u32).max(1);
    let (w, h) = (width as usize, height as usize);
    let mut out = Vec::with_capacity(out_w as usize * out_h as usize * 4);
    for oy in 0..out_h as usize {
        let y0 = oy * h / out_h as usize;
        let y1 = ((oy + 1) * h / out_h as usize).max(y0 + 1);
        for ox in 0..out_w as usize {
            let x0 = ox * w / out_w as usize;
            let x1 = ((ox + 1) * w / out_w as usize).max(x0 + 1);
            let mut sum = [0u32; 4];
            for y in y0..y1 {
                let row = &pixels[(y * w + x0) * 4..(y * w + x1) * 4];
                for px in row.as_chunks::<4>().0 {
                    for (s, &c) in sum.iter_mut().zip(px) {
                        *s += u32::from(c);
                    }
                }
            }
            let n = ((y1 - y0) * (x1 - x0)) as u32;
            out.extend(sum.iter().map(|s| ((s + n / 2) / n) as u8));
        }
    }
    Rgba {
        pixels: out,
        width: out_w,
        height: out_h,
    }
}

/// Decodes PNG or JPEG bytes and caps the result at [`MAX_SIDE`].
pub fn decode_bytes(bytes: &[u8]) -> Result<Rgba, String> {
    let image = ImageSource::decode(bytes).map_err(|err| err.to_string())?;
    let data = image.image_data();
    let rgba = Rgba {
        pixels: data.data.data().to_vec(),
        width: data.width,
        height: data.height,
    };
    Ok(fit_within(rgba, MAX_SIDE))
}

/// The bundled sample, decoded.
pub fn load_sample() -> Result<Rgba, String> {
    decode_bytes(SAMPLE_PNG)
}

/// A photo picked through the system picker (a file dialog on desktop), decoded and capped,
/// with a display name; `None` when the user closed the picker without choosing.
pub fn pick_photo() -> Result<Option<(Rgba, String)>, String> {
    let options = PickOptions::new().filter(MediaFilter::Images).limit(1);
    let picked = PhotoLibrary::pick(&options).map_err(|err| err.to_string())?;
    let Some(item) = picked.into_iter().next() else {
        return Ok(None);
    };
    let name = item.file_name.unwrap_or_else(|| "Picked photo".to_owned());
    decode_file(&item.path).map(|rgba| Some((rgba, name)))
}

/// Decodes a file natively (HEIC included) at most [`MAX_SIDE`] on its long side; where the
/// platform has no native decoder (Linux, Windows) it reads the file as PNG or JPEG.
pub fn decode_file(path: &Path) -> Result<Rgba, String> {
    match PhotoLibrary::decode_file(path, &DecodeOptions::new(MAX_SIDE).srgb()) {
        Ok(image) => Ok(Rgba {
            pixels: image.rgba,
            width: image.width,
            height: image.height,
        }),
        Err(PhotoError::Unsupported | PhotoError::FeatureDisabled { .. }) => {
            let bytes = std::fs::read(path).map_err(|err| err.to_string())?;
            decode_bytes(&bytes)
        }
        Err(err) => Err(err.to_string()),
    }
}

/// The subject mask as a tinted layer: [`MASK_TINT`] whose alpha follows the mask.
pub fn mask_overlay(mask: &Plane) -> Rgba {
    let mut pixels = Vec::with_capacity(mask.data.len() * 4);
    for &v in &mask.data {
        let alpha = (v.clamp(0.0, 1.0) * MASK_ALPHA).round() as u8;
        pixels.extend_from_slice(&[MASK_TINT[0], MASK_TINT[1], MASK_TINT[2], alpha]);
    }
    fit_within(
        Rgba {
            pixels,
            width: mask.width as u32,
            height: mask.height as u32,
        },
        OVERLAY_SIDE,
    )
}

/// The depth map as an opaque grayscale image, near is white.
pub fn depth_image(depth: &Plane) -> Rgba {
    fit_within(
        Rgba {
            pixels: depth.to_rgba8_gray(),
            width: depth.width as u32,
            height: depth.height as u32,
        },
        OVERLAY_SIDE,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(width: u32, height: u32, px: [u8; 4]) -> Rgba {
        Rgba {
            pixels: px.repeat((width * height) as usize),
            width,
            height,
        }
    }

    #[test]
    fn small_images_are_left_alone() {
        let image = solid(4, 3, [1, 2, 3, 4]);
        assert_eq!(fit_within(image.clone(), 4), image);
    }

    #[test]
    fn large_images_keep_their_aspect_and_colour() {
        let out = fit_within(solid(4000, 3000, [10, 20, 30, 255]), MAX_SIDE);
        assert_eq!((out.width, out.height), (2048, 1536));
        assert_eq!(out.pixels.len(), 2048 * 1536 * 4);
        assert!(
            out.pixels
                .as_chunks::<4>()
                .0
                .iter()
                .all(|px| *px == [10, 20, 30, 255])
        );
    }

    #[test]
    fn downscaling_averages_the_source_area() {
        // A 2x1 image of black and white becomes one mid-grey pixel.
        let image = Rgba {
            pixels: vec![0, 0, 0, 255, 255, 255, 255, 255],
            width: 2,
            height: 1,
        };
        let out = fit_within(image, 1);
        assert_eq!((out.width, out.height), (1, 1));
        assert_eq!(out.pixels, vec![128, 128, 128, 255]);
    }

    #[test]
    fn the_sample_decodes_small_and_opaque() {
        let sample = load_sample().expect("the bundled sample decodes");
        assert_eq!((sample.width, sample.height), (640, 480));
        assert!(
            sample
                .pixels
                .as_chunks::<4>()
                .0
                .iter()
                .all(|px| px[3] == 255)
        );
        assert!(SAMPLE_PNG.len() < 300 * 1024);
    }

    #[test]
    fn the_mask_becomes_tinted_alpha() {
        let mask = Plane::new(2, 1, vec![0.0, 1.0]).expect("plane");
        let out = mask_overlay(&mask);
        assert_eq!(
            out.pixels,
            vec![0x67, 0x50, 0xA4, 0, 0x67, 0x50, 0xA4, MASK_ALPHA as u8]
        );
    }
}
