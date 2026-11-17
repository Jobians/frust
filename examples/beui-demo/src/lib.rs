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
//! The layout width picks the shell, not the target OS: below
//! [`nav::COMPACT_BREAKPOINT`] a phone shell (a top bar with a menu button
//! over the page, every page in a drawer), at or above it the
//! `frust_beui::components::animated_sidebar` rail beside the page. Both sit
//! in a [`safe_area`]; the phone shell's also holds the page above the soft
//! keyboard. Page content swaps under `pattern_switcher` with the
//! `FadeThrough` pattern — non-directional, the idiomatic choice for a
//! destination change with no spatial relationship.
//!
//! # Density
//!
//! Android and iOS start under beUI's touch density, every other target under
//! the comfortable one ([`start_density`]). The Content page's density chips
//! read the live theme, so they start on whichever one was installed.
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
use frust::{
    AnyView, BackFallback, Component, CrossAxisAlignment, Either, Get, GetUntracked, RwSignal, Set,
    View, WindowMetrics, any, back_fallback, row, safe_area, use_context,
};
use frust_beui::Density;

use nav::Page;

/// The app's whole retained state: which gallery page is showing, whether the
/// nav rail is expanded, whether the phone shell's page list is open, and the
/// state of the two pages that keep theirs here (Home and Theming) — every
/// section page hosts its own in a page-local component (see `pages/mod.rs`).
pub struct AppState {
    pub page: Page,
    pub sidebar_open: bool,
    /// A signal rather than a plain flag: the back fallback that closes the
    /// list runs with no app state in reach.
    pub menu_open: RwSignal<bool>,
    /// Claims the system back press while the page list is open.
    pub menu_back: BackFallback,
    pub home: pages::home::State,
    pub theming: pages::theming::State,
}

impl Default for AppState {
    fn default() -> Self {
        let menu_open = RwSignal::new(false);
        Self {
            page: Page::Home,
            sidebar_open: true,
            menu_open,
            menu_back: BackFallback::new(
                move || menu_open.get_untracked(),
                move || menu_open.set(false),
            ),
            home: Default::default(),
            theming: Default::default(),
        }
    }
}

/// The density the app starts under: touch on Android and iOS, beUI's own
/// comfortable sizes everywhere else.
pub const fn start_density() -> Density {
    if cfg!(any(target_os = "android", target_os = "ios")) {
        Density::Touch
    } else {
        Density::Comfortable
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
        shell(state)
    }
}

/// The whole shell for the current layout width: the phone shell below
/// [`nav::COMPACT_BREAKPOINT`], the rail beside the page at or above it.
pub fn shell(state: &mut AppState) -> impl View<AppState> + use<> {
    let page = state.page;
    let compact = use_context::<WindowMetrics>().is_some_and(|m| nav::is_compact(m.size.width));
    let body = page_body(page, state);
    if compact {
        let slot = if page.fills_phone_slot() {
            Either::Left(nav::phone_fill_slot(body))
        } else {
            Either::Right(nav::phone_scroll_slot(body))
        };
        let content =
            pattern_switcher::<AppState, FadeThrough, _>(page as usize, FadeThrough, slot);
        let phone = nav::phone_shell(page, state.menu_open.get(), content);
        Either::Left(back_fallback(&state.menu_back, phone))
    } else {
        let content = pattern_switcher::<AppState, FadeThrough, _>(
            page as usize,
            FadeThrough,
            nav::scroll_slot(body),
        );
        let wide = row()
            .child(nav::shell_sidebar(state))
            .flex(1, nav::shell_inset(page, state.sidebar_open, content))
            .cross_axis(CrossAxisAlignment::Stretch);
        Either::Right(safe_area(wide))
    }
}

