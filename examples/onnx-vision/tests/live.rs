//! The live path, ignored by default because it downloads: the app downloads U2-NetP (4.6 MB)
//! from its Models screen, then runs Subject mask on the bundled sample from its Vision screen,
//! all through the real components in a headless `Tester`. Run it with
//!
//! ```sh
//! cargo test --test live -- --ignored --nocapture
//! ```
//!
//! It prints the download and inference times. The store is a scratch directory under cargo's
//! target directory, emptied first so the download really happens.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use frust::accesskit::Role;
use frust::component;
use frust::kurbo::Size;
use frust::testing::{TesterOptions, find};
use frust_models::ModelStore;
use frust_onnx::catalog::by_key;
use frust_onnx::{RgbaView, SessionOptions, VisionModels};
use onnxvision::OnnxVisionApp;
use onnxvision::imaging::load_sample;

const PHONE: Size = Size::new(412.0, 915.0);

fn live_store() -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("onnxvision-live-store");
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

#[test]
#[ignore = "downloads U2-NetP (4.6 MB) from GitHub and runs it"]
fn download_then_segment_the_sample() {
    frust_material::install();
    let root = live_store();
    let app_root = root.clone();
    let mut tester =
        frust::testing::Tester::new_with(PHONE, &TesterOptions::default(), move || {
            ((), move |_: &mut ()| {
                component(OnnxVisionApp {
                    store_root: Some(app_root.clone()),
                })
            })
        });

    // Models: the first Download button is the first catalog row, U2-NetP.
    let started = Instant::now();
    tester.tap(find::label(Role::Button, "Download"));
    tester.pump_until("U2-NetP installed", Duration::from_secs(300), |t| {
        t.texts().iter().any(|s| s.starts_with("Installed, 4.6 MB"))
    });
    let download = started.elapsed();
    println!("download: U2-NetP in {} ms", download.as_millis());

    // Vision: the sample decodes off the UI thread, then Subject mask is enabled.
    tester.tap(find::label(Role::Tab, "Vision"));
    tester.pump_until("the sample", Duration::from_secs(30), |t| {
        !t.find(&find::text("Sample (generated) · 640x480"))
            .is_empty()
    });
    tester.pump_until("Subject mask enabled", Duration::from_secs(30), |t| {
        let Some(node) = t
            .find(&find::label(Role::Button, "Subject mask"))
            .first()
            .and_then(|m| m.node)
        else {
            return false;
        };
        t.semantics()
            .nodes
            .iter()
            .any(|(id, n)| *id == node && !n.is_disabled())
    });
    tester.tap(find::label(Role::Button, "Subject mask"));
    tester.pump_until("the mask timing", Duration::from_secs(120), |t| {
        t.texts().iter().any(|s| s.starts_with("Subject mask: "))
    });
    let line = tester
        .texts()
        .into_iter()
        .find(|s| s.starts_with("Subject mask: "))
        .expect("checked above");
    println!("status line: {line}");
    assert!(line.contains(" ms on CPU"), "{line}");

    // The mask is meaningful: the ball is subject, the sky corner is not.
    let models = VisionModels::new(
        ModelStore::with_root(root).expect("store"),
        SessionOptions::default(),
    );
    assert!(
        models
            .store()
            .get(&(by_key("u2netp").expect("row").id)())
            .is_some()
    );
    let sample = load_sample().expect("sample");
    let view = RgbaView::new(sample.width, sample.height, &sample.pixels).expect("view");
    let started = Instant::now();
    let mask = models.segment_subject(view).expect("segment");
    println!(
        "direct segment_subject: {} ms (session load included)",
        started.elapsed().as_millis()
    );
    let at = |x: usize, y: usize| mask.data[y * mask.width + x];
    println!("mask at ball {:.2}, at sky {:.2}", at(390, 300), at(20, 20));
    assert!(at(390, 300) > 0.5, "the ball is the subject");
    assert!(at(20, 20) < 0.5, "the sky is background");
}
