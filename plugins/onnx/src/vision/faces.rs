//! Face detection with YuNet: [`VisionModels::detect_faces`], and the pure decode
//! ([`decode_yunet`]), suppression ([`nms`]) and coordinate mapping ([`map_to_image`]) it is
//! built from, usable on any YuNet output.

use std::cmp::Ordering;

use super::VisionModels;
use crate::catalog::VisionTask;
use crate::image::{ChannelOrder, Normalization, to_nchw_f32};
use crate::{DType, OnnxError, RgbaView, Session};

/// The score below which [`VisionModels::detect_faces`] drops a face unless told otherwise.
pub const DEFAULT_MIN_SCORE: f32 = 0.6;
/// Of two faces overlapping by more than this intersection-over-union, the lower-scored is
/// dropped.
pub const NMS_IOU: f32 = 0.3;
/// At most this many candidates, the highest-scored, enter suppression.
pub const TOP_K: usize = 5000;
/// YuNet's output strides; each has the outputs `cls_<s>`, `obj_<s>`, `bbox_<s>` and `kps_<s>`.
pub const YUNET_STRIDES: [u32; 3] = [8, 16, 32];

/// One detected face.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Face {
    /// `[x, y, width, height]` in pixels, the top-left corner first. Not clipped: a face at an
    /// edge may extend past the image.
    pub rect: [f32; 4],
    /// Five `[x, y]` points in pixels: right eye, left eye, nose tip, right and left mouth
    /// corners (right and left of the person, not of the viewer).
    pub landmarks: [[f32; 2]; 5],
    /// Confidence, 0..1.
    pub score: f32,
}

/// The four outputs of one YuNet stride, flat: one `cls` and one `obj` value, four `bbox`
/// values and ten `kps` values per grid cell, cells row by row.
#[derive(Debug, Clone, Copy)]
pub struct YunetLevel<'a> {
    /// The stride in input pixels (8, 16 or 32).
    pub stride: u32,
    /// Class scores.
    pub cls: &'a [f32],
    /// Objectness scores.
    pub obj: &'a [f32],
    /// Box offsets: centre `x`, `y` in cells from the cell's corner, then `ln(w)`, `ln(h)` in
    /// strides.
    pub bbox: &'a [f32],
    /// Landmark offsets, `x`, `y` five times, in cells from the cell's corner.
    pub kps: &'a [f32],
}

