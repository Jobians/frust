//! Per-pod layout skipping, proven against the layout audit.
//!
//! A `RenderRoot` returns a clean pod's recorded size without calling its
//! `layout`, so a widget that changes what its layout reads without signalling
//! dirty (a rebuild `LAYOUT`, a paint `request_layout`, a consumed event) lays
//! out stale. `RenderRoot::set_layout_audit` catches exactly that: after the
//! skipping pass it runs a forced full pass and counts every pod whose geometry
//! differs. Every scenario below runs with the audit on and asserts it found
//! nothing; the harness also names the offending widget types, by replaying the
//! audit's two passes itself, so a regression points at its widget.
//!
//! The widgets here are the ones the layout-skip audit fixed: the select,
//! combobox and multi-select lists' trigger-width floor (an anchor cell the
//! trigger's paint writes), the filtering lists' row presences, the dynamic
//! island / expandable tabs / notification stack boxes (a reduce-motion snap
//! lands them in `paint` without asking for a frame), and the input's message
//! row, which stops taking room on the paint its exit lands on.
//!
//! The phone-content parts (alert, avatar, card, chip, grouped list, key-value)
//! set text runs in rebuild, read `BeuiMetrics` at layout and lay out children
//! conditionally. Each is driven through an unchanged rebuild (no pod left
//! layout-dirty), a content change, a live density and text-scale change, and
//! its structural toggles; every changed tree must match a fresh mount.

use std::any::Any;
use std::collections::HashMap;

use frust::authoring::text::TextContext;
use frust::authoring::{AnyView, DiscardScene, PaintOutcome, PaintScene, View, WidgetId, any};
use frust::{SizedBox, SizedBoxView, Theme, column, row, stack};
use frust_beui::Density;
use frust_beui::blocks::dynamic_island::{dynamic_island, dynamic_island_slot};
use frust_beui::blocks::expandable_tabs::{expandable_tabs, expandable_tabs_item};
use frust_beui::blocks::notification_stack::{notification, notification_stack};
use frust_beui::components::alert::{AlertTone, alert};
use frust_beui::components::avatar::{AvatarSize, avatar};
use frust_beui::components::card::{card, hero_card, stat_tile};
use frust_beui::components::chip::{StatusBadgeTone, chip, status_badge};
use frust_beui::components::combobox::{combobox, combobox_option, combobox_trigger};
use frust_beui::components::grouped_list::{grouped_list, list_row, section_header};
use frust_beui::components::input::input;
use frust_beui::components::key_value::{TimelineStep, TimelineStepState, key_value_row, timeline};
use frust_beui::components::multi_select::{
    multi_select, multi_select_option, multi_select_trigger,
};
use frust_beui::components::select::{select, select_option, select_trigger};
use frust_beui::overlay::OverlayAnchor;
use frust_core::{FrameTime, RenderRoot};
use kurbo::{Point, Size};

// ---------------------------------------------------------------------------
// The audit harness.
// ---------------------------------------------------------------------------

/// One frame's interval, in nanoseconds (60 Hz).
const FRAME_NS: u64 = 16_666_667;

/// A pod's geometry as the audit compares it: origin relative to its parent,
/// and size.
type Geometry = HashMap<WidgetId, (&'static str, Point, Size)>;

fn geometry<S: 'static>(root: &RenderRoot<S, AnyView<S>>) -> Geometry {
    let nodes = root.inspect();
    let origins: HashMap<WidgetId, Point> =
        nodes.iter().map(|n| (n.id, n.bounds.origin())).collect();
    nodes
        .into_iter()
        .map(|n| {
            let parent = n
                .parent
                .and_then(|p| origins.get(&p).copied())
                .unwrap_or(Point::ZERO);
            let origin = Point::new(n.bounds.x0 - parent.x, n.bounds.y0 - parent.y);
            (n.id, (n.type_name, origin, n.bounds.size()))
        })
        .collect()
}

/// The app's view function, as the harness holds it.
type ViewFn<S> = Box<dyn FnMut(&mut S) -> AnyView<S>>;

/// A mounted view driven frame by frame with the layout audit on.
struct Audit<S: 'static> {
    root: RenderRoot<S, AnyView<S>>,
    view: ViewFn<S>,
    state: S,
    text: TextContext,
    window: Size,
    now: u64,
    /// `type_name` of every pod the replayed audit found stale.
    offenders: Vec<&'static str>,
}

