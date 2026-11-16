//! Content: the parts a phone screen is built from — a hero card over stat
//! tiles, a grouped list, chips and status badges, avatars, alerts, key-value
//! rows and a timeline — beside every button tone and size, a text field, a
//! floating select and a toast, in one column no wider than a phone.
//!
//! # Density
//!
//! The chips at the top pick beUI's [`Density`]. Each pushes [`density_theme`]
//! (`theme_with_density` with the live brightness pinned) through
//! `frust::set_app_theme`, the override slot the light/dark [`theme_toggle`]
//! writes too. The toggle re-attaches the live theme's [`BeuiMetrics`], so a
//! brightness flip keeps the chosen density; the chips read the live
//! brightness, so a density change keeps the brightness. Only the parts that
//! read the metrics change size (see `frust_beui::tokens::metrics`).
//!
//! # Width
//!
//! The column is at most [`MAX_CONTENT_WIDTH`] wide and centred, so a desktop
//! window shows the phone layout instead of stretching it. State lives in a
//! page-local [`frust::component`], as on every section page.

use frust::{
    Align, Alignment, Brightness, Component, CrossAxisAlignment, SizedBox, Theme, View, ViewSeq,
    WrapCrossAlignment, column, component, icon, icons, row, set_app_theme, stack, text,
    use_context, wrap,
};
use frust_beui::components::alert::{AlertTone, alert};
use frust_beui::components::animated_toast_stack::{
    AnimatedToastStackEntry, AnimatedToastStackPosition, AnimatedToastStackStatus, animated_toast,
    animated_toast_stack,
};
use frust_beui::components::avatar::{AvatarSize, AvatarTone, avatar};
use frust_beui::components::button::{ButtonSize, ButtonTone, button};
use frust_beui::components::card::{hero_card, stat_tile};
use frust_beui::components::chip::{StatusBadgeTone, chip, status_badge};
use frust_beui::components::grouped_list::{grouped_list, list_row, section_header};
use frust_beui::components::input::input;
use frust_beui::components::key_value::{TimelineStep, TimelineStepState, key_value_row, timeline};
use frust_beui::components::select::{SelectOption, select, select_option, select_trigger};
use frust_beui::components::theme_toggle::theme_toggle;
use frust_beui::overlay::OverlayAnchor;
use frust_beui::{BeuiMetrics, Density, theme_with_density};

use crate::AppState;
use crate::nav::{caption, heading};

/// The widest the page's column lays out, in logical px: a large phone.
pub const MAX_CONTENT_WIDTH: f64 = 480.0;

/// The toast stage's height: room for the trigger and a short stack.
const TOAST_STAGE_HEIGHT: f64 = 200.0;

/// The select's options.
const PLANS: [&str; 3] = ["Starter", "Pro", "Team"];

/// The removable chips a fresh page starts with.
const TAGS: [&str; 3] = ["design", "mobile", "beta"];

/// One badge per tone, labelled by what the tone means.
const BADGES: [(&str, StatusBadgeTone); 5] = [
    ("Draft", StatusBadgeTone::Neutral),
    ("Syncing", StatusBadgeTone::Info),
    ("Live", StatusBadgeTone::Success),
    ("Expiring", StatusBadgeTone::Warning),
    ("Failed", StatusBadgeTone::Danger),
];

/// Every button tone, labelled by its name.
const TONES: [(&str, ButtonTone); 5] = [
    ("Primary", ButtonTone::Primary),
    ("Secondary", ButtonTone::Secondary),
    ("Ghost", ButtonTone::Ghost),
    ("Outline", ButtonTone::Outline),
    ("Destructive", ButtonTone::Destructive),
];

/// Every button size; the square icon size carries a one-glyph label.
const SIZES: [(&str, ButtonSize); 4] = [
    ("Small", ButtonSize::Sm),
    ("Medium", ButtonSize::Md),
    ("Large", ButtonSize::Lg),
    ("+", ButtonSize::Icon),
];

