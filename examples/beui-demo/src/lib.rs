//! `beuidemo`: the beUI design-system plugin's gallery, on desktop, Android
//! and iOS — a standalone workspace (see `Cargo.toml`'s header), shaped like
//! `examples/material3-demo`.
//!
//! This module owns the whole app; `src/main.rs` is only the desktop preview
//! entry point calling the `__frust_main` the [`frust::app!`] at the bottom of
//! this file generates. That macro also emits the Android JNI exports and the
//! iOS C-ABI exports, so the same tree drives all three shells.
//!
//! # Nav shell
//!
//! The rail is `frust_beui::components::animated_sidebar` — the catalog's own
//! motion-driven sidebar — carrying every gallery page in one flat,
//! springing list; see `nav.rs`'s module docs for why it is flat rather than
//! the grouped/labelled shape `examples/shadcn-demo`'s `frust_shadcn` sidebar
//! has. Page content swaps under `frust::motion::switcher`'s
//! `pattern_switcher` with the `FadeThrough` pattern — non-directional, the
//! idiomatic choice for a destination change with no spatial relationship.
//!
//! # Running it
//!
//! From this directory: `cargo run` for the desktop preview, `frust run -d
//! <device>` for an Android or iOS device or simulator — see `README.md`.

mod nav;
mod pages;

use frust::motion::patterns::FadeThrough;
use frust::motion::switcher::pattern_switcher;
// `DesktopConfig` and the menu vocabulary are named fully-qualified inside
// `frust::app!`'s `desktop = { .. }` block: `frust` gates them off for Android
// and iOS, and the macro emits that block only into the desktop entry point.
use frust::{AnyView, Component, CrossAxisAlignment, View, any, row};

use nav::Page;

/// The app's whole retained state: which gallery page is showing, whether the
/// nav rail is expanded, and the state of the two pages that keep theirs here
/// (Home and Theming) — every section page hosts its own in a page-local
/// component (see `pages/mod.rs`).
pub struct AppState {
    pub page: Page,
    pub sidebar_open: bool,
    pub home: pages::home::State,
    pub theming: pages::theming::State,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            page: Page::Home,
            sidebar_open: true,
            home: Default::default(),
            theming: Default::default(),
        }
    }
}

/// Resolve the active page's content view.
// erasure: keep 20-arm page match, one concrete view type per page
fn page_body(page: Page, state: &mut AppState) -> AnyView<AppState> {
    match page {
        Page::Home => any(pages::home::page(&mut state.home)),
        Page::Content => any(pages::content::page()),
        Page::Theming => any(pages::theming::page(&mut state.theming)),
        Page::MotionText => any(pages::motion::text::page()),
        Page::MotionButtons => any(pages::motion::buttons::page()),
        Page::MotionControls => any(pages::motion::controls::page()),
        Page::MotionSelection => any(pages::motion::selection::page()),
        Page::MotionOverlays => any(pages::motion::overlays::page()),
        Page::MotionNavigation => any(pages::motion::navigation::page()),
        Page::MotionSurfaces => any(pages::motion::surfaces::page()),
        Page::MotionData => any(pages::motion::data::page()),
        Page::MotionShader => any(pages::motion::shader::page()),
        Page::AgentsPrimitives => any(pages::agents::primitives::page()),
        Page::AgentsPanels => any(pages::agents::panels::page()),
        Page::AgentsChat => any(pages::agents::chat::page()),
        Page::BlocksCommand => any(pages::blocks::command::page()),
        Page::BlocksMorph => any(pages::blocks::morph::page()),
        Page::BlocksForms => any(pages::blocks::forms::page()),
        Page::BlocksShowcase => any(pages::blocks::showcase::page()),
        Page::GpuEffects => any(pages::gpu_effects::page()),
    }
}

#[derive(Default)]
struct BeuiDemoApp;

impl Component for BeuiDemoApp {
    type State = AppState;

    fn init(&self) -> AppState {
        AppState::default()
    }

    fn build(&self, state: &mut AppState) -> impl View<AppState> {
        let page = state.page;
        let open = state.sidebar_open;

        let body = page_body(page, state);
        let content = pattern_switcher::<AppState, FadeThrough, _>(
            page as usize,
            FadeThrough,
            nav::scroll_slot(body),
        );

        let sidebar = nav::shell_sidebar(state);

        row()
            .child(sidebar)
            .flex(1, nav::shell_inset(page, open, content))
            .cross_axis(CrossAxisAlignment::Stretch)
    }
}

frust::app!(
    BeuiDemoApp,
    setup = {
        frust_beui::install();
    },
    desktop = {
        let quit =
            frust::MenuSpec::new().with_item(frust::MenuItemSpec::role(frust::MenuRole::Quit));
        frust::DesktopConfig::new()
            .with_app_name("beUI Gallery")
            .with_app_id("dev.frust.beuidemo")
            .with_menu_spec(
                frust::MenuSpec::new().with_item(frust::MenuItemSpec::submenu("File", quit)),
            )
    }
);
