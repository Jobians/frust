//! Pixel-to-tensor helpers for vision models: RGBA8 pixels to a normalised `[1, 3, H, W]` f32
//! tensor, plus bilinear resizing and min-max normalisation of single-channel planes.
//!
//! Everything here is single-threaded and uses no dependency. Resizing samples at pixel centres
//! (the half-pixel convention) and clamps at the borders, so resizing to the same size copies.

use crate::{OnnxError, Tensor};

/// A borrowed, validated RGBA8 image: four bytes per pixel, rows top to bottom, no padding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RgbaView<'a> {
    width: u32,
    height: u32,
    pixels: &'a [u8],
}

impl<'a> RgbaView<'a> {
    /// A view over `pixels`. Fails when a side is zero or `pixels` is not exactly
    /// `width * height * 4` bytes.
    pub fn new(width: u32, height: u32, pixels: &'a [u8]) -> Result<Self, OnnxError> {
        if width == 0 || height == 0 {
            return Err(OnnxError::InvalidInput(format!(
                "image size {width}x{height} has a zero side"
            )));
        }
        let expected = (width as usize)
            .checked_mul(height as usize)
            .and_then(|n| n.checked_mul(4));
        if expected != Some(pixels.len()) {
            return Err(OnnxError::InvalidInput(format!(
                "a {width}x{height} RGBA image needs {} bytes, got {}",
                expected.map_or_else(|| "too many".to_owned(), |n| n.to_string()),
                pixels.len()
            )));
        }
        Ok(Self {
            width,
            height,
            pixels,
        })
    }

    /// Width in pixels.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Height in pixels.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// The RGBA bytes.
    pub fn pixels(&self) -> &'a [u8] {
        self.pixels
    }
}

/// How a colour byte becomes a model input: `(byte * scale - mean) / std`, per channel in the
/// order the image is emitted (see [`ChannelOrder`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Normalization {
    /// Multiplier applied to the 0..=255 byte first: `1.0 / 255.0` for 0..1 inputs, `1.0` to keep
    /// the raw byte value.
    pub scale: f32,
    /// Subtracted after scaling, per output channel.
    pub mean: [f32; 3],
    /// Divided by after the subtraction, per output channel.
    pub std: [f32; 3],
}

impl Normalization {
    /// ImageNet statistics on 0..1 values: mean `[0.485, 0.456, 0.406]`, std
    /// `[0.229, 0.224, 0.225]`.
    pub const IMAGENET: Self = Self {
        scale: 1.0 / 255.0,
        mean: [0.485, 0.456, 0.406],
        std: [0.229, 0.224, 0.225],
    };
    /// Plain 0..1 values.
    pub const UNIT: Self = Self {
        scale: 1.0 / 255.0,
        mean: [0.0; 3],
        std: [1.0; 3],
    };
    /// The byte values as floats, 0..=255, with no scaling.
    pub const RAW_255: Self = Self {
        scale: 1.0,
        mean: [0.0; 3],
        std: [1.0; 3],
    };
}

/// Which colour channel comes first in the output tensor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChannelOrder {
    /// Red, green, blue.
    Rgb,
    /// Blue, green, red.
    Bgr,
}