impl<S: 'static> Audit<S> {
    fn new(state: S, theme: Theme, view: impl FnMut(&mut S) -> AnyView<S> + 'static) -> Self {
        let mut audit = Audit {
            root: RenderRoot::new(),
            view: Box::new(view),
            state,
            text: TextContext::new(),
            window: Size::new(400.0, 600.0),
            now: 0,
            offenders: Vec::new(),
        };
        // Measure with the faces beUI ships, not the host's sans-serif: a fallback with a
        // 1.0 em line height lets taller text fit inside a row's minimum height.
        for bytes in frust_beui::font_data() {
            audit
                .text
                .register_fonts(bytes.to_vec())
                .expect("bundled faces register");
        }
        audit.root.set_theme(Box::new(theme));
        audit
    }

    /// The audit's own two passes, replayed so the offenders can be named: the
    /// skipping pass with the audit off, a snapshot, then the audited layout —
    /// whose skipping pass now finds every pod clean, so it is exactly the
    /// forced pass — and a second snapshot to compare.
    fn layout(&mut self) {
        self.root.set_layout_audit(false);
        self.root
            .layout_with_text(self.window, &mut self.text as &mut dyn Any);
        let skipped = geometry(&self.root);
        self.root.set_layout_audit(true);
        self.root
            .layout_with_text(self.window, &mut self.text as &mut dyn Any);
        for (id, (name, origin, size)) in geometry(&self.root) {
            if let Some((_, was_origin, was_size)) = skipped.get(&id) {
                let moved = (origin - *was_origin).hypot() > 1e-3
                    || (size.width - was_size.width).abs() > 1e-3
                    || (size.height - was_size.height).abs() > 1e-3;
                if moved {
                    self.offenders.push(name);
                }
            }
        }
    }

    fn paint(&mut self, scene: &mut dyn PaintScene) -> PaintOutcome {
        let outcome = self.root.paint(scene, FrameTime::from_nanos(self.now));
        self.now += FRAME_NS;
        outcome
    }

    /// A frame the app's state may have changed for: rebuild, layout, paint.
    fn frame(&mut self) -> PaintOutcome {
        self.root.rebuild(&mut self.view, &mut self.state);
        self.layout();
        self.paint(&mut DiscardScene)
    }

    /// A frame nothing rebuilt for (an animation tick, a paint-driven change).
    fn tick(&mut self) -> PaintOutcome {
        self.layout();
        self.paint(&mut DiscardScene)
    }

    /// Tick until nothing owes a frame, then twice more.
    fn settle(&mut self) {
        for _ in 0..600 {
            if !self.tick().needs_frame {
                break;
            }
        }
        self.tick();
        self.tick();
    }

    /// [`Audit::frame`], answering the type of every pod the rebuild left
    /// layout-dirty: the pods the skipping pass will lay out again.
    fn frame_dirty(&mut self) -> Vec<&'static str> {
        self.root.rebuild(&mut self.view, &mut self.state);
        let names: HashMap<WidgetId, &'static str> = self
            .root
            .inspect()
            .into_iter()
            .map(|n| (n.id, short(n.type_name)))
            .collect();
        let dirty = self
            .root
            .inspect_layout_dirty()
            .into_iter()
            .filter(|&(_, dirty)| dirty)
            .filter_map(|(id, _)| names.get(&id).copied())
            .collect();
        self.layout();
        self.paint(&mut DiscardScene);
        dirty
    }

    /// Every pod's type, parent-relative origin and size, in tree order — ids
    /// dropped, so two mounts compare.
    fn shape(&self) -> Vec<(&'static str, Point, Size)> {
        let nodes = self.root.inspect();
        let origins: HashMap<WidgetId, Point> =
            nodes.iter().map(|n| (n.id, n.bounds.origin())).collect();
        nodes
            .iter()
            .map(|n| {
                let parent = n
                    .parent
                    .and_then(|p| origins.get(&p).copied())
                    .unwrap_or(Point::ZERO);
                let origin = Point::new(n.bounds.x0 - parent.x, n.bounds.y0 - parent.y);
                (short(n.type_name), origin, n.bounds.size())
            })
            .collect()
    }

    /// The size of every pod of type `widget`, in tree order.
    fn sizes(&self, widget: &str) -> Vec<Size> {
        self.root
            .inspect()
            .into_iter()
            .filter(|n| short(n.type_name) == widget)
            .map(|n| n.bounds.size())
            .collect()
    }

    fn assert_clean(&self, scenario: &str) {
        assert!(
            self.offenders.is_empty(),
            "{scenario}: stale layout under skipping in {:?}",
            self.offenders
        );
        assert_eq!(
            self.root.layout_audit_mismatches(),
            0,
            "{scenario}: the layout audit reported mismatches"
        );
    }
}

fn light() -> Theme {
    frust_beui::theme()
}

fn reduced() -> Theme {
    let mut theme = frust_beui::theme();
    theme.motion.reduce_motion = true;
    theme
}

fn both_motions() -> [Theme; 2] {
    [light(), reduced()]
}