/// The theme the density chips push: beUI's at `density`, with `brightness`
/// pinned — the shape `theme_toggle` pushes, so either control keeps the
/// other's choice.
pub fn density_theme(density: Density, brightness: Brightness) -> Theme {
    theme_with_density(density).with_brightness(brightness)
}

/// This page's retained values. The density is not here: it lives on the
/// app theme, which both the chips and the brightness toggle write.
pub struct State {
    presses: u32,
    starred: bool,
    tags: Vec<&'static str>,
    alert_dismissed: bool,
    name: String,
    plan: Option<usize>,
    plan_open: bool,
    plan_anchor: OverlayAnchor,
    toasts: Vec<AnimatedToastStackEntry>,
    next_toast_id: u64,
}

impl Default for State {
    fn default() -> Self {
        Self {
            presses: 0,
            starred: true,
            tags: TAGS.to_vec(),
            alert_dismissed: false,
            name: String::new(),
            plan: Some(1),
            plan_open: false,
            plan_anchor: OverlayAnchor::new(),
            toasts: Vec::new(),
            next_toast_id: 1,
        }
    }
}

/// The page's interactive body.
struct ContentPage;

impl Component for ContentPage {
    type State = State;

    fn init(&self) -> State {
        State::default()
    }

    fn build(&self, state: &mut State) -> impl View<State> {
        let live = use_context::<Theme>();
        column()
            .child(Self::density_bar(live.as_ref()))
            .child(Self::section("Cards", Self::summary()))
            .child(Self::section("Grouped list", Self::people(state)))
            .child(Self::section("Chips and badges", Self::chips(state)))
            .child(Self::section("Avatars", Self::avatars()))
            .child(Self::section("Alerts", Self::alerts(state)))
            .child(Self::section("Details", Self::details()))
            .child(Self::section("Buttons", Self::buttons()))
            .child(Self::section("Form", Self::form(state)))
            .child(Self::section("Toast", Self::toast(state)))
            .cross_axis(CrossAxisAlignment::Stretch)
    }
}

impl ContentPage {
    /// A vertical spacer.
    fn gap(height: f64) -> impl View<State> {
        SizedBox(None, Some(height))
    }

    /// One titled block of the column.
    fn section<M>(title: &str, body: impl ViewSeq<State, M>) -> impl View<State> {
        column()
            .child(text(title.to_string()).size(16.0))
            .child(Self::gap(10.0))
            .children(body)
            .child(Self::gap(28.0))
            .cross_axis(CrossAxisAlignment::Stretch)
    }

    /// Queue a toast with the next id.
    fn raise(s: &mut State, status: AnimatedToastStackStatus, title: &str) {
        let id = s.next_toast_id;
        s.next_toast_id += 1;
        s.toasts.push(animated_toast(id, title).status(status));
    }

    /// The density chips, the brightness toggle, and what the live density
    /// resolves to.
    fn density_bar(live: Option<&Theme>) -> impl View<State> {
        let brightness = live.map_or(Brightness::Light, |t| t.brightness);
        let metrics = BeuiMetrics::resolve(live);
        let touch = metrics.density == Density::Touch;
        let summary = if touch {
            format!(
                "Touch: controls clear {:.0} px, a medium button is {:.0} px tall.",
                metrics.min_touch_target, metrics.height_md
            )
        } else {
            format!(
                "Comfortable: beUI's own sizes, a medium button is {:.0} px tall.",
                metrics.height_md
            )
        };
        let controls = wrap()
            .spacing(8.0)
            .run_spacing(8.0)
            .cross_alignment(WrapCrossAlignment::Center)
            .child(text("Density").size(14.0))
            .child(
                chip("Comfortable")
                    .selected(!touch)
                    .on_press(move |_: &mut State| {
                        set_app_theme(density_theme(Density::Comfortable, brightness))
                    }),
            )
            .child(
                chip("Touch")
                    .selected(touch)
                    .on_press(move |_: &mut State| {
                        set_app_theme(density_theme(Density::Touch, brightness))
                    }),
            )
            .child(theme_toggle::<State>());
        column()
            .child(controls)
            .child(Self::gap(6.0))
            .child(caption(summary))
            .child(Self::gap(24.0))
            .cross_axis(CrossAxisAlignment::Stretch)
    }

