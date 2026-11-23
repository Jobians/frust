//! Vision tasks: the YuNet decode, suppression and coordinate mapping on hand-built outputs,
//! missing and unfit models offline, and ignored live runs of the three catalog models.

#![cfg(feature = "vision")]

mod support;

use frust_models::ModelId;
use frust_onnx::catalog::{VISION_CATALOG, VisionTask, by_key};
use frust_onnx::vision::{YunetLevel, decode_yunet, map_to_image, nms};
use frust_onnx::{Face, OnnxError, Plane, RgbaView, SessionOptions, VisionModels};

fn face(rect: [f32; 4], score: f32) -> Face {
    Face {
        rect,
        landmarks: [[0.0; 2]; 5],
        score,
    }
}

/// A 64x64 input's stride-32 level (a 2x2 grid) with every cell empty except `cell`, which
/// scores `sqrt(cls * obj)`.
struct Level {
    cls: Vec<f32>,
    obj: Vec<f32>,
    bbox: Vec<f32>,
    kps: Vec<f32>,
}

impl Level {
    fn one_cell(cell: usize, cls: f32, obj: f32) -> Self {
        let mut level = Level {
            cls: vec![0.0; 4],
            obj: vec![0.0; 4],
            bbox: vec![0.0; 16],
            kps: vec![0.0; 40],
        };
        level.cls[cell] = cls;
        level.obj[cell] = obj;
        // Centre half a cell in, twice a stride wide, one stride high.
        level.bbox[cell * 4..cell * 4 + 4].copy_from_slice(&[0.5, 0.5, 2f32.ln(), 0.0]);
        for (n, v) in level.kps[cell * 10..cell * 10 + 10].iter_mut().enumerate() {
            *v = 0.25 * (n / 2) as f32;
        }
        level
    }

    fn view(&self) -> YunetLevel<'_> {
        YunetLevel {
            stride: 32,
            cls: &self.cls,
            obj: &self.obj,
            bbox: &self.bbox,
            kps: &self.kps,
        }
    }
}

fn assert_close(got: f32, want: f32) {
    assert!((got - want).abs() < 1e-4, "got {got}, want {want}");
}

#[test]
fn decode_yunet_turns_one_strong_cell_into_its_box() {
    // Cell 3 of the 2x2 grid is column 1, row 1.
    let level = Level::one_cell(3, 0.9, 0.9);
    let faces = decode_yunet(&[level.view()], 64, 64, 0.6).unwrap();
    assert_eq!(faces.len(), 1, "{faces:?}");
    let face = faces[0];
    assert_close(face.score, 0.9);
    // Centre (1.5 * 32, 1.5 * 32) = (48, 48), 64 x 32.
    for (got, want) in face.rect.into_iter().zip([16.0, 32.0, 64.0, 32.0]) {
        assert_close(got, want);
    }
    for (n, [x, y]) in face.landmarks.into_iter().enumerate() {
        let offset = (1.0 + 0.25 * n as f32) * 32.0;
        assert_close(x, offset);
        assert_close(y, offset);
    }
}

