//! `onnxvision`: on-device vision with `frust-models` and `frust-onnx`, the way an app uses them.
//!
//! Two destinations behind a Material 3 navigation bar:
//!
//! - **Models** ([`models_screen`]): the vision catalog with download, live progress, cancel,
//!   resume and delete, the store's disk use and a note on devices under 4 GB of memory.
//! - **Vision** ([`vision_screen`]): the bundled sample or a picked photo, and the three tasks
//!   (subject mask, faces, depth) run through `VisionHandle` with their overlays and timings.
//!
//! Both screens share one [`Services`]: the model store, its listing, one download tracker per
//! catalog row and the vision handle. Nothing blocking runs on the UI thread: downloads run on
//! their own threads, and decoding, disk walks and inference run on blocking threads.

pub mod format;
pub mod imaging;
pub mod models_screen;
pub mod services;
pub mod vision_screen;

use std::path::PathBuf;
use std::rc::Rc;

use frust::{
    Component, EdgeInsets, Either, Padding, View, component, icon, indexed_stack, safe_area,
    scaffold, text,
};
use frust_material::{app_bar, icons, nav_item, navigation_bar};

pub use models_screen::ModelsScreen;
pub use services::Services;
pub use vision_screen::VisionScreen;

/// The app: the shell around the two screens.
#[derive(Default)]
pub struct OnnxVisionApp {
    /// Where the model store lives; `None` is the app data directory. The tests point it at a
    /// scratch directory.
    pub store_root: Option<PathBuf>,
}

/// The shell's state.
pub struct AppState {
    /// The selected destination: 0 is Models, 1 is Vision.
    pub tab: usize,
    /// The shared handles, or why the model store could not open.
    pub services: Result<Rc<Services>, String>,
}

impl Component for OnnxVisionApp {
    type State = AppState;

    fn init(&self) -> AppState {
        AppState {
            tab: 0,
            services: Services::open(self.store_root.clone()).map(Rc::new),
        }
    }

    fn build(&self, state: &mut AppState) -> impl View<AppState> {
        // An indexed stack keeps both screens mounted, so a download's row and a photo survive
        // switching destinations.
        let body = match &state.services {
            Ok(services) => Either::Left(
                indexed_stack(Some(state.tab))
                    .child(component(ModelsScreen {
                        services: Rc::clone(services),
                    }))
                    .child(component(VisionScreen {
                        services: Rc::clone(services),
                    })),
            ),
            Err(message) => Either::Right(Padding(EdgeInsets::all(16.0), text(message.clone()))),
        };
        let destinations = vec![
            nav_item::<AppState>("Models").icon(icon(icons::SYSTEM_UPDATE)),
            nav_item::<AppState>("Vision").icon(icon(icons::IMAGE)),
        ];
        let nav_bar = navigation_bar(destinations, state.tab, |state: &mut AppState, index| {
            state.tab = index;
        });
        scaffold(body)
            .app_bar(app_bar("ONNX Vision"))
            .bottom_bar(safe_area(nav_bar).top(false).bottom(false))
    }
}

// `setup` runs once on the UI thread before the root `init`: Material 3 is the design system.
frust::app!(
    OnnxVisionApp,
    setup = {
        frust_material::install();
    }
);