    /// Two stat tiles side by side, sharing the width.
    fn tile_row(left: impl View<State>, right: impl View<State>) -> impl View<State> {
        row()
            .flex(1, left)
            .child(SizedBox(Some(8.0), None))
            .flex(1, right)
    }

    /// The hero card: two actions over a 2x2 grid of stat tiles.
    fn summary() -> impl View<State> {
        let grid = column()
            .child(Self::tile_row(
                stat_tile("Orders", "128").trailing(status_badge("+12%", StatusBadgeTone::Success)),
                stat_tile("Revenue", "$4.2k"),
            ))
            .child(Self::gap(8.0))
            .child(Self::tile_row(
                stat_tile("Returns", "3").trailing(status_badge("-2", StatusBadgeTone::Info)),
                stat_tile("Rating", "4.8"),
            ))
            .cross_axis(CrossAxisAlignment::Stretch);
        hero_card("This week", grid)
            .subtitle("Monday 10 to Sunday 16 November")
            .actions((
                button("View report", |s: &mut State| {
                    Self::raise(s, AnimatedToastStackStatus::Info, "Report opened")
                }),
                button("Share", |s: &mut State| {
                    Self::raise(s, AnimatedToastStackStatus::Success, "Link copied")
                })
                .tone(ButtonTone::Outline),
            ))
    }

    /// A grouped list: avatar rows, one pressable counter row, one
    /// destructive row.
    fn people(state: &State) -> impl View<State> {
        let presses = match state.presses {
            1 => "1 press".to_string(),
            n => format!("{n} presses"),
        };
        let rows = [
            list_row("Ada Lovelace")
                .leading(avatar("AL").label("Ada Lovelace"))
                .subtitle("ada@example.com")
                .value("Owner")
                .chevron(true),
            list_row("Grace Hopper")
                .leading(avatar("GH").label("Grace Hopper"))
                .subtitle("grace@example.com")
                .value("Editor")
                .chevron(true),
            list_row("Count a press")
                .leading(avatar("+").tone(AvatarTone::Muted))
                .value(presses)
                .chevron(true)
                .on_press(|s: &mut State| s.presses += 1),
            list_row("Remove from team").destructive(true),
        ];
        column()
            .child(section_header("Team"))
            .child(Self::gap(6.0))
            .child(grouped_list(rows))
            .cross_axis(CrossAxisAlignment::Stretch)
    }

    /// A selectable chip, the removable tags, and every badge tone.
    fn chips(state: &State) -> impl View<State> {
        let tags: Vec<_> = state
            .tags
            .iter()
            .map(|&tag| chip(tag).on_remove(move |s: &mut State| s.tags.retain(|t| *t != tag)))
            .collect();
        let chips = wrap()
            .spacing(8.0)
            .run_spacing(8.0)
            .cross_alignment(WrapCrossAlignment::Center)
            .child(
                chip("Starred")
                    .leading(icon(icons::STAR).size(14.0))
                    .selected(state.starred)
                    .on_press(|s: &mut State| s.starred = !s.starred),
            )
            .children(tags)
            .when(state.tags.len() < TAGS.len(), |w| {
                w.child(
                    button("Reset tags", |s: &mut State| s.tags = TAGS.to_vec())
                        .tone(ButtonTone::Ghost)
                        .size(ButtonSize::Sm),
                )
            });
        let badges =
            wrap()
                .spacing(8.0)
                .run_spacing(8.0)
                .children(BADGES.map(|(label, tone)| {
                    status_badge(label, tone).dot(tone != StatusBadgeTone::Neutral)
                }));
        column()
            .child(chips)
            .child(Self::gap(12.0))
            .child(badges)
            .cross_axis(CrossAxisAlignment::Start)
    }