frust::app!(
    BeuiDemoApp,
    setup = {
        frust_beui::install_with_density(start_density());
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

/// Serialises every test that wires a back fallback or pushes a back press:
/// the press counter is process-wide.
#[cfg(test)]
pub(crate) static BACK_PRESS_SLOT: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod shell_tests {
    use std::sync::MutexGuard;
    use std::time::Duration;

    use frust::authoring::{Rect, Role, Size, WindowEdgeInsets, WindowInsets};
    use frust::testing::{Tester, TesterOptions, find};
    use frust::{GetUntracked, View, push_back_press};
    use frust_beui::agents::message_scroller::MessageScrollerWidget;
    use frust_beui::agents::prompt_input::PROMPT_SEND_LABEL;
    use frust_beui::components::animated_sidebar::AnimatedSidebarWidget;
    use frust_beui::{BeuiMetrics, Density, theme_with_density};

    use super::{AppState, BACK_PRESS_SLOT, Page, shell};

    const PHONE: Size = Size::new(390.0, 844.0);
    const DESKTOP: Size = Size::new(1200.0, 800.0);
    const FRAME: Duration = Duration::from_millis(16);
    const EPSILON: f64 = 0.5;

    fn slot() -> MutexGuard<'static, ()> {
        BACK_PRESS_SLOT.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The app's own shell on `page`, under the phone theme and `insets`.
    fn app(size: Size, insets: WindowInsets, page: Page) -> Tester<AppState, impl View<AppState>> {
        let options = TesterOptions::default()
            .theme(theme_with_density(Density::Touch))
            .insets(insets);
        Tester::new_with(size, &options, move || {
            let state = AppState {
                page,
                ..AppState::default()
            };
            (state, shell)
        })
    }

    /// Run `frames` 16 ms frames: long enough for the drawer and the page
    /// switch to settle, without waiting on pages that animate forever.
    fn run(tester: &mut Tester<AppState, impl View<AppState>>, frames: usize) {
        for _ in 0..frames {
            tester.pump(FRAME);
        }
    }

    fn rail_count(tester: &Tester<AppState, impl View<AppState>>) -> usize {
        tester.find(&find::by_type::<AnimatedSidebarWidget>()).len()
    }

    fn list_shows(tester: &Tester<AppState, impl View<AppState>>, label: &str) -> bool {
        !tester.find(&find::label(Role::Button, label)).is_empty()
    }

    #[test]
    fn a_phone_width_has_no_rail_and_the_menu_switches_the_page() {
        let _slot = slot();
        let mut app = app(PHONE, WindowInsets::default(), Page::Home);
        assert_eq!(rail_count(&app), 0);
        let menu = app.bounds(&find::label(Role::Button, "Menu"));
        let touch = BeuiMetrics::touch().min_touch_target;
        assert!(
            menu.width() >= touch - EPSILON && menu.height() >= touch - EPSILON,
            "{menu:?}"
        );
        assert!(!list_shows(&app, "Theming"), "the list starts closed");

        app.tap(find::label(Role::Button, "Menu"));
        run(&mut app, 60);
        assert!(app.state().menu_open.get_untracked());
        for page in Page::ALL {
            assert!(list_shows(&app, page.label()), "{page:?} is listed");
        }
        for section in ["Motion", "Agents", "Blocks"] {
            assert!(!app.find(&find::text(section)).is_empty(), "{section}");
        }

        app.tap(find::label(Role::Button, "Content"));
        run(&mut app, 60);
        assert_eq!(app.state().page, Page::Content);
        assert!(!app.state().menu_open.get_untracked());
        assert!(!list_shows(&app, "Theming"), "the list closed");
        assert!(
            list_shows(&app, "Count a press"),
            "the Content page is showing: {:?}",
            app.texts()
        );
    }

    #[test]
    fn a_back_press_and_a_backdrop_tap_each_close_the_list() {
        let _slot = slot();
        let mut app = app(PHONE, WindowInsets::default(), Page::Home);
        app.tap(find::label(Role::Button, "Menu"));
        run(&mut app, 60);
        assert!(list_shows(&app, "Theming"));
        push_back_press();
        run(&mut app, 60);
        assert!(!app.state().menu_open.get_untracked());
        assert!(!list_shows(&app, "Theming"), "back closed the list");
        assert_eq!(app.state().page, Page::Home, "and left the page alone");

        app.tap(find::label(Role::Button, "Menu"));
        run(&mut app, 60);
        assert!(list_shows(&app, "Theming"));
        app.tap(frust::authoring::Point::new(
            PHONE.width - 20.0,
            PHONE.height / 2.0,
        ));
        run(&mut app, 60);
        assert!(!app.state().menu_open.get_untracked());
        assert!(!list_shows(&app, "Theming"), "the backdrop closed the list");
    }

    #[test]
    fn a_desktop_width_keeps_the_rail_and_has_no_menu_button() {
        let _slot = slot();
        let app = app(DESKTOP, WindowInsets::default(), Page::Home);
        assert_eq!(rail_count(&app), 1);
        assert!(!list_shows(&app, "Menu"));
        assert!(list_shows(&app, "Collapse"));
    }

    #[test]
    fn a_narrow_desktop_window_gets_the_phone_shell_too() {
        let _slot = slot();
        let app = app(
            Size::new(super::nav::COMPACT_BREAKPOINT - 1.0, 600.0),
            WindowInsets::default(),
            Page::Home,
        );
        assert_eq!(rail_count(&app), 0);
        assert!(list_shows(&app, "Menu"));
        let app = self::app(
            Size::new(super::nav::COMPACT_BREAKPOINT, 600.0),
            WindowInsets::default(),
            Page::Home,
        );
        assert_eq!(rail_count(&app), 1);
    }

    /// The page slot's viewport: the shell's outermost scroll view.
    fn page_viewport(tester: &Tester<AppState, impl View<AppState>>) -> Rect {
        tester
            .find(&find::by_type_name("frust_widgets::scroll::ScrollWidget"))
            .first()
            .expect("the page slot scrolls")
            .bounds
    }

    #[test]
    fn the_phone_shell_respects_the_safe_area_and_the_keyboard() {
        let _slot = slot();
        let bars = WindowEdgeInsets {
            top: 47.0,
            bottom: 34.0,
            ..WindowEdgeInsets::default()
        };
        let app = app(
            PHONE,
            WindowInsets::new(bars, WindowEdgeInsets::default()),
            Page::Home,
        );
        let menu = app.bounds(&find::label(Role::Button, "Menu"));
        assert!(
            menu.y0 >= 47.0 - EPSILON,
            "top bar under the status bar: {menu:?}"
        );
        let viewport = page_viewport(&app);
        assert!(
            (viewport.y1 - (PHONE.height - 34.0)).abs() < EPSILON,
            "content ends at the home indicator: {viewport:?}"
        );

        let keyboard = WindowEdgeInsets {
            bottom: 336.0,
            ..WindowEdgeInsets::default()
        };
        let app = self::app(PHONE, WindowInsets::new(bars, keyboard), Page::Home);
        let viewport = page_viewport(&app);
        assert!(
            (viewport.y1 - (PHONE.height - 336.0)).abs() < EPSILON,
            "content ends at the keyboard: {viewport:?}"
        );
        let menu = app.bounds(&find::label(Role::Button, "Menu"));
        assert!(menu.y0 >= 47.0 - EPSILON, "{menu:?}");
    }

    /// A `keyboard`-tall soft keyboard over the phone window.
    fn keyboard_up(keyboard: f64) -> frust::WindowMetrics {
        let bars = WindowEdgeInsets {
            bottom: keyboard,
            ..WindowEdgeInsets::default()
        };
        let insets = WindowInsets::new(WindowEdgeInsets::default(), bars);
        frust::WindowMetrics::new(PHONE, 1.0, insets)
    }

    #[test]
    fn a_focused_field_stays_above_the_keyboard() {
        let _slot = slot();
        let keyboard = 336.0;
        let mut app = app(PHONE, WindowInsets::default(), Page::Content);
        run(&mut app, 30);
        let field = || find::role(Role::TextInput);
        // Bring the field into the top half, where a tap can reach it.
        for _ in 0..40 {
            if app.bounds(&field()).y1 < PHONE.height / 2.0 {
                break;
            }
            app.drag(
                frust::authoring::Point::new(PHONE.width / 2.0, 600.0),
                (0.0, -200.0).into(),
            );
            run(&mut app, 30);
        }
        app.tap(field());
        run(&mut app, 10);
        app.set_metrics(keyboard_up(keyboard));
        run(&mut app, 60);
        let (field, viewport) = (app.bounds(&field()), page_viewport(&app));
        assert!(
            (viewport.y1 - (PHONE.height - keyboard)).abs() < EPSILON,
            "{viewport:?}"
        );
        // The scroll view reveals the caret line with a 16 px margin, not
        // the whole field, so a tall field's bottom edge may stay under.
        let line = field.center().y;
        assert!(
            field.y0 >= viewport.y0 - EPSILON && line < viewport.y1,
            "field {field:?} outside {viewport:?}"
        );
    }

    /// The Chat page fills the phone slot, so its composer is laid out from
    /// the slot's bottom edge rather than revealed by a scroll: the field and
    /// the send button both end above the keyboard, and the transcript keeps
    /// the room above them.
    #[test]
    fn the_chat_composer_and_its_send_button_stay_above_the_keyboard() {
        let _slot = slot();
        let mut app = app(PHONE, WindowInsets::default(), Page::AgentsChat);
        run(&mut app, 30);
        let field = || find::role(Role::TextInput);
        let send = || find::label(Role::Button, PROMPT_SEND_LABEL);
        let top_bar = app.bounds(&find::label(Role::Button, "Menu")).y1;

        for keyboard in [0.0, 336.0] {
            if keyboard > 0.0 {
                app.tap(field());
                run(&mut app, 10);
                app.set_metrics(keyboard_up(keyboard));
                run(&mut app, 60);
            }
            let edge = PHONE.height - keyboard;
            let (field, send) = (app.bounds(&field()), app.bounds(&send()));
            for (name, part) in [("field", field), ("send button", send)] {
                assert!(
                    part.y0 >= top_bar - EPSILON && part.y1 <= edge + EPSILON,
                    "keyboard {keyboard}: the {name} {part:?} is outside {top_bar}..{edge}"
                );
            }
            // Pinned to the slot's bottom edge, not merely somewhere above it.
            assert!(
                edge - send.y1 < 48.0,
                "keyboard {keyboard}: the send button {send:?} left the bottom edge {edge}"
            );
            let transcript = app
                .find(&find::by_type::<MessageScrollerWidget>())
                .first()
                .expect("the transcript scrolls")
                .bounds;
            assert!(
                transcript.y0 >= top_bar - EPSILON
                    && transcript.y1 <= field.y0 + EPSILON
                    && transcript.height() > 100.0,
                "keyboard {keyboard}: transcript {transcript:?} above the field {field:?}"
            );
        }
    }

    /// The Chat page rebuilds on every event, which lays the transcript out
    /// again between the moves of a drag; the drag still scrolls it.
    #[test]
    fn a_touch_drag_scrolls_the_chat_transcript() {
        let _slot = slot();
        let mut app = app(PHONE, WindowInsets::default(), Page::AgentsChat);
        run(&mut app, 30);
        let transcript = find::by_type::<MessageScrollerWidget>();
        let first_row = |app: &Tester<AppState, _>| {
            let nodes = app.inspect();
            let scroller = nodes
                .iter()
                .find(|n| n.type_name.ends_with("MessageScrollerWidget"))
                .expect("the transcript")
                .id;
            nodes
                .iter()
                .find(|n| n.parent == Some(scroller))
                .expect("a row")
                .bounds
                .y0
        };
        let viewport = app.bounds(&transcript);
        let start = first_row(&app);
        assert!(start < viewport.y0, "the transcript opens on its last turn");

        app.drag(viewport.center(), (0.0, 150.0).into());
        run(&mut app, 60);
        let pulled = first_row(&app);
        assert!(
            pulled - start >= 150.0 - EPSILON,
            "the drag showed earlier turns: {start} to {pulled}"
        );

        app.drag(viewport.center(), (0.0, -400.0).into());
        run(&mut app, 60);
        assert!(
            (first_row(&app) - start).abs() < EPSILON,
            "and dragging back returns to the last turn"
        );
    }

    /// The pages whose content runs past a 390 px window's right edge in the
    /// phone shell: fixed-width stages, rows that do not wrap and wide
    /// specimens. Recorded rather than fixed here; a page that stops
    /// overflowing leaves this list.
    const WIDER_THAN_A_PHONE: [Page; 14] = [
        Page::Home,
        Page::MotionText,
        Page::MotionButtons,
        Page::MotionControls,
        Page::MotionSelection,
        Page::MotionOverlays,
        Page::MotionNavigation,
        Page::MotionSurfaces,
        Page::MotionData,
        Page::MotionShader,
        Page::BlocksCommand,
        Page::BlocksForms,
        Page::BlocksShowcase,
        Page::GpuEffects,
    ];

    const SCROLL: &str = "frust_widgets::scroll::ScrollWidget";

    /// Whether any node runs past the window's right edge, not counting
    /// content an inner scroll view clips (the page slot is the outermost).
    fn overflows(tester: &Tester<AppState, impl View<AppState>>, width: f64) -> bool {
        let nodes = tester.inspect();
        let slot = nodes.iter().find(|n| n.type_name == SCROLL).map(|n| n.id);
        let inner_scroll = |node: &frust::testing::InspectNode| {
            let mut parent = node.parent;
            while let Some(id) = parent {
                let Some(up) = nodes.iter().find(|n| n.id == id) else {
                    break;
                };
                if up.type_name == SCROLL && Some(up.id) != slot {
                    return true;
                }
                parent = up.parent;
            }
            false
        };
        nodes
            .iter()
            .any(|n| n.bounds.x1 > width + EPSILON && !inner_scroll(n))
    }

    #[test]
    fn every_page_lays_out_at_a_phone_width() {
        let _slot = slot();
        let mut wide = Vec::new();
        for page in Page::ALL {
            let mut app = app(PHONE, WindowInsets::default(), page);
            run(&mut app, 3);
            assert_eq!(app.state().page, page);
            if overflows(&app, PHONE.width) {
                wide.push(page);
            }
        }
        assert_eq!(wide, WIDER_THAN_A_PHONE);
    }
}