/// A type name's last path segment: `AlertWidget` for the alert's widget.
fn short(type_name: &'static str) -> &'static str {
    type_name.rsplit("::").next().unwrap_or(type_name)
}

// ---------------------------------------------------------------------------
// The anchored lists' width floor follows their trigger.
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Lists {
    trigger_width: f64,
    query: String,
}

/// Which anchored list a scenario mounts.
#[derive(Clone, Copy, Debug)]
enum List {
    Select,
    Combobox,
    MultiSelect,
}

fn list_view(which: List, anchor: &OverlayAnchor, state: &Lists) -> impl View<Lists> + use<> {
    let fruits = ["Apple", "Banana", "Cherry", "Damson"];
    let trigger: AnyView<Lists> = match which {
        List::Select => any(select_trigger(anchor, Some("Apple".to_string()))),
        List::Combobox => any(combobox_trigger(
            anchor,
            state.query.clone(),
            |_: &mut Lists, _| {},
        )),
        List::MultiSelect => any(multi_select_trigger(
            anchor,
            fruits.iter().map(|f| multi_select_option(*f)).collect(),
            vec![0],
            state.query.clone(),
            |_: &mut Lists, _| {},
        )),
    };
    let list: AnyView<Lists> = match which {
        List::Select => any(select(
            fruits.iter().map(|f| select_option(*f)).collect(),
            Some(0),
            |_: &mut Lists, _| {},
        )
        .anchor(anchor)
        .open(true)),
        List::Combobox => any(combobox(
            fruits.iter().map(|f| combobox_option(*f)).collect(),
            Some(0),
            state.query.clone(),
            |_: &mut Lists, _| {},
        )
        .anchor(anchor)
        .open(true)),
        List::MultiSelect => any(multi_select(
            fruits.iter().map(|f| multi_select_option(*f)).collect(),
            vec![0],
            state.query.clone(),
            |_: &mut Lists, _| {},
        )
        .anchor(anchor)
        .open(true)),
    };
    stack()
        .child(SizedBox(Some(state.trigger_width), Some(40.0)).child(trigger))
        .child(list)
}

fn list_audit(which: List, theme: Theme) -> Audit<Lists> {
    let anchor = OverlayAnchor::new();
    Audit::new(
        Lists {
            trigger_width: 150.0,
            query: String::new(),
        },
        theme,
        move |state: &mut Lists| any(list_view(which, &anchor, state)),
    )
}

#[test]
fn anchored_lists_follow_a_trigger_that_resizes() {
    for which in [List::Select, List::Combobox, List::MultiSelect] {
        for theme in both_motions() {
            let mut audit = list_audit(which, theme);
            audit.frame();
            audit.settle();
            for width in [340.0, 200.0, 380.0] {
                audit.state.trigger_width = width;
                // One rebuild for the resize, then only ticks: the trigger's
                // paint writes the anchor cell after the list's last layout.
                audit.frame();
                audit.settle();
            }
            audit.assert_clean(&format!("{which:?} over a resizing trigger"));
        }
    }
}

#[test]
fn filtering_lists_relayout_as_their_rows_come_and_go() {
    for which in [List::Combobox, List::MultiSelect] {
        for theme in both_motions() {
            let mut audit = list_audit(which, theme);
            audit.frame();
            audit.settle();
            for query in ["an", "zzz", "", "ch", ""] {
                audit.state.query = query.to_string();
                audit.frame();
                audit.settle();
            }
            audit.assert_clean(&format!("{which:?} filtering"));
        }
    }
}

// ---------------------------------------------------------------------------
// The sized blocks: island, expandable tabs, notification stack.
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Blocks {
    island: Option<String>,
    tab: Option<String>,
    expanded: bool,
}

fn blocks(state: &mut Blocks) -> AnyView<Blocks> {
    any(column()
        .child(dynamic_island(
            state.island.clone(),
            SizedBox(Some(60.0), Some(16.0)),
            vec![
                dynamic_island_slot("music", SizedBox(Some(220.0), Some(64.0))),
                dynamic_island_slot("call", SizedBox(Some(160.0), Some(40.0))),
            ],
        ))
        .child(expandable_tabs(
            state.tab.clone(),
            vec![
                expandable_tabs_item(
                    "home",
                    "Home",
                    SizedBox(Some(16.0), Some(16.0)),
                    SizedBox(Some(120.0), Some(60.0)),
                ),
                expandable_tabs_item(
                    "inbox",
                    "Inbox",
                    SizedBox(Some(16.0), Some(16.0)),
                    SizedBox(Some(120.0), Some(90.0)),
                ),
            ],
            |state: &mut Blocks, tab| state.tab = tab,
        ))
        .child(notification_stack(
            vec![
                notification("a", "Build finished"),
                notification("b", "Review requested").description("2 files"),
                notification("c", "Deploy queued"),
            ],
            state.expanded,
            |state: &mut Blocks, expanded| state.expanded = expanded,
        ))
        .child(SizedBox(Some(100.0), Some(20.0))))
}

#[test]
fn the_sized_blocks_stay_laid_out_through_their_morphs() {
    for theme in both_motions() {
        let mut audit = Audit::new(Blocks::default(), theme, blocks);
        audit.frame();
        audit.settle();
        let steps: [(Option<&str>, Option<&str>, bool); 4] = [
            (Some("music"), Some("home"), true),
            (Some("call"), Some("inbox"), false),
            (None, None, true),
            (Some("music"), Some("home"), false),
        ];
        for (island, tab, expanded) in steps {
            audit.state.island = island.map(str::to_string);
            audit.state.tab = tab.map(str::to_string);
            audit.state.expanded = expanded;
            audit.frame();
            audit.settle();
        }
        audit.assert_clean("island + expandable tabs + notification stack");
    }
}

// ---------------------------------------------------------------------------
// The input's message row.
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Field {
    error: Option<String>,
}

fn field(state: &mut Field) -> AnyView<Field> {
    let mut email = input(String::from("ada@"), |_: &mut Field, _| {}).label("Email");
    if let Some(error) = &state.error {
        email = email.error(error.clone());
    }
    any(column()
        .child(email)
        .child(SizedBox(Some(100.0), Some(20.0))))
}

#[test]
fn an_input_message_row_comes_and_goes_through_layout() {
    for theme in both_motions() {
        let mut audit = Audit::new(Field::default(), theme, field);
        audit.frame();
        audit.settle();
        for error in [Some("Enter a valid email"), None, Some("Required"), None] {
            audit.state.error = error.map(str::to_string);
            audit.frame();
            audit.settle();
        }
        audit.assert_clean("input message row");
    }
}

// ---------------------------------------------------------------------------
// The phone-content parts: the shared drivers.
// ---------------------------------------------------------------------------

/// A part scenario's view function: a plain `fn`, so a fresh copy can be mounted.
type PartView<S> = fn(&mut S) -> AnyView<S>;

/// A theme a part scenario mounts or switches to.
type ThemeFn = fn() -> Theme;

fn comfortable() -> Theme {
    frust_beui::theme_with_density(Density::Comfortable)
}

fn touch() -> Theme {
    frust_beui::theme_with_density(Density::Touch)
}

/// beUI's theme under an OS text scale of `factor`.
fn text_scale(factor: f32) -> Theme {
    let theme = frust_beui::theme();
    let authored = theme.authored_type_scale();
    theme.with_text_scale(&authored, factor)
}

fn unscaled() -> Theme {
    text_scale(1.0)
}

fn enlarged() -> Theme {
    text_scale(1.5)
}

/// Both densities the unchanged-rebuild and toggle scenarios run under.
const DENSITIES: [ThemeFn; 2] = [comfortable, touch];

/// Mount `state` under `theme` and settle it.
fn mount<S: Clone + 'static>(state: &S, theme: Theme, view: PartView<S>) -> Audit<S> {
    let mut audit = Audit::new(state.clone(), theme, view);
    audit.frame();
    audit.settle();
    audit
}

