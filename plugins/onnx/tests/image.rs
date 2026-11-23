//! Image helpers against hand-computed values.

use frust_onnx::{
    ChannelOrder, Normalization, OnnxError, Plane, RgbaView, normalize_plane, resize_plane,
    to_nchw_f32,
};

fn close(a: &[f32], b: &[f32]) {
    assert_eq!(a.len(), b.len(), "{a:?} vs {b:?}");
    for (x, y) in a.iter().zip(b) {
        assert!((x - y).abs() < 1e-5, "{a:?} vs {b:?}");
    }
}

/// 2x2: red, green / blue, white-ish (alpha varies and must not matter).
const TWO_BY_TWO: [u8; 16] = [
    255, 0, 0, 255, //
    0, 255, 0, 10, //
    0, 0, 255, 255, //
    255, 255, 255, 0,
];

#[test]
fn constructor_refusals() {
    assert!(matches!(
        RgbaView::new(0, 2, &[]),
        Err(OnnxError::InvalidInput(_))
    ));
    assert!(matches!(
        RgbaView::new(2, 0, &[]),
        Err(OnnxError::InvalidInput(_))
    ));
    assert!(RgbaView::new(2, 2, &[0; 15]).is_err());
    assert!(RgbaView::new(2, 2, &[0; 17]).is_err());
    assert!(RgbaView::new(u32::MAX, u32::MAX, &[0; 4]).is_err());
    let view = RgbaView::new(2, 2, &TWO_BY_TWO).unwrap();
    assert_eq!(
        (view.width(), view.height(), view.pixels().len()),
        (2, 2, 16)
    );
    assert!(to_nchw_f32(view, 0, 2, Normalization::UNIT, ChannelOrder::Rgb).is_err());
    assert!(Plane::new(2, 2, vec![0.0; 3]).is_err());
}

#[test]
fn identity_resize_unit_rgb() {
    let view = RgbaView::new(2, 2, &TWO_BY_TWO).unwrap();
    let t = to_nchw_f32(view, 2, 2, Normalization::UNIT, ChannelOrder::Rgb).unwrap();
    assert_eq!(t.shape(), &[1, 3, 2, 2]);
    close(
        t.as_f32().unwrap(),
        &[
            1.0, 0.0, 0.0, 1.0, // red plane
            0.0, 1.0, 0.0, 1.0, // green plane
            0.0, 0.0, 1.0, 1.0, // blue plane
        ],
    );
}

#[test]
fn bgr_swaps_first_and_last_plane() {
    let view = RgbaView::new(2, 2, &TWO_BY_TWO).unwrap();
    let t = to_nchw_f32(view, 2, 2, Normalization::UNIT, ChannelOrder::Bgr).unwrap();
    close(
        t.as_f32().unwrap(),
        &[
            0.0, 0.0, 1.0, 1.0, //
            0.0, 1.0, 0.0, 1.0, //
            1.0, 0.0, 0.0, 1.0,
        ],
    );
}

#[test]
fn raw_255_keeps_byte_values() {
    let view = RgbaView::new(2, 2, &TWO_BY_TWO).unwrap();
    let t = to_nchw_f32(view, 2, 2, Normalization::RAW_255, ChannelOrder::Rgb).unwrap();
    close(&t.as_f32().unwrap()[..4], &[255.0, 0.0, 0.0, 255.0]);
}

#[test]
fn imagenet_normalisation() {
    let view = RgbaView::new(2, 2, &TWO_BY_TWO).unwrap();
    let t = to_nchw_f32(view, 2, 2, Normalization::IMAGENET, ChannelOrder::Rgb).unwrap();
    let d = t.as_f32().unwrap();
    // Red channel: 1.0 -> (1 - 0.485) / 0.229, 0.0 -> -0.485 / 0.229.
    let hi = (1.0 - 0.485) / 0.229;
    let lo = -0.485 / 0.229;
    close(&d[..4], &[hi, lo, lo, hi]);
    // Blue channel: 1.0 -> (1 - 0.406) / 0.225.
    let hi_b = (1.0 - 0.406) / 0.225;
    let lo_b = -0.406 / 0.225;
    close(&d[8..], &[lo_b, lo_b, hi_b, hi_b]);
}