    /// Every avatar size, plus the muted tone.
    fn avatars() -> impl View<State> {
        wrap()
            .spacing(12.0)
            .run_spacing(8.0)
            .cross_alignment(WrapCrossAlignment::Center)
            .child(avatar("Sm").size(AvatarSize::Sm).label("Small avatar"))
            .child(avatar("Md").size(AvatarSize::Md).label("Medium avatar"))
            .child(avatar("Lg").size(AvatarSize::Lg).label("Large avatar"))
            .child(
                avatar("KM")
                    .size(AvatarSize::Lg)
                    .tone(AvatarTone::Muted)
                    .label("Muted avatar"),
            )
    }

    /// Every alert tone: a banner, actions, and a dismissible one.
    fn alerts(state: &State) -> impl View<State> {
        column()
            .child(
                alert(AlertTone::Info, "Maintenance tonight")
                    .message("Sync pauses from 01:00 to 02:00.")
                    .banner(true),
            )
            .child(Self::gap(10.0))
            .child(alert(AlertTone::Success, "Profile saved"))
            .child(Self::gap(10.0))
            .child(
                alert(AlertTone::Warning, "Storage almost full")
                    .message("2.1 GB of 2.5 GB used.")
                    .actions(
                        button("Manage", |_: &mut State| {})
                            .tone(ButtonTone::Outline)
                            .size(ButtonSize::Sm),
                    ),
            )
            .child(Self::gap(10.0))
            .when(!state.alert_dismissed, |c| {
                c.child(
                    alert(AlertTone::Danger, "Payment failed")
                        .message("Update your card to keep your plan.")
                        .on_dismiss(|s: &mut State| s.alert_dismissed = true),
                )
            })
            .when(state.alert_dismissed, |c| {
                c.child(
                    button("Show the dismissed alert", |s: &mut State| {
                        s.alert_dismissed = false
                    })
                    .tone(ButtonTone::Ghost)
                    .size(ButtonSize::Sm),
                )
            })
            .cross_axis(CrossAxisAlignment::Stretch)
    }

    /// Key-value rows (a mono id, a long value, a trailing badge) over a
    /// three-step timeline.
    fn details() -> impl View<State> {
        column()
            .child(key_value_row("Order", "A-20931-7F").mono(true))
            .child(key_value_row(
                "Ship to",
                "Flat 4, 221B Baker Street, Marylebone, London NW1 6XE, United Kingdom",
            ))
            .child(
                key_value_row("Status", "In transit")
                    .trailing(status_badge("On time", StatusBadgeTone::Success)),
            )
            .child(Self::gap(16.0))
            .child(timeline([
                TimelineStep::new("Ordered", TimelineStepState::Done).detail("Mon 10 Nov, 09:41"),
                TimelineStep::new("Shipped", TimelineStepState::Current)
                    .detail("Left the warehouse"),
                TimelineStep::new("Delivered", TimelineStepState::Pending),
            ]))
            .cross_axis(CrossAxisAlignment::Stretch)
    }

    /// Every button tone at the default size, then every size.
    fn buttons() -> impl View<State> {
        let tones = TONES.map(|(label, tone)| button(label, |_: &mut State| {}).tone(tone));
        let sizes = SIZES.map(|(label, size)| {
            button(label, |_: &mut State| {})
                .tone(ButtonTone::Secondary)
                .size(size)
        });
        column()
            .child(wrap().spacing(8.0).run_spacing(8.0).children(tones))
            .child(Self::gap(12.0))
            .child(
                wrap()
                    .spacing(8.0)
                    .run_spacing(8.0)
                    .cross_alignment(WrapCrossAlignment::Center)
                    .children(sizes),
            )
            .cross_axis(CrossAxisAlignment::Start)
    }

