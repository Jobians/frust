//! Headless app tests: the shell boots, the Models screen lists the catalog, and with an empty
//! store every Vision action is disabled with its hint. They run the real components through
//! `frust::testing::Tester` (no GPU, no window) over a model store in a scratch directory; no
//! test touches the network.

use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use frust::accesskit::Role;
use frust::kurbo::Size;
use frust::testing::{Tester, TesterOptions, find};
use frust::{View, component};
use frust_onnx::catalog::VISION_CATALOG;
use onnxvision::{ModelsScreen, OnnxVisionApp, Services, VisionScreen};

/// A phone-sized window.
const PHONE: Size = Size::new(412.0, 915.0);

/// A fresh, empty store directory under cargo's per-target scratch space.
fn scratch_store() -> PathBuf {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!(
        "onnxvision-store-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn services() -> Rc<Services> {
    Rc::new(Services::open(Some(scratch_store())).expect("a scratch store opens"))
}

/// Whether the button labelled `label` reports itself disabled.
fn disabled<V: View<()>>(tester: &Tester<(), V>, label: &str) -> bool {
    let found = tester.find(&find::label(Role::Button, label));
    let node = found
        .first()
        .and_then(|m| m.node)
        .unwrap_or_else(|| panic!("no button labelled {label:?}"));
    tester
        .semantics()
        .nodes
        .iter()
        .find(|(id, _)| *id == node)
        .is_some_and(|(_, n)| n.is_disabled())
}

#[test]
fn the_app_boots_on_the_models_destination() {
    frust_material::install();
    let root = scratch_store();
    let tester = Tester::new_with(PHONE, &TesterOptions::default(), move || {
        let app = OnnxVisionApp {
            store_root: Some(root),
        };
        ((), move |_: &mut ()| {
            component(OnnxVisionApp {
                store_root: app.store_root.clone(),
            })
        })
    });
    let texts = tester.texts();
    for expected in ["ONNX Vision", "Models", "Vision"] {
        assert!(
            texts.iter().any(|t| t == expected),
            "{expected:?} missing from {texts:?}"
        );
    }
    // The Models destination is the first one shown.
    assert!(
        !tester
            .find(&find::text(VISION_CATALOG[0].display))
            .is_empty()
    );
}

#[test]
fn the_models_screen_lists_one_row_per_catalog_model() {
    frust_material::install();
    let mut tester = Tester::new_with(PHONE, &TesterOptions::default(), || {
        let services = services();
        ((), move |_: &mut ()| {
            component(ModelsScreen {
                services: Rc::clone(&services),
            })
        })
    });
    // The storage line appears once the off-thread walk of the empty store lands.
    tester.pump_until("the storage line", Duration::from_secs(10), |t| {
        !t.find(&find::text("Models use 0 B on this device"))
            .is_empty()
    });
    assert_eq!(VISION_CATALOG.len(), 3);
    for model in VISION_CATALOG {
        assert_eq!(
            tester.find(&find::text(model.display)).len(),
            1,
            "one row for {}",
            model.display
        );
    }
    assert_eq!(tester.find(&find::label(Role::Button, "Download")).len(), 3);
    assert_eq!(tester.find(&find::text("Not downloaded")).len(), 3);
}

#[test]
fn every_vision_action_is_disabled_while_its_model_is_missing() {
    frust_material::install();
    let mut tester = Tester::new_with(PHONE, &TesterOptions::default(), || {
        let services = services();
        assert_eq!(services.vision.missing_models().len(), 3);
        ((), move |_: &mut ()| {
            component(VisionScreen {
                services: Rc::clone(&services),
            })
        })
    });
    // Wait for the sample to decode off the UI thread, so a photo is not what disables them.
    tester.pump_until("the sample", Duration::from_secs(10), |t| {
        !t.find(&find::text("Sample (generated) · 640x480"))
            .is_empty()
    });
    for label in ["Subject mask", "Faces", "Depth"] {
        assert!(disabled(&tester, label), "{label} is enabled");
    }
    for model in VISION_CATALOG {
        let hint = format!(
            "{}: download the model first ({} on the Models tab)",
            onnxvision::format::task_label(model.task),
            model.display
        );
        assert_eq!(tester.find(&find::text(&hint)).len(), 1, "hint {hint:?}");
    }
    // The photo sources stay available.
    assert!(!disabled(&tester, "Sample"));
    assert!(!disabled(&tester, "Pick photo"));
}