#[test]
fn decode_yunet_drops_a_cell_below_the_threshold() {
    // sqrt(0.5 * 0.5) = 0.5 < 0.6.
    let level = Level::one_cell(0, 0.5, 0.5);
    assert!(
        decode_yunet(&[level.view()], 64, 64, 0.6)
            .unwrap()
            .is_empty()
    );
    // Scores are clamped to 0..1 first: an out-of-range class score cannot lift a weak cell.
    let level = Level::one_cell(0, 4.0, 0.2);
    assert!(
        decode_yunet(&[level.view()], 64, 64, 0.6)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn decode_yunet_refuses_a_level_shorter_than_its_grid() {
    let level = Level::one_cell(0, 0.9, 0.9);
    let short = YunetLevel {
        kps: &level.kps[..39],
        ..level.view()
    };
    let err = decode_yunet(&[short], 64, 64, 0.6).unwrap_err();
    assert!(matches!(err, OnnxError::InvalidInput(_)), "{err:?}");
}

#[test]
fn nms_keeps_the_higher_of_two_overlapping_faces() {
    let lower = face([1.0, 1.0, 10.0, 10.0], 0.8);
    let higher = face([0.0, 0.0, 10.0, 10.0], 0.9);
    let kept = nms(vec![lower, higher], 0.3, 5000);
    assert_eq!(kept, vec![higher]);
}

#[test]
fn nms_keeps_both_of_two_disjoint_faces_best_first() {
    let a = face([0.0, 0.0, 10.0, 10.0], 0.7);
    let b = face([50.0, 50.0, 10.0, 10.0], 0.95);
    assert_eq!(nms(vec![a, b], 0.3, 5000), vec![b, a]);
    assert_eq!(
        nms(vec![a, b], 0.3, 1),
        vec![b],
        "top_k bounds the candidates"
    );
}

#[test]
fn faces_map_back_onto_a_non_square_image() {
    let mut on_input = face([320.0, 320.0, 64.0, 64.0], 0.9);
    on_input.landmarks[2] = [100.0, 200.0];
    let mapped = map_to_image(on_input, (640, 640), (1280, 480));
    assert_eq!(mapped.rect, [640.0, 240.0, 128.0, 48.0]);
    assert_eq!(mapped.landmarks[2], [200.0, 150.0]);
    assert_eq!(mapped.landmarks[0], [0.0, 0.0]);
    assert_eq!(mapped.score, 0.9);
}

#[test]
fn inverted_flips_a_plane() {
    let plane = Plane::new(2, 1, vec![0.25, 1.0]).unwrap();
    assert_eq!(plane.inverted().data, vec![0.75, 0.0]);
    assert_eq!(
        VisionModels::DEPTH_CONVENTION,
        frust_onnx::DepthConvention::NearIsOne
    );
}

fn small_image() -> Vec<u8> {
    vec![128; 8 * 6 * 4]
}

#[test]
fn every_task_reports_its_missing_model() {
    let models = VisionModels::new(support::empty_store("missing"), SessionOptions::default());
    let pixels = small_image();
    let image = RgbaView::new(8, 6, &pixels).unwrap();
    let key = |err: OnnxError| match err {
        OnnxError::ModelMissing { key } => key,
        other => panic!("not ModelMissing: {other:?}"),
    };
    assert_eq!(key(models.segment_subject(image).unwrap_err()), "u2netp");
    assert_eq!(key(models.detect_faces(image, None).unwrap_err()), "yunet");
    assert_eq!(
        key(models.estimate_depth(image).unwrap_err()),
        "depth-anything-v2-small"
    );
    for task in [
        VisionTask::Segmentation,
        VisionTask::FaceDetection,
        VisionTask::Depth,
    ] {
        assert!(!models.is_installed(task));
    }
    let missing: Vec<_> = models.missing().iter().map(|m| m.key).collect();
    assert_eq!(missing, ["u2netp", "yunet", "depth-anything-v2-small"]);
    assert_eq!(models.cache().load_count(), 0);

    let custom = ModelId::new("local", "mine", "mask.onnx").unwrap();
    let models = models.with_model(VisionTask::Segmentation, custom.clone());
    assert_eq!(
        key(models.segment_subject(image).unwrap_err()),
        custom.to_string()
    );
    let missing: Vec<_> = models.missing().iter().map(|m| m.key).collect();
    assert_eq!(missing, ["yunet", "depth-anything-v2-small"]);
}

#[test]
fn a_custom_model_that_does_not_fit_is_unsupported() {
    let store = support::empty_store("unfit");
    let id = ModelId::new("local", "fixtures", "add_f32.onnx").unwrap();
    store.import(&support::add_f32_path(), &id, None).unwrap();
    let pixels = small_image();
    let image = RgbaView::new(8, 6, &pixels).unwrap();
    for task in [
        VisionTask::Segmentation,
        VisionTask::FaceDetection,
        VisionTask::Depth,
    ] {
        let models = VisionModels::new(store.clone(), SessionOptions::default())
            .with_model(task, id.clone());
        assert!(models.is_installed(task));
        let result = match task {
            VisionTask::Segmentation => models.segment_subject(image).map(drop),
            VisionTask::FaceDetection => models.detect_faces(image, None).map(drop),
            VisionTask::Depth => models.estimate_depth(image).map(drop),
        };
        let err = result.unwrap_err();
        assert!(
            matches!(err, OnnxError::Unsupported(_)),
            "{task:?}: {err:?}"
        );
        assert!(models.cache().is_empty(), "an unfit model is not kept warm");
    }
    let _ = std::fs::remove_dir_all(store.root());
}

/// An RGBA image of `width` x `height`: a bright disc of radius `radius` centred on a dark field.
fn disc_image(width: u32, height: u32, radius: f32) -> Vec<u8> {
    let (cx, cy) = (width as f32 / 2.0, height as f32 / 2.0);
    let mut pixels = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            let inside = (x as f32 + 0.5 - cx).hypot(y as f32 + 0.5 - cy) <= radius;
            let v = if inside { 235 } else { 20 };
            pixels.extend_from_slice(&[v, v, v, 255]);
        }
    }
    pixels
}

