//! A button that segments a generated image and a line showing the mask status, in one
//! `Component`.
//!
//! The component owns a `VisionHandle`. `build` only reads its signals; the button hands the
//! handle an image and returns at once, while the model runs on the handle's worker thread and
//! its result is written on the UI thread, which wakes for it. A
//! model that is not downloaded yet shows up as a failed status (see `frust-models` for the
//! download). Run it inside a Frust app by passing `VisionScreen` to `frust::app!`; here it only
//! has to compile.

use std::sync::Arc;

use frust::{Component, Get, View, button, column, text};
use frust_models::ModelStore;
use frust_onnx::api::{VisionHandle, VisionStatus};
use frust_onnx::{SessionOptions, VisionModels};

const SIDE: u32 = 256;

#[derive(Default)]
struct VisionScreen;

struct ScreenState {
    vision: VisionHandle,
}

/// A bright disc on a dark background, as RGBA8.
fn generated_image() -> Arc<[u8]> {
    let centre = SIDE as f32 / 2.0;
    let mut pixels = Vec::with_capacity((SIDE * SIDE * 4) as usize);
    for y in 0..SIDE {
        for x in 0..SIDE {
            let distance = ((x as f32 - centre).powi(2) + (y as f32 - centre).powi(2)).sqrt();
            let value = if distance < centre / 2.0 { 230 } else { 20 };
            pixels.extend_from_slice(&[value, value, value, 255]);
        }
    }
    pixels.into()
}

impl Component for VisionScreen {
    type State = ScreenState;

    fn init(&self) -> ScreenState {
        let store = ModelStore::open().expect("the app data directory is available");
        ScreenState {
            vision: VisionHandle::new(VisionModels::new(store, SessionOptions::default())),
        }
    }

    fn build(&self, state: &mut ScreenState) -> impl View<ScreenState> {
        let status = match state.vision.status().get() {
            VisionStatus::Idle => "Idle".to_owned(),
            VisionStatus::Running(task) => format!("Running {task:?}"),
            VisionStatus::Failed(message) => format!("Failed: {message}"),
        };
        let mask = match state.vision.mask().get() {
            Some(mask) => {
                let subject = mask.data.iter().filter(|&&v| v >= 0.5).count();
                format!(
                    "Mask {}x{}: {subject} subject pixels",
                    mask.width, mask.height
                )
            }
            None => "No mask yet".to_owned(),
        };

        column()
            .child(button("Segment", |state: &mut ScreenState| {
                state.vision.segment(generated_image(), SIDE, SIDE);
            }))
            .child(text(status))
            .child(text(mask))
    }
}

fn main() {
    // The example is compiled to prove the API shape; a real app starts it with
    // `frust::app!(VisionScreen);`.
    let _ = VisionScreen;
}