    /// The select's options.
    fn plans() -> Vec<SelectOption> {
        PLANS.iter().map(|&plan| select_option(plan)).collect()
    }

    /// A text field and a select whose panel floats, so it needs no
    /// full-area stage inside the scrolling page.
    fn form(state: &State) -> impl View<State> {
        let label = state.plan.map(|index| PLANS[index].to_string());
        column()
            .child(
                input(state.name.clone(), |s: &mut State, value| s.name = value)
                    .label("Display name")
                    .placeholder("Ada Lovelace"),
            )
            .child(Self::gap(12.0))
            .child(
                select_trigger(&state.plan_anchor, label)
                    .placeholder("Choose a plan")
                    .open(state.plan_open)
                    .on_open_change(|s: &mut State, open| s.plan_open = open),
            )
            .child(
                select(Self::plans(), state.plan, |s: &mut State, index| {
                    s.plan = Some(index);
                    s.plan_open = false;
                })
                .anchor(&state.plan_anchor)
                .open(state.plan_open)
                .on_open_change(|s: &mut State, open| s.plan_open = open)
                .floating(true),
            )
            .cross_axis(CrossAxisAlignment::Stretch)
    }

    /// A toast trigger over the stack it raises into, in a bounded stage: the
    /// stack fills the area it is given.
    fn toast(state: &State) -> impl View<State> {
        let trigger = column()
            .child(
                button("Show a toast", |s: &mut State| {
                    Self::raise(
                        s,
                        AnimatedToastStackStatus::Success,
                        "Saved to your library",
                    )
                })
                .tone(ButtonTone::Secondary)
                .size(ButtonSize::Sm),
            )
            .cross_axis(CrossAxisAlignment::Start);
        let toasts = animated_toast_stack(state.toasts.clone(), |s: &mut State, id| {
            s.toasts.retain(|toast| toast.id() != id)
        })
        .position(AnimatedToastStackPosition::BottomCenter)
        .max_visible(3);
        SizedBox(None, Some(TOAST_STAGE_HEIGHT)).child(stack().child(trigger).child(toasts))
    }
}