/// The live tree laid out exactly what a fresh mount of its state under `theme` does.
fn assert_matches_fresh<S: Clone + 'static>(
    audit: &Audit<S>,
    theme: Theme,
    view: PartView<S>,
    scenario: &str,
) {
    let fresh = mount(&audit.state, theme, view).shape();
    let live = audit.shape();
    assert_eq!(
        live.len(),
        fresh.len(),
        "{scenario}: the live tree has {} pods, a fresh mount {}",
        live.len(),
        fresh.len()
    );
    for ((name, origin, size), (fresh_name, fresh_origin, fresh_size)) in live.iter().zip(&fresh) {
        assert_eq!(name, fresh_name, "{scenario}: the trees differ in shape");
        let off = (*origin - *fresh_origin).hypot() > 1e-3
            || (size.width - fresh_size.width).abs() > 1e-3
            || (size.height - fresh_size.height).abs() > 1e-3;
        assert!(
            !off,
            "{scenario}: {name} laid out at {origin:?} {size:?}, a fresh mount at \
             {fresh_origin:?} {fresh_size:?}"
        );
    }
}

/// Two unchanged rebuilds move nothing and keep the audit clean. Answers every
/// pod they left layout-dirty.
fn quiet_rebuild<S: Clone + 'static>(audit: &mut Audit<S>, scenario: &str) -> Vec<&'static str> {
    let before = audit.shape();
    let mut dirty = Vec::new();
    for _ in 0..2 {
        dirty.extend(audit.frame_dirty());
        audit.settle();
    }
    assert_eq!(
        audit.shape(),
        before,
        "{scenario}: an unchanged rebuild moved a pod"
    );
    audit.assert_clean(scenario);
    dirty
}

/// An unchanged rebuild leaves no pod layout-dirty, moves nothing and keeps the
/// audit clean.
fn assert_quiet_rebuild<S: Clone + 'static>(audit: &mut Audit<S>, scenario: &str) {
    let dirty = quiet_rebuild(audit, scenario);
    assert!(
        dirty.is_empty(),
        "{scenario}: an unchanged rebuild left {dirty:?} layout-dirty"
    );
}

/// Move `audit` to `next`: the rebuild dirties a `widget` pod, the audit stays
/// clean, and the result matches a fresh mount of `next` under `theme`.
fn step<S: Clone + 'static>(
    audit: &mut Audit<S>,
    next: S,
    theme: ThemeFn,
    view: PartView<S>,
    widget: &str,
    scenario: &str,
) {
    audit.state = next;
    let dirty = audit.frame_dirty();
    assert!(
        dirty.contains(&widget),
        "{scenario}: the rebuild left every {widget} clean (dirty: {dirty:?})"
    );
    audit.settle();
    audit.assert_clean(scenario);
    assert_matches_fresh(audit, theme(), view, scenario);
}

/// The size of every `widget` pod before and after one live theme change.
struct Retheme {
    change: &'static str,
    before: Vec<Size>,
    after: Vec<Size>,
}