/// Every cell of `levels` scoring at least `min_score`, as faces in model input pixels, in no
/// particular order (run [`nms`] next).
///
/// `input_w` x `input_h` is the model's input size, padded up to a multiple of 32 to give each
/// level's grid. A cell's score is `sqrt(cls * obj)`, each clamped to 0..1 first. A level whose
/// slices are shorter than its grid is [`OnnxError::InvalidInput`].
pub fn decode_yunet(
    levels: &[YunetLevel<'_>],
    input_w: u32,
    input_h: u32,
    min_score: f32,
) -> Result<Vec<Face>, OnnxError> {
    let pad = |side: u32| side.div_ceil(32) * 32;
    let mut faces = Vec::new();
    for level in levels {
        if level.stride == 0 {
            return Err(OnnxError::InvalidInput("a YuNet stride of 0".to_owned()));
        }
        let cols = (pad(input_w) / level.stride) as usize;
        let rows = (pad(input_h) / level.stride) as usize;
        let cells = cols * rows;
        let short = |name: &str, len: usize, per_cell: usize| {
            (len < cells * per_cell).then(|| {
                OnnxError::InvalidInput(format!(
                    "{name}_{} holds {len} values; its {cols}x{rows} grid needs {}",
                    level.stride,
                    cells * per_cell
                ))
            })
        };
        let lengths = [
            ("cls", level.cls.len(), 1),
            ("obj", level.obj.len(), 1),
            ("bbox", level.bbox.len(), 4),
            ("kps", level.kps.len(), 10),
        ];
        if let Some(err) = lengths
            .into_iter()
            .find_map(|(name, len, per_cell)| short(name, len, per_cell))
        {
            return Err(err);
        }
        let stride = level.stride as f32;
        for index in 0..cells {
            let score =
                (level.cls[index].clamp(0.0, 1.0) * level.obj[index].clamp(0.0, 1.0)).sqrt();
            if score.partial_cmp(&min_score).is_none_or(Ordering::is_lt) {
                continue;
            }
            let (col, row) = ((index % cols) as f32, (index / cols) as f32);
            let bbox = &level.bbox[index * 4..index * 4 + 4];
            let (cx, cy) = ((col + bbox[0]) * stride, (row + bbox[1]) * stride);
            let (w, h) = (bbox[2].exp() * stride, bbox[3].exp() * stride);
            let kps = &level.kps[index * 10..index * 10 + 10];
            let landmarks = std::array::from_fn(|n| {
                [(kps[2 * n] + col) * stride, (kps[2 * n + 1] + row) * stride]
            });
            faces.push(Face {
                rect: [cx - w / 2.0, cy - h / 2.0, w, h],
                landmarks,
                score,
            });
        }
    }
    Ok(faces)
}

fn iou(a: &[f32; 4], b: &[f32; 4]) -> f32 {
    let w = (a[0] + a[2]).min(b[0] + b[2]) - a[0].max(b[0]);
    let h = (a[1] + a[3]).min(b[1] + b[3]) - a[1].max(b[1]);
    if w <= 0.0 || h <= 0.0 {
        return 0.0;
    }
    let inter = w * h;
    let union = a[2] * a[3] + b[2] * b[3] - inter;
    if union > 0.0 { inter / union } else { 0.0 }
}

/// Greedy non-maximum suppression: the `top_k` highest-scored faces, best first, each dropped
/// when it overlaps an already kept one by more than `iou_threshold` (intersection over union).
/// Returns the kept faces sorted by score, highest first.
pub fn nms(mut faces: Vec<Face>, iou_threshold: f32, top_k: usize) -> Vec<Face> {
    faces.sort_by(|a, b| b.score.total_cmp(&a.score));
    faces.truncate(top_k);
    let mut kept: Vec<Face> = Vec::new();
    for face in faces {
        if kept
            .iter()
            .all(|k| iou(&k.rect, &face.rect) <= iou_threshold)
        {
            kept.push(face);
        }
    }
    kept
}

/// `face`, found on a `from` (width, height) model input that the image was stretched onto,
/// in the pixels of the `to` (width, height) image: x scaled by `to.0 / from.0`, y by
/// `to.1 / from.1`.
pub fn map_to_image(face: Face, from: (u32, u32), to: (u32, u32)) -> Face {
    let sx = to.0 as f32 / from.0 as f32;
    let sy = to.1 as f32 / from.1 as f32;
    let [x, y, w, h] = face.rect;
    Face {
        rect: [x * sx, y * sy, w * sx, h * sy],
        landmarks: face.landmarks.map(|[lx, ly]| [lx * sx, ly * sy]),
        score: face.score,
    }
}

/// The twelve per-stride output names, `cls` to `kps` for each stride.
fn output_names() -> impl Iterator<Item = String> {
    YUNET_STRIDES
        .into_iter()
        .flat_map(|stride| ["cls", "obj", "bbox", "kps"].map(|head| format!("{head}_{stride}")))
}

fn yunet_outputs(session: &Session) -> Result<(), String> {
    for name in output_names() {
        match session.outputs().iter().find(|output| output.name == name) {
            Some(output) if output.dtype == DType::F32 => {}
            Some(output) => return Err(format!("output `{name}` is {}, not f32", output.dtype)),
            None => return Err(format!("it has no output `{name}`")),
        }
    }
    Ok(())
}

impl VisionModels {
    /// The faces in `image`, highest score first, in the image's pixels.
    ///
    /// The image is **stretched** (not letterboxed) to the model's input (640x640 for the
    /// catalog's YuNet), blue-green-red with raw 0..255 values; the outputs are decoded with
    /// [`decode_yunet`], faces scoring under `min_score` (default [`DEFAULT_MIN_SCORE`]) are
    /// dropped, overlaps are suppressed with [`nms`] at [`NMS_IOU`] and [`TOP_K`], and every box
    /// and landmark is scaled back to the image by [`map_to_image`], x and y separately.
    ///
    /// **Blocking**: loads the model on first use, then runs it on the calling thread. Fails with
    /// [`OnnxError::ModelMissing`] when the model is not in the store.
    pub fn detect_faces(
        &self,
        image: RgbaView<'_>,
        min_score: Option<f32>,
    ) -> Result<Vec<Face>, OnnxError> {
        let model = self.load(VisionTask::FaceDetection, yunet_outputs)?;
        if model.width % 32 != 0 || model.height % 32 != 0 {
            return Err(OnnxError::Unsupported(format!(
                "a YuNet input of {}x{} is not a multiple of 32 on each side",
                model.width, model.height
            )));
        }
        let input = to_nchw_f32(
            image,
            model.width,
            model.height,
            Normalization::RAW_255,
            ChannelOrder::Bgr,
        )?;
        let outputs = model
            .session
            .run_blocking(&[(model.input.as_str(), input)])?;
        let output = |head: &str, stride: u32| {
            let name = format!("{head}_{stride}");
            outputs
                .iter()
                .find(|(n, _)| *n == name)
                .and_then(|(_, tensor)| tensor.as_f32())
                .ok_or_else(|| OnnxError::Run(format!("the model returned no f32 `{name}`")))
        };
        let mut levels = Vec::with_capacity(YUNET_STRIDES.len());
        for stride in YUNET_STRIDES {
            levels.push(YunetLevel {
                stride,
                cls: output("cls", stride)?,
                obj: output("obj", stride)?,
                bbox: output("bbox", stride)?,
                kps: output("kps", stride)?,
            });
        }
        let min_score = min_score.unwrap_or(DEFAULT_MIN_SCORE);
        let faces = decode_yunet(&levels, model.width, model.height, min_score)?;
        let from = (model.width, model.height);
        let to = (image.width(), image.height());
        Ok(nms(faces, NMS_IOU, TOP_K)
            .into_iter()
            .map(|face| map_to_image(face, from, to))
            .collect())
    }
}