pub fn page() -> impl View<AppState> {
    let content = column()
        .child(heading("Content"))
        .child(SizedBox(None, Some(8.0)))
        .child(caption(
            "The parts a phone screen is made of, in a column no wider than a \
             phone. Switch the density to see which parts follow it; the \
             light/dark toggle keeps the density you chose.",
        ))
        .child(SizedBox(None, Some(20.0)))
        .child(component(ContentPage))
        .cross_axis(CrossAxisAlignment::Stretch);
    Align(
        Alignment::new(0.0, -1.0),
        SizedBox(Some(MAX_CONTENT_WIDTH), None).child(content),
    )
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::time::Duration;

    use frust::authoring::{Role, Size};
    use frust::testing::{Tester, TesterOptions, find};
    use frust::{Brightness, CrossAxisAlignment, IconWidget, Theme, View, row};
    use frust_beui::{BeuiMetrics, Density, theme_with_density};
    use frust_shell_common::ThemeOverrideWatcher;

    use super::{MAX_CONTENT_WIDTH, density_theme, page};
    use crate::nav::{Page, scroll_slot, shell_sidebar};
    use crate::{AppState, page_body};

    const PHONE: Size = Size::new(390.0, 844.0);
    const DESKTOP: Size = Size::new(1200.0, 800.0);
    /// A phone width tall enough to hold the whole page, so every tap lands
    /// inside the window.
    const TALL_PHONE: Size = Size::new(390.0, 6000.0);
    const FRAME: Duration = Duration::from_millis(16);
    const EPSILON: f64 = 0.5;

    /// The theme-override slot is process-global: a test that writes it
    /// holds this, so two writers never interleave.
    static OVERRIDE_SLOT: Mutex<()> = Mutex::new(());

    /// The page in the gallery's own scrolling slot, under `theme`. The app
    /// state is built inside the Tester, which owns the reactive runtime its
    /// back fallback needs.
    fn page_tester(theme: Theme, size: Size) -> Tester<AppState, impl View<AppState>> {
        Tester::new_with(size, &TesterOptions::default().theme(theme), || {
            (AppState::default(), |_: &mut AppState| scroll_slot(page()))
        })
    }

    const THEMES: [(Density, Brightness); 4] = [
        (Density::Comfortable, Brightness::Light),
        (Density::Comfortable, Brightness::Dark),
        (Density::Touch, Brightness::Light),
        (Density::Touch, Brightness::Dark),
    ];

    #[test]
    fn nothing_extends_past_a_phone_width_in_any_density_or_brightness() {
        for (density, brightness) in THEMES {
            let tester = page_tester(density_theme(density, brightness), PHONE);
            let past: Vec<String> = tester
                .inspect()
                .into_iter()
                .filter(|n| n.bounds.x0 < -EPSILON || n.bounds.x1 > PHONE.width + EPSILON)
                .map(|n| format!("{} {:?}", n.type_name, n.bounds))
                .collect();
            assert!(past.is_empty(), "{density:?} {brightness:?}: {past:#?}");
            tester.assert_no_text_overflow();
        }
    }

    #[test]
    fn a_desktop_window_centres_the_phone_column() {
        let left = (DESKTOP.width - MAX_CONTENT_WIDTH) / 2.0;
        let right = left + MAX_CONTENT_WIDTH;
        for (density, brightness) in THEMES {
            let tester = page_tester(density_theme(density, brightness), DESKTOP);
            let heading = tester.bounds(&find::value("Content"));
            assert!(
                (heading.x0 - left).abs() < EPSILON,
                "{density:?} {brightness:?}: heading at {heading:?}"
            );
            let outside: Vec<String> = tester
                .text_runs()
                .into_iter()
                .filter(|run| run.bounds.x0 < left - EPSILON || run.bounds.x1 > right + EPSILON)
                .map(|run| format!("{:?} {:?}", run.text, run.bounds))
                .collect();
            assert!(
                outside.is_empty(),
                "{density:?} {brightness:?}: {outside:#?}"
            );
        }
    }

    #[test]
    fn switching_density_changes_a_buttons_height() {
        let height = |density| {
            page_tester(density_theme(density, Brightness::Light), TALL_PHONE)
                .bounds(&find::label(Role::Button, "Primary"))
                .height()
        };
        let comfortable = height(Density::Comfortable);
        let touch = height(Density::Touch);
        assert!(
            (comfortable - BeuiMetrics::comfortable().height_md).abs() < EPSILON,
            "{comfortable}"
        );
        assert!(
            (touch - BeuiMetrics::touch().height_md).abs() < EPSILON,
            "{touch}"
        );
        assert!(touch > comfortable);
    }

    /// The chips derive their selection from the live theme, so a phone
    /// started under `install_with_density(Density::Touch)` opens the page on
    /// Touch with no state of its own to agree with.
    #[test]
    fn the_chips_start_on_the_density_the_app_installed() {
        for density in [Density::Comfortable, Density::Touch] {
            let tester = page_tester(theme_with_density(density), PHONE);
            let metrics = BeuiMetrics::for_density(density);
            let summary = match density {
                Density::Touch => format!(
                    "Touch: controls clear {:.0} px, a medium button is {:.0} px tall.",
                    metrics.min_touch_target, metrics.height_md
                ),
                _ => format!(
                    "Comfortable: beUI's own sizes, a medium button is {:.0} px tall.",
                    metrics.height_md
                ),
            };
            assert_eq!(tester.find(&find::value(&summary)).len(), 1, "{density:?}");
        }
        let host = if cfg!(any(target_os = "android", target_os = "ios")) {
            Density::Touch
        } else {
            Density::Comfortable
        };
        assert_eq!(crate::start_density(), host);
    }

    #[test]
    fn pressing_the_counter_row_updates_its_value() {
        let theme = density_theme(Density::Touch, Brightness::Light);
        let mut tester = page_tester(theme, TALL_PHONE);
        let row = find::label(Role::Button, "Count a press");
        assert_eq!(tester.find(&find::value("0 presses")).len(), 1);
        tester.tap(&row);
        tester.pump(FRAME);
        assert_eq!(tester.find(&find::value("1 press")).len(), 1);
        tester.tap(&row);
        tester.pump(FRAME);
        assert_eq!(tester.find(&find::value("2 presses")).len(), 1);
    }

    /// Both controls write the app-theme override; this polls it the way a
    /// shell does each frame and re-themes a fresh page with what it read.
    #[test]
    fn the_density_chips_and_the_brightness_toggle_keep_each_others_choice() {
        let _slot = OVERRIDE_SLOT.lock().unwrap_or_else(|e| e.into_inner());
        let mut watcher = ThemeOverrideWatcher::new();
        let _ = watcher.poll();

        let mut tester = page_tester(
            density_theme(Density::Comfortable, Brightness::Dark),
            TALL_PHONE,
        );
        tester.tap(find::label(Role::Button, "Touch"));
        tester.pump(FRAME);
        let touch = watcher
            .poll()
            .flatten()
            .expect("the Touch chip pushes an app theme");
        assert_eq!(BeuiMetrics::resolve(Some(&touch)), BeuiMetrics::touch());
        assert_eq!(
            touch.brightness,
            Brightness::Dark,
            "density kept brightness"
        );

        let mut tester = page_tester(touch, TALL_PHONE);
        tester.tap(find::label(Role::Button, "Switch to light mode"));
        tester.pump(FRAME);
        let flipped = watcher
            .poll()
            .flatten()
            .expect("the toggle pushes an app theme");
        assert_eq!(flipped.brightness, Brightness::Light);
        assert_eq!(
            BeuiMetrics::resolve(Some(&flipped)),
            BeuiMetrics::touch(),
            "brightness kept density"
        );
    }

    #[test]
    fn the_content_page_is_the_twentieth_and_reachable_from_the_rail() {
        assert_eq!(Page::ALL.len(), 20);
        for (i, page) in Page::ALL.iter().enumerate() {
            assert!(!Page::ALL[i + 1..].contains(page), "{page:?} appears twice");
        }
        assert!(Page::ALL.contains(&Page::Content));

        // The shell's own rail beside the shell's own page dispatch.
        let mut app = Tester::new_with(DESKTOP, &TesterOptions::default(), || {
            let build = |s: &mut AppState| {
                let body = page_body(s.page, s);
                row()
                    .child(shell_sidebar(s))
                    .flex(1, scroll_slot(body))
                    .cross_axis(CrossAxisAlignment::Stretch)
            };
            (AppState::default(), build)
        });
        assert_eq!(app.state().page, Page::Home);
        // The rail's item nodes carry no bounds of their own, so the tap aims
        // at the item's icon: the rail's icons come first, in `Page::ALL` order.
        let icons = app.find(&find::by_type::<IconWidget>());
        let index = Page::ALL.iter().position(|&p| p == Page::Content).unwrap();
        app.tap(icons[index].bounds.center());
        app.pump(FRAME);
        assert_eq!(app.state().page, Page::Content);
        app.pump(Duration::from_millis(600));
        assert!(
            !app.find(&find::label(Role::Button, "Count a press"))
                .is_empty(),
            "the Content page is showing: {:?}",
            app.texts()
        );
    }
}