/// Drive a mount of `state` through both live theme changes — Comfortable to
/// Touch density, and an OS text scale of 1.0 to 1.5. Each must keep the audit
/// clean and match a fresh mount under the new theme.
fn theme_changes<S: Clone + 'static>(
    state: &S,
    view: PartView<S>,
    widget: &str,
    scenario: &str,
) -> [Retheme; 2] {
    let changes: [(ThemeFn, ThemeFn, &'static str); 2] = [
        (comfortable, touch, "Comfortable to Touch"),
        (unscaled, enlarged, "text scale 1.0 to 1.5"),
    ];
    changes.map(|(from, to, change)| {
        let scenario = format!("{scenario}, {change}");
        let mut audit = mount(state, from(), view);
        let before = audit.sizes(widget);
        assert!(!before.is_empty(), "{scenario}: no {widget} mounted");
        audit.root.set_theme(Box::new(to()));
        audit.frame();
        audit.settle();
        audit.assert_clean(&scenario);
        assert_matches_fresh(&audit, to(), view, &scenario);
        Retheme {
            change,
            before,
            after: audit.sizes(widget),
        }
    })
}

/// Every pod in `retheme` grew taller under its theme change.
fn assert_taller(retheme: &Retheme, what: &str) {
    assert_eq!(retheme.before.len(), retheme.after.len());
    for (i, (before, after)) in retheme.before.iter().zip(&retheme.after).enumerate() {
        assert!(
            after.height > before.height + 1e-3,
            "{what} #{i}, {}: height {} did not grow (now {})",
            retheme.change,
            before.height,
            after.height
        );
    }
}

/// The first `widget` pod's size.
fn size_of<S: 'static>(audit: &Audit<S>, widget: &str) -> Size {
    audit
        .sizes(widget)
        .first()
        .copied()
        .unwrap_or_else(|| panic!("no {widget} mounted"))
}

/// A fixed-size stand-in child.
fn block<S: 'static>(width: f64, height: f64) -> SizedBoxView<S> {
    SizedBox(Some(width), Some(height))
}

// ---------------------------------------------------------------------------
// Alert.
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct Notice {
    title: String,
    message: Option<String>,
    dismissible: bool,
}

fn notice() -> Notice {
    Notice {
        title: "Saved".to_string(),
        message: Some("Two files are still uploading".to_string()),
        dismissible: true,
    }
}

fn notice_view(state: &mut Notice) -> AnyView<Notice> {
    let mut part = alert(AlertTone::Warning, state.title.clone())
        .actions([block(64.0, 28.0), block(64.0, 28.0)]);
    if let Some(message) = &state.message {
        part = part.message(message.clone());
    }
    if state.dismissible {
        part = part.on_dismiss(|_: &mut Notice| {});
    }
    any(column().child(part).child(block(100.0, 20.0)))
}

#[test]
fn an_alert_skips_an_unchanged_rebuild() {
    for theme in DENSITIES {
        let mut audit = mount(&notice(), theme(), notice_view);
        assert_quiet_rebuild(&mut audit, "alert");
    }
}

#[test]
fn an_alert_relays_out_a_longer_title() {
    let mut audit = mount(&notice(), comfortable(), notice_view);
    let short = size_of(&audit, "AlertWidget");
    let next = Notice {
        title: "Your changes were saved, but two attachments could not be uploaded yet".to_string(),
        ..notice()
    };
    step(
        &mut audit,
        next,
        comfortable,
        notice_view,
        "AlertWidget",
        "alert, longer title",
    );
    let long = size_of(&audit, "AlertWidget");
    assert!(
        long.height > short.height,
        "a wrapped title must grow the alert: {short:?} -> {long:?}"
    );
}

#[test]
fn an_alert_relays_out_under_density_and_text_scale() {
    for retheme in theme_changes(&notice(), notice_view, "AlertWidget", "alert") {
        assert_taller(&retheme, "alert");
    }
}