fn live_models() -> VisionModels {
    VisionModels::new(support::catalog_store(), SessionOptions::default())
}

/// The names and shapes every catalog model declares, which the task functions rely on.
#[test]
#[ignore = "downloads the catalog models (about 32 MB): `cargo test -p frust-onnx --test vision -- --ignored`"]
fn live_models_declare_the_expected_signatures() {
    let store = support::catalog_store();
    let session = |key: &str| {
        let model = by_key(key).unwrap();
        frust_onnx::Session::from_file(store.path_for(&(model.id)())).unwrap()
    };
    let shape = |dims: &[usize]| dims.iter().map(|&d| Some(d)).collect::<Vec<_>>();

    let u2netp = session("u2netp");
    assert_eq!(u2netp.inputs()[0].name, "input.1");
    assert_eq!(u2netp.inputs()[0].shape, shape(&[1, 3, 320, 320]));
    assert_eq!(u2netp.outputs().len(), 7);
    assert_eq!(u2netp.outputs()[0].name, "1959");
    assert_eq!(u2netp.outputs()[0].shape, shape(&[1, 1, 320, 320]));

    let yunet = session("yunet");
    assert_eq!(yunet.inputs()[0].name, "input");
    assert_eq!(yunet.inputs()[0].shape, shape(&[1, 3, 640, 640]));
    for (stride, cells) in [(8, 6400), (16, 1600), (32, 400)] {
        for (head, per_cell) in [("cls", 1), ("obj", 1), ("bbox", 4), ("kps", 10)] {
            let name = format!("{head}_{stride}");
            let output = yunet.outputs().iter().find(|o| o.name == name).unwrap();
            assert_eq!(output.shape, shape(&[1, cells, per_cell]), "{name}");
        }
    }

    let depth = session("depth-anything-v2-small");
    assert_eq!(depth.inputs()[0].name, "pixel_values");
    assert_eq!(depth.inputs()[0].shape, vec![None, Some(3), None, None]);
    assert_eq!(depth.outputs()[0].name, "predicted_depth");
    assert_eq!(depth.outputs()[0].shape, vec![None, None, None]);
    let input = frust_onnx::Tensor::from_f32([1, 3, 518, 518], vec![0.0; 3 * 518 * 518]).unwrap();
    let outputs = depth.run_blocking(&[("pixel_values", input)]).unwrap();
    assert_eq!(
        outputs[0].1.shape(),
        &[1, 518, 518],
        "one disparity plane at the input size"
    );

    for model in VISION_CATALOG {
        let s = session(model.key);
        println!("{}: {:?} -> {:?}", model.key, s.inputs(), s.outputs());
    }
}