#[test]
fn four_by_two_downscale_and_upscale() {
    // One channel carries 0, 100, 200, 40 / 0, 100, 200, 40 in red; others are 0.
    let mut px = Vec::new();
    for _ in 0..2 {
        for r in [0u8, 100, 200, 40] {
            px.extend_from_slice(&[r, 0, 0, 255]);
        }
    }
    let view = RgbaView::new(4, 2, &px).unwrap();
    // Identity.
    let same = to_nchw_f32(view, 4, 2, Normalization::RAW_255, ChannelOrder::Rgb).unwrap();
    close(
        &same.as_f32().unwrap()[..8],
        &[0.0, 100.0, 200.0, 40.0, 0.0, 100.0, 200.0, 40.0],
    );
    // Down to 2x1: samples at source x = 0.5 and 2.5 -> 50 and 120; rows average equal rows.
    let down = to_nchw_f32(view, 2, 1, Normalization::RAW_255, ChannelOrder::Rgb).unwrap();
    assert_eq!(down.shape(), &[1, 3, 1, 2]);
    close(&down.as_f32().unwrap()[..2], &[50.0, 120.0]);
    // Up to 8x2 reaches the clamped ends at the border.
    let up = to_nchw_f32(view, 8, 2, Normalization::RAW_255, ChannelOrder::Rgb).unwrap();
    let r = &up.as_f32().unwrap()[..8];
    assert_eq!(r[0], 0.0);
    assert_eq!(r[7], 40.0);
    assert!((r[1] - 25.0).abs() < 1e-4, "{r:?}");
}

#[test]
fn resize_plane_identity_up_and_down() {
    let src = [0.0, 1.0, 2.0, 3.0];
    close(&resize_plane(&src, 2, 2, 2, 2), &src);
    // 2x2 -> 4x4: corners are kept, the first inner sample is a quarter along.
    let up = resize_plane(&src, 2, 2, 4, 4);
    assert_eq!(up.len(), 16);
    assert_eq!(up[0], 0.0);
    assert_eq!(up[3], 1.0);
    assert_eq!(up[12], 2.0);
    assert_eq!(up[15], 3.0);
    assert!((up[1] - 0.25).abs() < 1e-6);
    // 4x1 -> 2x1 samples between source pairs.
    close(
        &resize_plane(&[0.0, 2.0, 4.0, 6.0], 4, 1, 2, 1),
        &[1.0, 5.0],
    );
    // Mismatched or zero sizes give an empty plane.
    assert!(resize_plane(&src, 3, 2, 2, 2).is_empty());
    assert!(resize_plane(&src, 2, 2, 0, 2).is_empty());
}

#[test]
fn normalize_plane_min_max_and_constant() {
    let mut p = [2.0, 4.0, 6.0];
    normalize_plane(&mut p);
    close(&p, &[0.0, 0.5, 1.0]);
    let mut flat = [3.0; 4];
    normalize_plane(&mut flat);
    assert_eq!(flat, [0.0; 4]);
    let mut odd = [f32::NAN, 1.0, 3.0];
    normalize_plane(&mut odd);
    close(&odd, &[0.0, 0.0, 1.0]);
    normalize_plane(&mut []);
}

#[test]
fn plane_to_gray_rgba() {
    let p = Plane::new(2, 2, vec![0.0, 0.5, 1.0, 2.0]).unwrap();
    assert_eq!(
        p.to_rgba8_gray(),
        vec![
            0, 0, 0, 255, 128, 128, 128, 255, 255, 255, 255, 255, 255, 255, 255, 255
        ]
    );
}