#[test]
fn an_alert_relays_out_its_dismiss_control_and_message_coming_and_going() {
    let message = Some("Two files are still uploading".to_string());
    for theme in DENSITIES {
        let mut audit = mount(&notice(), theme(), notice_view);
        let mut heights = Vec::new();
        for (message, dismissible) in [
            (None, true),
            (None, false),
            (message.clone(), false),
            (message.clone(), true),
        ] {
            let next = Notice {
                message,
                dismissible,
                ..notice()
            };
            step(
                &mut audit,
                next,
                theme,
                notice_view,
                "AlertWidget",
                "alert toggles",
            );
            heights.push(size_of(&audit, "AlertWidget").height);
        }
        assert!(
            heights[2] > heights[1],
            "a message must grow the alert: {heights:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Avatar.
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct Person {
    initials: String,
    size: AvatarSize,
}

fn person() -> Person {
    Person {
        initials: "A".to_string(),
        size: AvatarSize::Md,
    }
}

fn person_view(state: &mut Person) -> AnyView<Person> {
    any(column()
        .child(avatar(state.initials.clone()).size(state.size))
        .child(block(100.0, 20.0)))
}

#[test]
fn an_avatar_skips_an_unchanged_rebuild() {
    for theme in DENSITIES {
        let mut audit = mount(&person(), theme(), person_view);
        assert_quiet_rebuild(&mut audit, "avatar");
    }
}

#[test]
fn an_avatar_relays_out_new_initials_and_a_new_size() {
    let mut audit = mount(&person(), comfortable(), person_view);
    let initials = Person {
        initials: "al".to_string(),
        ..person()
    };
    step(
        &mut audit,
        initials.clone(),
        comfortable,
        person_view,
        "AvatarWidget",
        "avatar, initials",
    );
    // The initials sit inside a fixed circle: the edge holds.
    assert_eq!(size_of(&audit, "AvatarWidget"), Size::new(28.0, 28.0));
    let larger = Person {
        size: AvatarSize::Lg,
        ..initials
    };
    step(
        &mut audit,
        larger,
        comfortable,
        person_view,
        "AvatarWidget",
        "avatar, size",
    );
    assert_eq!(size_of(&audit, "AvatarWidget"), Size::new(40.0, 40.0));
}

#[test]
fn an_avatar_relays_out_under_density_and_text_scale() {
    let [density, scale] = theme_changes(&person(), person_view, "AvatarWidget", "avatar");
    // An avatar is not a touch target: its edge does not follow the density.
    assert_eq!(density.before, density.after, "avatar, {}", density.change);
    // The edge follows the OS text scale with the initials.
    assert_eq!(
        scale.after,
        vec![Size::new(42.0, 42.0)],
        "avatar, {}",
        scale.change
    );
}

// ---------------------------------------------------------------------------
// Card, hero card and stat tile.
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct Cards {
    /// Width of the strip beside the plain card's avatar.
    strip: f64,
    pressable: bool,
    subtitle: Option<String>,
    actions: bool,
    value: String,
    trailing: bool,
}

fn cards() -> Cards {
    Cards {
        strip: 40.0,
        pressable: true,
        subtitle: None,
        actions: true,
        value: "12".to_string(),
        trailing: true,
    }
}

/// A plain card, a hero card whose three actions wrap onto two rows, and a stat
/// tile in a row (which hands it an unbounded width, so it sizes to its text).
fn cards_view(state: &mut Cards) -> AnyView<Cards> {
    let mut plain = card(row().child(avatar("PL")).child(block(state.strip, 8.0))).padding(4.0);
    if state.pressable {
        plain = plain.on_press(|_: &mut Cards| {});
    }
    let mut hero = hero_card("Weekly plan", block(120.0, 24.0));
    if let Some(subtitle) = &state.subtitle {
        hero = hero.subtitle(subtitle.clone());
    }
    if state.actions {
        hero = hero.actions([block(120.0, 32.0), block(120.0, 32.0), block(120.0, 32.0)]);
    }
    let mut stat = stat_tile("Revenue", state.value.clone());
    if state.trailing {
        stat = stat.trailing(block(24.0, 24.0));
    }
    any(column()
        .child(plain)
        .child(hero)
        .child(row().child(stat))
        .child(block(100.0, 20.0)))
}

/// The plain card, hero card and stat tile sizes, in that order.
fn card_sizes(audit: &Audit<Cards>) -> [Size; 3] {
    let sizes = audit.sizes("CardWidget");
    [sizes[0], sizes[1], sizes[2]]
}

#[test]
fn cards_skip_an_unchanged_rebuild() {
    for theme in DENSITIES {
        let mut audit = mount(&cards(), theme(), cards_view);
        assert_quiet_rebuild(&mut audit, "cards");
    }
}

#[test]
fn cards_relay_out_their_content_changing() {
    let mut audit = mount(&cards(), comfortable(), cards_view);
    let [plain, hero, stat] = card_sizes(&audit);

    let wider = Cards {
        strip: 160.0,
        ..cards()
    };
    step(
        &mut audit,
        wider.clone(),
        comfortable,
        cards_view,
        "CardWidget",
        "card, wider child",
    );
    assert!(
        card_sizes(&audit)[0].width > plain.width,
        "the plain card shrinks to its child"
    );

    let subtitled = Cards {
        subtitle: Some("Three sessions left this week".to_string()),
        ..wider
    };
    step(
        &mut audit,
        subtitled.clone(),
        comfortable,
        cards_view,
        "CardWidget",
        "hero card, subtitle",
    );
    assert!(
        card_sizes(&audit)[1].height > hero.height,
        "a subtitle grows the hero card"
    );

    let longer = Cards {
        value: "1,284,330".to_string(),
        ..subtitled
    };
    step(
        &mut audit,
        longer,
        comfortable,
        cards_view,
        "CardWidget",
        "stat tile, longer value",
    );
    assert!(
        card_sizes(&audit)[2].width > stat.width,
        "a longer value widens the stat tile"
    );
}

#[test]
fn cards_relay_out_under_density_and_text_scale() {
    for retheme in theme_changes(&cards(), cards_view, "CardWidget", "cards") {
        assert_taller(&retheme, "card");
    }
}

#[test]
fn cards_relay_out_their_press_actions_and_trailing_coming_and_going() {
    for (theme, floored) in [(comfortable as ThemeFn, false), (touch, true)] {
        let mut audit = mount(&cards(), theme(), cards_view);
        let [plain, hero, stat] = card_sizes(&audit);
        let bare = Cards {
            pressable: false,
            actions: false,
            trailing: false,
            ..cards()
        };
        step(
            &mut audit,
            bare,
            theme,
            cards_view,
            "CardWidget",
            "cards, toggles off",
        );
        let [bare_plain, bare_hero, bare_stat] = card_sizes(&audit);
        if floored {
            assert!(
                plain.height > bare_plain.height,
                "a pressable card clears the touch target"
            );
        } else {
            assert_eq!(
                plain, bare_plain,
                "the comfortable set imposes no touch floor"
            );
        }
        assert!(hero.height > bare_hero.height, "the action rows take room");
        assert!(stat.width > bare_stat.width, "the trailing view takes room");
        step(
            &mut audit,
            cards(),
            theme,
            cards_view,
            "CardWidget",
            "cards, toggles on",
        );
        assert_eq!(card_sizes(&audit), [plain, hero, stat]);
    }
}

// ---------------------------------------------------------------------------
// Chip and status badge.
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct Tags {
    label: String,
    removable: bool,
    badge: String,
}

fn tags() -> Tags {
    Tags {
        label: "Design".to_string(),
        removable: true,
        badge: "Live".to_string(),
    }
}

fn tags_view(state: &mut Tags) -> AnyView<Tags> {
    let mut part = chip(state.label.clone()).on_press(|_: &mut Tags| {});
    if state.removable {
        part = part.on_remove(|_: &mut Tags| {});
    }
    any(column()
        .child(
            row()
                .child(part)
                .child(status_badge(state.badge.clone(), StatusBadgeTone::Success).dot(true)),
        )
        .child(block(100.0, 20.0)))
}

#[test]
fn chips_skip_an_unchanged_rebuild() {
    for theme in DENSITIES {
        let mut audit = mount(&tags(), theme(), tags_view);
        assert_quiet_rebuild(&mut audit, "chips");
    }
}

#[test]
fn a_chip_places_its_leading_child_in_layout() {
    let view: PartView<Tags> = |state| {
        any(row().child(
            chip(state.label.clone())
                .leading(block(16.0, 16.0))
                .on_press(|_: &mut Tags| {}),
        ))
    };
    for theme in DENSITIES {
        let mut audit = Audit::new(tags(), theme(), view);
        // Rebuild and layout only: no paint has run, so the position must
        // already be final.
        audit.root.rebuild(&mut audit.view, &mut audit.state);
        audit.layout();
        let before = audit.shape();
        audit.frame();
        audit.settle();
        assert_eq!(
            audit.shape(),
            before,
            "the leading child moved when the chip painted"
        );
        assert_quiet_rebuild(&mut audit, "chip with a leading view");
    }
}

#[test]
fn chips_relay_out_a_longer_label() {
    let mut audit = mount(&tags(), comfortable(), tags_view);
    let before = audit.sizes("ChipWidget");
    let next = Tags {
        label: "Design systems".to_string(),
        badge: "Live since Monday".to_string(),
        ..tags()
    };
    step(
        &mut audit,
        next,
        comfortable,
        tags_view,
        "ChipWidget",
        "chips, longer labels",
    );
    for (before, after) in before.iter().zip(audit.sizes("ChipWidget")) {
        assert!(
            after.width > before.width,
            "a longer label widens: {before:?} -> {after:?}"
        );
    }
}

#[test]
fn chips_relay_out_under_density_and_text_scale() {
    for retheme in theme_changes(&tags(), tags_view, "ChipWidget", "chips") {
        assert_taller(&retheme, "chip");
    }
}

#[test]
fn a_chip_relays_out_its_remove_control_coming_and_going() {
    for theme in DENSITIES {
        let mut audit = mount(&tags(), theme(), tags_view);
        let removable = size_of(&audit, "ChipWidget");
        let fixed = Tags {
            removable: false,
            ..tags()
        };
        step(
            &mut audit,
            fixed,
            theme,
            tags_view,
            "ChipWidget",
            "chip, remove off",
        );
        assert!(
            size_of(&audit, "ChipWidget").width < removable.width,
            "the remove control takes room"
        );
        step(
            &mut audit,
            tags(),
            theme,
            tags_view,
            "ChipWidget",
            "chip, remove on",
        );
        assert_eq!(size_of(&audit, "ChipWidget"), removable);
    }
}

// ---------------------------------------------------------------------------
// Grouped list.
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct Settings {
    rows: usize,
    value: bool,
    subtitle: bool,
}

fn settings() -> Settings {
    Settings {
        rows: 2,
        value: true,
        subtitle: false,
    }
}

fn settings_view(state: &mut Settings) -> AnyView<Settings> {
    let rows = (0..state.rows).map(|i| {
        let mut part = list_row(format!("Setting {i}"))
            .chevron(true)
            .on_press(|_: &mut Settings| {});
        if state.value {
            part = part.value("On");
        }
        if state.subtitle {
            part = part.subtitle("Applies to every device");
        }
        part
    });
    any(column()
        .child(section_header("Account"))
        .child(grouped_list(rows))
        .child(block(100.0, 20.0)))
}

#[test]
fn a_grouped_list_skips_an_unchanged_rebuild() {
    for theme in DENSITIES {
        let mut audit = mount(&settings(), theme(), settings_view);
        assert_quiet_rebuild(&mut audit, "grouped list");
    }
}

#[test]
fn a_grouped_list_relays_out_a_row_added() {
    let mut audit = mount(&settings(), comfortable(), settings_view);
    let two = size_of(&audit, "GroupedListWidget");
    let next = Settings {
        rows: 3,
        ..settings()
    };
    step(
        &mut audit,
        next,
        comfortable,
        settings_view,
        "GroupedListWidget",
        "list, row added",
    );
    let three = size_of(&audit, "GroupedListWidget");
    assert!(
        three.height > two.height,
        "a third row grows the group: {two:?} -> {three:?}"
    );
}

#[test]
fn a_grouped_list_relays_out_under_density_and_text_scale() {
    for widget in ["GroupedListWidget", "ListRowWidget", "SectionHeaderWidget"] {
        for retheme in theme_changes(&settings(), settings_view, widget, "grouped list") {
            assert_taller(&retheme, widget);
        }
    }
}

#[test]
fn a_list_row_relays_out_its_value_and_subtitle_coming_and_going() {
    for theme in DENSITIES {
        let mut audit = mount(&settings(), theme(), settings_view);
        let plain = size_of(&audit, "ListRowWidget");
        for (value, subtitle) in [(false, false), (false, true), (true, true), (true, false)] {
            let next = Settings {
                value,
                subtitle,
                ..settings()
            };
            step(
                &mut audit,
                next,
                theme,
                settings_view,
                "ListRowWidget",
                "list row toggles",
            );
            let size = size_of(&audit, "ListRowWidget");
            if subtitle {
                assert!(size.height > plain.height, "a subtitle grows the row");
            } else {
                assert_eq!(size, plain, "a value sits inside the row's height");
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Key-value row and timeline.
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct Details {
    value: String,
    stacked: bool,
    trailing: bool,
    steps: usize,
}

fn details() -> Details {
    Details {
        value: "#A-1042".to_string(),
        stacked: false,
        trailing: true,
        steps: 2,
    }
}

fn details_view(state: &mut Details) -> AnyView<Details> {
    let mut part = key_value_row("Order", state.value.clone()).stacked(state.stacked);
    if state.trailing {
        // Shorter than the text, so the row's height follows the type.
        part = part.trailing(block(16.0, 12.0));
    }
    let steps = (0..state.steps).map(|i| {
        let stage = if i == 0 {
            TimelineStepState::Done
        } else {
            TimelineStepState::Current
        };
        TimelineStep::new(format!("Stage {i}"), stage).detail("Today")
    });
    any(column()
        .child(part)
        .child(timeline(steps))
        .child(block(100.0, 20.0)))
}

/// The key-value row and timeline sizes, in that order.
fn detail_sizes(audit: &Audit<Details>) -> [Size; 2] {
    let sizes = audit.sizes("KeyValueWidget");
    [sizes[0], sizes[1]]
}

#[test]
fn key_values_skip_an_unchanged_rebuild() {
    for theme in DENSITIES {
        let mut audit = mount(&details(), theme(), details_view);
        assert_quiet_rebuild(&mut audit, "key-value");
    }
}

#[test]
fn key_values_relay_out_a_longer_value_and_a_step_added() {
    let mut audit = mount(&details(), comfortable(), details_view);
    let [row_size, timeline_size] = detail_sizes(&audit);
    let longer = Details {
        value: "Shipped to 1200 Market Street, Suite 400, Springfield".to_string(),
        ..details()
    };
    step(
        &mut audit,
        longer.clone(),
        comfortable,
        details_view,
        "KeyValueWidget",
        "row, longer value",
    );
    assert!(
        detail_sizes(&audit)[0].height > row_size.height,
        "a value too long for the line stacks under the key"
    );
    let more = Details { steps: 3, ..longer };
    step(
        &mut audit,
        more,
        comfortable,
        details_view,
        "KeyValueWidget",
        "timeline, step added",
    );
    assert!(
        detail_sizes(&audit)[1].height > timeline_size.height,
        "a third step grows the timeline"
    );
}

#[test]
fn key_values_relay_out_under_density_and_text_scale() {
    for retheme in theme_changes(&details(), details_view, "KeyValueWidget", "key-value") {
        assert_taller(&retheme, "key-value");
    }
}

#[test]
fn a_key_value_row_relays_out_its_trailing_and_stacking_coming_and_going() {
    for theme in DENSITIES {
        let mut audit = mount(&details(), theme(), details_view);
        let inline = detail_sizes(&audit)[0];
        for (stacked, trailing) in [(false, false), (true, false), (true, true), (false, true)] {
            let next = Details {
                stacked,
                trailing,
                ..details()
            };
            step(
                &mut audit,
                next,
                theme,
                details_view,
                "KeyValueWidget",
                "row toggles",
            );
            let size = detail_sizes(&audit)[0];
            if stacked {
                assert!(size.height > inline.height, "a stacked row is taller");
            }
        }
        assert_eq!(detail_sizes(&audit)[0], inline);
    }
}