#[test]
#[ignore = "downloads the catalog models (about 32 MB): `cargo test -p frust-onnx --test vision -- --ignored`"]
fn live_mask_of_a_bright_disc_is_higher_inside() {
    let models = live_models();
    assert!(models.missing().is_empty());
    let (width, height, radius) = (400, 300, 80.0);
    let pixels = disc_image(width, height, radius);
    let mask = models
        .segment_subject(RgbaView::new(width, height, &pixels).unwrap())
        .unwrap();
    assert_eq!((mask.width, mask.height), (400, 300));
    let (mut inside, mut outside) = ((0.0, 0), (0.0, 0));
    for (i, &v) in mask.data.iter().enumerate() {
        assert!((0.0..=1.0).contains(&v), "{v}");
        let (x, y) = ((i % 400) as f32 + 0.5, (i / 400) as f32 + 0.5);
        let bucket = if (x - 200.0).hypot(y - 150.0) <= radius {
            &mut inside
        } else {
            &mut outside
        };
        bucket.0 += v as f64;
        bucket.1 += 1;
    }
    let (inside, outside) = (inside.0 / inside.1 as f64, outside.0 / outside.1 as f64);
    println!("mask mean inside {inside:.3}, outside {outside:.3}");
    assert!(inside > outside, "inside {inside}, outside {outside}");
}

#[test]
#[ignore = "downloads the catalog models (about 32 MB): `cargo test -p frust-onnx --test vision -- --ignored`"]
fn live_depth_is_a_finite_varied_plane_of_the_image_size() {
    let models = live_models();
    let (width, height) = (360, 240);
    let pixels = disc_image(width, height, 60.0);
    let depth = models
        .estimate_depth(RgbaView::new(width, height, &pixels).unwrap())
        .unwrap();
    assert_eq!((depth.width, depth.height), (360, 240));
    assert!(
        depth
            .data
            .iter()
            .all(|v| v.is_finite() && (0.0..=1.0).contains(v))
    );
    let n = depth.data.len() as f64;
    let mean = depth.data.iter().map(|&v| v as f64).sum::<f64>() / n;
    let variance = depth
        .data
        .iter()
        .map(|&v| (v as f64 - mean).powi(2))
        .sum::<f64>()
        / n;
    println!("depth mean {mean:.3}, variance {variance:.5}");
    assert!(variance > 0.0);
}

#[test]
#[ignore = "downloads the catalog models (about 32 MB): `cargo test -p frust-onnx --test vision -- --ignored`"]
fn live_blank_image_has_no_faces() {
    let models = live_models();
    for value in [0u8, 255] {
        let pixels: Vec<u8> = [value, value, value, 255].repeat(500 * 375);
        let faces = models
            .detect_faces(RgbaView::new(500, 375, &pixels).unwrap(), None)
            .unwrap();
        assert!(faces.is_empty(), "{faces:?}");
    }
}

/// Peak resident memory of one depth session plus a 12-megapixel image, in a fresh process:
/// `cargo test --release -p frust-onnx --test vision -- --ignored --exact measure_depth_memory --nocapture`.
#[cfg(target_os = "linux")]
#[test]
#[ignore = "measurement: downloads the catalog models; run alone in a fresh process (see the doc)"]
fn measure_depth_memory() {
    let status = |key: &str| -> f64 {
        let status = std::fs::read_to_string("/proc/self/status").unwrap();
        let kib: f64 = status
            .lines()
            .find_map(|line| line.strip_prefix(key))
            .and_then(|rest| rest.split_whitespace().next())
            .and_then(|kib| kib.parse().ok())
            .unwrap();
        kib / 1024.0
    };
    let models = live_models();
    let (width, height) = (4032, 3024);
    let pixels = disc_image(width, height, 900.0);
    let before = status("VmHWM:");
    let started = std::time::Instant::now();
    let depth = models
        .estimate_depth(RgbaView::new(width, height, &pixels).unwrap())
        .unwrap();
    let first = started.elapsed();
    assert_eq!(depth.data.len(), (width * height) as usize);
    drop(depth);
    let started = std::time::Instant::now();
    let depth = models
        .estimate_depth(RgbaView::new(width, height, &pixels).unwrap())
        .unwrap();
    let second = started.elapsed();
    println!(
        "MEASURE depth {width}x{height}: peak before {before:.1} MiB, peak after {:.1} MiB, \
         resident {:.1} MiB, first call {:.0} ms (load included), second {:.0} ms",
        status("VmHWM:"),
        status("VmRSS:"),
        first.as_secs_f64() * 1000.0,
        second.as_secs_f64() * 1000.0,
    );
    drop(depth);
}