/// One output index's two source indices and the weight of the second.
fn axis_taps(src: usize, dst: usize) -> Vec<(usize, usize, f32)> {
    let ratio = src as f32 / dst as f32;
    (0..dst)
        .map(|i| {
            let pos = ((i as f32 + 0.5) * ratio - 0.5).clamp(0.0, (src - 1) as f32);
            let lo = pos.floor() as usize;
            let hi = (lo + 1).min(src - 1);
            (lo, hi, pos - lo as f32)
        })
        .collect()
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

fn check_target(out_w: u32, out_h: u32) -> Result<(), OnnxError> {
    if out_w == 0 || out_h == 0 {
        return Err(OnnxError::InvalidInput(format!(
            "output size {out_w}x{out_h} has a zero side"
        )));
    }
    Ok(())
}

/// Resizes `view` to `out_w` x `out_h` bilinearly and returns it as an f32 tensor of shape
/// `[1, 3, out_h, out_w]`, channels in `order`, each value `(byte * scale - mean) / std`.
/// Alpha is ignored.
pub fn to_nchw_f32(
    view: RgbaView<'_>,
    out_w: u32,
    out_h: u32,
    norm: Normalization,
    order: ChannelOrder,
) -> Result<Tensor, OnnxError> {
    check_target(out_w, out_h)?;
    let (sw, sh) = (view.width as usize, view.height as usize);
    let (dw, dh) = (out_w as usize, out_h as usize);
    let plane = dw * dh;
    let mut out = vec![0.0f32; plane * 3];
    let xs = axis_taps(sw, dw);
    let ys = axis_taps(sh, dh);
    let src_channel = match order {
        ChannelOrder::Rgb => [0usize, 1, 2],
        ChannelOrder::Bgr => [2, 1, 0],
    };
    for (y, &(y0, y1, wy)) in ys.iter().enumerate() {
        let row0 = &view.pixels[y0 * sw * 4..(y0 + 1) * sw * 4];
        let row1 = &view.pixels[y1 * sw * 4..(y1 + 1) * sw * 4];
        for (x, &(x0, x1, wx)) in xs.iter().enumerate() {
            for (c, &sc) in src_channel.iter().enumerate() {
                let top = lerp(row0[x0 * 4 + sc] as f32, row0[x1 * 4 + sc] as f32, wx);
                let bottom = lerp(row1[x0 * 4 + sc] as f32, row1[x1 * 4 + sc] as f32, wx);
                let value = lerp(top, bottom, wy);
                out[c * plane + y * dw + x] = (value * norm.scale - norm.mean[c]) / norm.std[c];
            }
        }
    }
    Tensor::from_f32([1, 3, dh, dw], out)
}

/// Bilinear resize of a single-channel plane stored row-major, `sw` x `sh` to `dw` x `dh`.
/// Returns an empty vector when any size is zero or `src` is not `sw * sh` long.
pub fn resize_plane(src: &[f32], sw: usize, sh: usize, dw: usize, dh: usize) -> Vec<f32> {
    if sw == 0 || sh == 0 || dw == 0 || dh == 0 || sw.checked_mul(sh) != Some(src.len()) {
        return Vec::new();
    }
    let xs = axis_taps(sw, dw);
    let mut out = Vec::with_capacity(dw * dh);
    for (y0, y1, wy) in axis_taps(sh, dh) {
        let row0 = &src[y0 * sw..(y0 + 1) * sw];
        let row1 = &src[y1 * sw..(y1 + 1) * sw];
        for &(x0, x1, wx) in &xs {
            out.push(lerp(
                lerp(row0[x0], row0[x1], wx),
                lerp(row1[x0], row1[x1], wx),
                wy,
            ));
        }
    }
    out
}

/// Rescales `plane` in place so its smallest finite value becomes 0 and its largest 1. A plane
/// with no spread (constant, empty or without finite values) becomes all zeros, as does any
/// non-finite element.
pub fn normalize_plane(plane: &mut [f32]) {
    let (mut min, mut max) = (f32::INFINITY, f32::NEG_INFINITY);
    for &v in plane.iter().filter(|v| v.is_finite()) {
        min = min.min(v);
        max = max.max(v);
    }
    let span = max - min;
    if !(span.is_finite() && span > 0.0) {
        plane.fill(0.0);
        return;
    }
    for v in plane {
        *v = if v.is_finite() {
            (*v - min) / span
        } else {
            0.0
        };
    }
}

/// A single-channel f32 image, for example a mask or a depth map.
#[derive(Debug, Clone, PartialEq)]
pub struct Plane {
    /// Width in samples.
    pub width: usize,
    /// Height in samples.
    pub height: usize,
    /// `width * height` samples, row-major.
    pub data: Vec<f32>,
}

impl Plane {
    /// A plane over `data`; fails when `data.len()` is not `width * height`.
    pub fn new(width: usize, height: usize, data: Vec<f32>) -> Result<Self, OnnxError> {
        if width.checked_mul(height) != Some(data.len()) {
            return Err(OnnxError::InvalidInput(format!(
                "a {width}x{height} plane needs {} samples, got {}",
                width.saturating_mul(height),
                data.len()
            )));
        }
        Ok(Self {
            width,
            height,
            data,
        })
    }

    /// Display bytes: every sample clamped to 0..1 and mapped to a grey RGBA pixel, opaque.
    /// Samples that are not finite show as black.
    pub fn to_rgba8_gray(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.data.len() * 4);
        for &v in &self.data {
            let g = if v.is_finite() {
                (v.clamp(0.0, 1.0) * 255.0).round() as u8
            } else {
                0
            };
            out.extend_from_slice(&[g, g, g, 255]);
        }
        out
    }
}
