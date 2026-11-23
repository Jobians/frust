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
    store_named("onnxvision-live-store")
}

fn store_named(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
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
    // "Subject mask: 87 ms on CPU": a time, then the provider that ran. Apple hosts pick CoreML or
    // the CPU per model; everywhere else it is the CPU.
    let detail = line.strip_prefix("Subject mask: ").expect("checked above");
    let (time, provider) = detail
        .split_once(" ms on ")
        .unwrap_or_else(|| panic!("the status line names a time and a provider: {line}"));
    assert!(
        !time.is_empty() && time.chars().all(|c| c.is_ascii_digit()),
        "a whole number of milliseconds: {line}"
    );
    // The provider is the word after " on "; "(includes loading the model)" may follow it.
    let provider = provider.split_whitespace().next().unwrap_or_default();
    assert!(!provider.is_empty(), "a provider is named: {line}");
    if !cfg!(target_vendor = "apple") {
        assert_eq!(provider, "CPU", "{line}");
    }

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

/// Resident set size in bytes, from `/proc/self/statm`; `None` off Linux.
fn resident_bytes() -> Option<u64> {
    let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
    let pages: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
    Some(pages * 4096)
}

/// Whether the Subject mask button is enabled.
fn mask_enabled<V: frust::View<()>>(t: &frust::testing::Tester<(), V>) -> bool {
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
}

/// Runs Subject mask 30 times through the app's own screens and prints the resident memory after
/// runs 5 and 30. It prints rather than asserts: the figure depends on the host and on other
/// tests sharing the process, so run it alone:
/// `cargo test --test live growth -- --ignored --nocapture`.
#[test]
#[ignore = "downloads U2-NetP (4.6 MB) from GitHub and runs it 30 times"]
fn per_run_memory_growth() {
    frust_material::install();
    let root = store_named("onnxvision-growth-store");
    let mut tester =
        frust::testing::Tester::new_with(PHONE, &TesterOptions::default(), move || {
            ((), move |_: &mut ()| {
                component(OnnxVisionApp {
                    store_root: Some(root.clone()),
                })
            })
        });
    tester.tap(find::label(Role::Button, "Download"));
    tester.pump_until("U2-NetP installed", Duration::from_secs(300), |t| {
        t.texts().iter().any(|s| s.starts_with("Installed, 4.6 MB"))
    });
    tester.tap(find::label(Role::Tab, "Vision"));
    tester.pump_until("the sample", Duration::from_secs(30), |t| {
        !t.find(&find::text("Sample (generated) · 640x480"))
            .is_empty()
    });
    let mut at_five = None;
    for run in 1..=30u32 {
        tester.pump_until("Subject mask enabled", Duration::from_secs(60), |t| {
            mask_enabled(t)
        });
        tester.tap(find::label(Role::Button, "Subject mask"));
        // The button is disabled while a run is in flight: wait for it to start, then to end.
        tester.pump_until("the run to start", Duration::from_secs(60), |t| {
            !mask_enabled(t)
        });
        tester.pump_until("the run to end", Duration::from_secs(120), |t| {
            mask_enabled(t)
        });
        let line = tester
            .texts()
            .into_iter()
            .find(|s| s.starts_with("Subject mask: "));
        println!("run {run}: resident {:?} {line:?}", resident_bytes());
        if run == 5 {
            at_five = resident_bytes();
        }
    }
    let at_thirty = resident_bytes();
    println!("resident after run 5: {at_five:?} bytes, after run 30: {at_thirty:?} bytes");
}
