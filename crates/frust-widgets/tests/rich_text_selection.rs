//! A `selectable` rich_text block through a real `RenderRoot`: a touch
//! long-press selecting a word with handles and the toolbar request, a handle
//! drag, mouse press-drag and double/triple click, copy into the pass's
//! clipboard slot, select-all, focus loss, links beside selection, the inert
//! default, wrapped and bidi highlight rects, scrolling and disposal.
//!
//! Positions inside the text come from a `TextLayout` of the same text and
//! style made in the same `TextContext` the root lays the block out with, so
//! they land on the glyphs the block drew.

use std::sync::{Arc, Mutex};

use frust_core::accesskit::Role;
use frust_core::event::PointerId;
use frust_core::{
    BoxConstraints, BuildCtx, ChangeFlags, EditCommand, EventCtx, EventResult, FrameTime,
    InputEvent, Key, KeyEvent, LayoutCtx, Modifiers, NamedKey, PaintCtx, PaintScene, PointerButton,
    PointerEvent, PointerPhase, RenderRoot, SelectionToolbarActions, SelectionToolbarBuilder,
    SelectionToolbarPolicy, SelectionToolbarRequest, View, Widget, set_selection_toolbar_builder,
    set_selection_toolbar_policy,
};
use frust_scene::GlyphRun;
use frust_text::{TextAffinity, TextContext, TextLayout, TextPosition, TextStyle};
use frust_widgets::{FlexView, column, rich_text, scroll_view, span, text_input};
use kurbo::{Point, Rect, Size, Vec2};
use peniko::Color;

/// The process-global toolbar policy and builder are shared by every test
/// here, so every test holds this lock.
static POLICY: Mutex<()> = Mutex::new(());

/// The unthemed selection highlight.
const SELECTION: Color = Color::from_rgb8(0xBF, 0xDB, 0xFE);
/// The handle knob's radius.
const KNOB: f64 = 6.0;
const LONG_PRESS_MS: f64 = 500.0;
const WIDTH: f64 = 300.0;

#[derive(Default)]
struct App {
    opened: Vec<String>,
    field: String,
    show: bool,
}

#[derive(Default)]
struct Scene {
    highlights: Vec<Rect>,
    knobs: Vec<Rect>,
    /// Where each glyph run was placed: the block's text origin.
    origins: Vec<Point>,
}

impl PaintScene for Scene {
    fn fill_rect(&mut self, o: Point, s: Size, c: Color) {
        if c == SELECTION {
            self.highlights.push(Rect::from_origin_size(o, s));
        }
    }
    fn fill_rounded_rect(&mut self, o: Point, s: Size, radius: f64, _c: Color) {
        if radius == KNOB && s == Size::new(2.0 * KNOB, 2.0 * KNOB) {
            self.knobs.push(Rect::from_origin_size(o, s));
        }
    }
    fn draw_text(&mut self, _o: Point, _t: &str) {}
    fn draw_glyph_run(&mut self, run: GlyphRun) {
        let t = run.transform.translation();
        self.origins.push(Point::new(t.x, t.y));
    }
}

type Logic = Box<dyn FnMut(&mut App) -> FlexView<App>>;

struct Harness {
    root: RenderRoot<App, FlexView<App>>,
    logic: Logic,
    app: App,
    tcx: TextContext,
    window: Size,
    /// The block's text origin as last painted.
    origin: Point,
    scene: Scene,
}

impl Harness {
    fn new(logic: impl FnMut(&mut App) -> FlexView<App> + 'static) -> Self {
        let mut h = Self {
            root: RenderRoot::new(),
            logic: Box::new(logic),
            app: App {
                show: true,
                ..App::default()
            },
            tcx: TextContext::new(),
            window: Size::new(WIDTH, 400.0),
            origin: Point::ZERO,
            scene: Scene::default(),
        };
        h.frame(0.0);
        h
    }

    /// One whole frame: rebuild, layout, paint at `ms`.
    fn frame(&mut self, ms: f64) {
        self.root.rebuild(&mut self.logic, &mut self.app);
        self.root
            .layout_with_text(self.window, &mut self.tcx as &mut dyn std::any::Any);
        let mut scene = Scene::default();
        self.root
            .paint(&mut scene, FrameTime::from_nanos((ms * 1e6) as u64));
        if let Some(origin) = scene.origins.first() {
            self.origin = *origin;
        }
        self.scene = scene;
    }

    fn send(&mut self, event: InputEvent) -> bool {
        self.root.event(&mut self.app, &event).handled
    }

    fn pointer(&mut self, phase: PointerPhase, at: Point, touch: bool) -> bool {
        let event = PointerEvent {
            phase,
            position: at,
            button: PointerButton::Primary,
        };
        self.send(if touch {
            InputEvent::PointerContact {
                pointer_id: PointerId::touch(0),
                event,
            }
        } else {
            InputEvent::Pointer(event)
        })
    }

    fn click(&mut self, at: Point) {
        self.pointer(PointerPhase::Down, at, false);
        self.pointer(PointerPhase::Up, at, false);
    }

    /// A stationary touch long-press at `at`, finger still down.
    fn hold(&mut self, at: Point) {
        self.pointer(PointerPhase::Down, at, true);
        self.frame(10.0);
        self.frame(10.0 + LONG_PRESS_MS + 50.0);
        self.send(InputEvent::Housekeeping);
    }

    fn key(&mut self, key: Key, modifiers: Modifiers) -> bool {
        self.send(InputEvent::Key(KeyEvent {
            key,
            modifiers,
            repeat: false,
        }))
    }

    /// What a Ctrl+C copies right now.
    fn copied(&mut self) -> Option<String> {
        let _ = self.root.take_clipboard_write();
        let ctrl = Modifiers {
            ctrl: true,
            ..Modifiers::default()
        };
        self.key(Key::Character("c".into()), ctrl);
        self.root.take_clipboard_write()
    }

    /// A layout of `text` as the block lays it out.
    fn layout(&mut self, text: &str) -> TextLayout {
        self.tcx
            .layout(text, &TextStyle::default(), Some(WIDTH as f32))
    }

    /// The window point of the middle of byte range `range`'s first rect.
    fn at(&mut self, text: &str, range: std::ops::Range<usize>) -> Point {
        let rect = self.layout(text).range_rects(range)[0];
        rect.center() + self.origin.to_vec2()
    }

    /// The window point just right of caret `index`, mid-line.
    fn caret(&mut self, text: &str, index: usize) -> Point {
        let caret = self
            .layout(text)
            .caret_rect(TextPosition::new(index, TextAffinity::Downstream), 0.0);
        Point::new(caret.x0 + 0.5, caret.center().y) + self.origin.to_vec2()
    }

    fn rich_text_node(&self) -> frust_core::accesskit::Node {
        self.root
            .semantics()
            .nodes
            .into_iter()
            .find(|(_, node)| node.role() == Role::Label)
            .map(|(_, node)| node)
            .expect("the paragraph node")
    }
}

const TEXT: &str = "alpha beta gamma";

fn block(app: &mut App) -> FlexView<App> {
    column().when(app.show, |c| {
        c.child(rich_text([span(TEXT)]).selectable(true))
    })
}

fn framework_policy() -> std::sync::MutexGuard<'static, ()> {
    let guard = POLICY.lock().unwrap_or_else(|e| e.into_inner());
    set_selection_toolbar_policy(SelectionToolbarPolicy::Framework);
    guard
}

#[test]
fn a_long_press_selects_the_word_with_handles_and_the_toolbar() {
    let _policy = framework_policy();
    let mut h = Harness::new(block);
    let beta = h.at(TEXT, 6..10);
    h.hold(beta);
    h.pointer(PointerPhase::Up, beta, true);
    h.frame(700.0);
    assert_eq!(h.scene.knobs.len(), 2, "a handle at each end");
    assert!(!h.scene.highlights.is_empty(), "the selection paints");
    let request = h.root.selection_toolbar().expect("the block publishes");
    assert!(request.present_menu, "the long-press raised the toolbar");
    assert_eq!(
        request.actions,
        SelectionToolbarActions {
            copy: true,
            cut: false,
            paste: false,
            select_all: true,
        }
    );
    assert_eq!(h.copied().as_deref(), Some("beta"));
}

#[test]
fn dragging_the_end_handle_extends_by_character() {
    let _policy = framework_policy();
    let mut h = Harness::new(block);
    let beta = h.at(TEXT, 6..10);
    h.hold(beta);
    h.pointer(PointerPhase::Up, beta, true);
    h.frame(700.0);
    let knobs = h.scene.knobs.clone();
    let knob = knobs
        .iter()
        .max_by(|a, b| a.x0.total_cmp(&b.x0))
        .expect("two knobs")
        .center();
    // The knob hangs below the block's box, out of its hit test; its stem,
    // along the caret inside the line, is what a press reaches here.
    let end = Point::new(knob.x + 4.0, beta.y);
    assert!(
        h.pointer(PointerPhase::Down, end, true),
        "the handle takes it"
    );
    // To just right of the caret before "mma": "beta ga".
    let target = h.caret(TEXT, 13);
    let dy = end.y - target.y;
    h.pointer(
        PointerPhase::Move,
        Point::new(target.x, end.y - dy / 2.0),
        true,
    );
    h.pointer(PointerPhase::Move, Point::new(target.x, end.y), true);
    h.pointer(PointerPhase::Up, Point::new(target.x, end.y), true);
    h.frame(800.0);
    assert!(
        h.root.selection_toolbar().is_some_and(|r| r.present_menu),
        "the toolbar is back once the handle is let go"
    );
    assert_eq!(h.copied().as_deref(), Some("beta ga"));
}

#[test]
fn a_mouse_press_drag_selects_by_character() {
    let _policy = framework_policy();
    let mut h = Harness::new(block);
    let from = h.caret(TEXT, 1);
    let to = h.caret(TEXT, 8);
    assert!(h.pointer(PointerPhase::Down, from, false));
    h.pointer(PointerPhase::Move, Point::new(from.x + 30.0, from.y), false);
    h.pointer(PointerPhase::Move, to, false);
    h.pointer(PointerPhase::Up, to, false);
    h.frame(20.0);
    assert!(h.scene.knobs.is_empty(), "no handles for a mouse selection");
    assert_eq!(h.copied().as_deref(), Some("lpha be"));
}

#[test]
fn a_double_click_selects_a_word_and_a_triple_click_the_block() {
    let _policy = framework_policy();
    let mut h = Harness::new(block);
    let gamma = h.at(TEXT, 11..16);
    h.click(gamma);
    h.pointer(PointerPhase::Down, gamma, false);
    h.pointer(PointerPhase::Up, gamma, false);
    assert_eq!(h.copied().as_deref(), Some("gamma"));
    h.click(gamma);
    assert_eq!(h.copied().as_deref(), Some(TEXT));
}

#[test]
fn a_held_mouse_press_is_no_long_press() {
    let _policy = framework_policy();
    let mut h = Harness::new(block);
    let beta = h.at(TEXT, 6..10);
    h.pointer(PointerPhase::Down, beta, false);
    h.frame(10.0);
    h.frame(10.0 + LONG_PRESS_MS + 50.0);
    h.send(InputEvent::Housekeeping);
    h.pointer(PointerPhase::Up, beta, false);
    assert_eq!(h.copied(), None);
}

#[test]
fn copy_and_select_all_answer_the_chords_keys_and_commands() {
    let _policy = framework_policy();
    let mut h = Harness::new(block);
    let alpha = h.at(TEXT, 0..5);
    h.click(alpha);
    h.click(alpha);
    // The hardware copy key and a shell-delivered command take the same road.
    let _ = h.root.take_clipboard_write();
    h.key(Key::Named(NamedKey::Copy), Modifiers::default());
    assert_eq!(h.root.take_clipboard_write().as_deref(), Some("alpha"));
    h.send(InputEvent::EditCommand(EditCommand::Copy));
    assert_eq!(h.root.take_clipboard_write().as_deref(), Some("alpha"));
    // Cut and paste are answered with nothing.
    h.send(InputEvent::EditCommand(EditCommand::Cut));
    assert_eq!(h.root.take_clipboard_write(), None);
    let meta = Modifiers {
        meta: true,
        ..Modifiers::default()
    };
    assert!(h.key(Key::Character("a".into()), meta));
    assert_eq!(h.copied().as_deref(), Some(TEXT));
    h.frame(30.0);
    let request = h.root.selection_toolbar().expect("published while focused");
    assert!(!request.actions.select_all, "already wholly selected");
    // Escape clears, and a bare one is declined for a modal around the block.
    assert!(!h.key(Key::Named(NamedKey::Escape), Modifiers::default()));
    assert_eq!(h.copied(), None);
}

#[test]
fn focusing_a_text_field_clears_the_selection() {
    let _policy = framework_policy();
    let mut h = Harness::new(|app: &mut App| {
        column()
            .child(rich_text([span(TEXT)]).selectable(true))
            .child(text_input(app.field.clone(), |app: &mut App, v| {
                app.field = v
            }))
    });
    let beta = h.at(TEXT, 6..10);
    h.click(beta);
    h.click(beta);
    h.frame(20.0);
    assert!(!h.scene.highlights.is_empty());
    assert!(!h.rich_text_node().custom_actions().is_empty());
    // A press on the field below the block takes the focus.
    let below = h.layout(TEXT).size().height + 15.0;
    let field = Point::new(20.0, h.origin.y + below);
    h.click(field);
    h.frame(40.0);
    assert!(
        h.scene.highlights.is_empty(),
        "the selection went with the focus"
    );
    let verbs: Vec<String> = h
        .rich_text_node()
        .custom_actions()
        .iter()
        .map(|a| a.description.clone())
        .collect();
    assert_eq!(verbs, ["Select all"], "no Copy without a selection");
}

#[test]
fn a_tap_outside_the_selection_clears_it() {
    let _policy = framework_policy();
    let mut h = Harness::new(block);
    let beta = h.at(TEXT, 6..10);
    h.hold(beta);
    h.pointer(PointerPhase::Up, beta, true);
    let alpha = h.at(TEXT, 0..5);
    h.pointer(PointerPhase::Down, alpha, true);
    h.pointer(PointerPhase::Up, alpha, true);
    h.frame(800.0);
    assert!(h.scene.highlights.is_empty() && h.scene.knobs.is_empty());
    assert!(h.root.selection_toolbar().is_none(), "focus was given up");
}

fn linked(app: &mut App) -> FlexView<App> {
    let _ = app;
    column().child(
        rich_text([span("see "), span("docs").link("docs"), span(" now")])
            .selectable(true)
            .on_link(|app: &mut App, id| app.opened.push(id.to_owned())),
    )
}

#[test]
fn a_link_fires_on_a_tap_and_a_long_press_on_it_selects() {
    let _policy = framework_policy();
    let text = "see docs now";
    let mut h = Harness::new(linked);
    let docs = h.at(text, 4..8);
    h.pointer(PointerPhase::Down, docs, true);
    h.pointer(PointerPhase::Up, docs, true);
    assert_eq!(h.app.opened, ["docs"]);
    h.click(docs);
    assert_eq!(h.app.opened, ["docs", "docs"], "a mouse click too");
    h.frame(100.0);
    h.app.opened.clear();
    h.hold(docs);
    h.pointer(PointerPhase::Up, docs, true);
    assert!(h.app.opened.is_empty(), "a long-press is no tap");
    assert_eq!(h.copied().as_deref(), Some("docs"));
}

#[test]
fn the_default_block_takes_no_gesture_focus_or_verbs() {
    let _policy = framework_policy();
    let mut h = Harness::new(|_: &mut App| column().child(rich_text([span(TEXT)])));
    let beta = h.at(TEXT, 6..10);
    assert!(
        !h.pointer(PointerPhase::Down, beta, true),
        "nothing consumes it"
    );
    h.frame(10.0);
    h.frame(10.0 + LONG_PRESS_MS + 50.0);
    h.send(InputEvent::Housekeeping);
    h.pointer(PointerPhase::Up, beta, true);
    h.click(beta);
    h.click(beta);
    h.frame(700.0);
    assert!(h.scene.highlights.is_empty() && h.scene.knobs.is_empty());
    assert!(h.root.selection_toolbar().is_none());
    assert_eq!(h.copied(), None);
    let node = h.rich_text_node();
    assert!(node.custom_actions().is_empty());
}

#[test]
fn a_wrapped_selection_paints_one_rect_per_line() {
    let _policy = framework_policy();
    let words = "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu";
    let mut h =
        Harness::new(move |_: &mut App| column().child(rich_text([span(words)]).selectable(true)));
    let first = h.at(words, 0..5);
    h.click(first);
    h.click(first);
    h.click(first);
    h.frame(20.0);
    let layout = h.layout(words);
    assert!(layout.line_count() > 1, "the text wraps");
    let expected: Vec<Rect> = layout
        .range_rects(0..words.len())
        .into_iter()
        .map(|r| r + h.origin.to_vec2())
        .collect();
    assert_eq!(h.scene.highlights, expected);
    assert_eq!(h.scene.highlights.len(), layout.line_count());
}

#[test]
fn a_bidi_selection_paints_the_layouts_range_rects() {
    let _policy = framework_policy();
    let arabic = include_bytes!("../../../testing/fonts/NotoSansArabic-Subset.ttf");
    let text = "Hello \u{645}\u{631}\u{62d}\u{628}\u{627} Hello";
    let mut h =
        Harness::new(move |_: &mut App| column().child(rich_text([span(text)]).selectable(true)));
    h.tcx
        .register_fonts(arabic.to_vec())
        .expect("the bundled face registers");
    h.frame(0.0);
    // Drag from inside the first "Hello" into the Arabic word.
    let from = h.caret(text, 3);
    let arabic_mid = h.at(text, 6 + 4..6 + 6);
    h.pointer(PointerPhase::Down, from, false);
    h.pointer(PointerPhase::Move, Point::new(from.x + 40.0, from.y), false);
    h.pointer(PointerPhase::Move, arabic_mid, false);
    h.pointer(PointerPhase::Up, arabic_mid, false);
    h.frame(20.0);
    let copied = h.copied().expect("a selection");
    let start = text.find(&copied).expect("a slice of the text");
    let layout = h.layout(text);
    let expected: Vec<Rect> = layout
        .range_rects(start..start + copied.len())
        .into_iter()
        .map(|r| r + h.origin.to_vec2())
        .collect();
    assert!(
        expected.len() >= 2,
        "it crosses a run boundary: {expected:?}"
    );
    assert_eq!(h.scene.highlights, expected);
}

#[test]
fn a_selection_survives_a_parent_scroll() {
    let _policy = framework_policy();
    let words = "word ".repeat(400);
    let text = words.clone();
    let mut h = Harness::new(move |_: &mut App| {
        column().flex(
            1,
            scroll_view(rich_text([span(text.clone())]).selectable(true)),
        )
    });
    h.window = Size::new(WIDTH, 200.0);
    h.frame(0.0);
    let top = h.origin;
    let word = h.at(&words, 5..9);
    h.hold(word);
    h.pointer(PointerPhase::Up, word, true);
    h.frame(700.0);
    // A touch drag on the text, away from the handles, scrolls the parent.
    let start = Point::new(WIDTH / 2.0, 150.0);
    h.pointer(PointerPhase::Down, start, true);
    for step in 1..=6 {
        let at = start - Vec2::new(0.0, 15.0 * f64::from(step));
        h.pointer(PointerPhase::Move, at, true);
    }
    h.pointer(PointerPhase::Up, start - Vec2::new(0.0, 90.0), true);
    h.frame(800.0);
    assert!(
        h.origin.y < top.y,
        "the parent scrolled: {top:?} -> {:?}",
        h.origin
    );
    assert_eq!(h.copied().as_deref(), Some("word"), "the selection stays");
}

#[test]
fn disposing_the_block_drops_its_selection() {
    let _policy = framework_policy();
    let mut h = Harness::new(block);
    let beta = h.at(TEXT, 6..10);
    h.hold(beta);
    h.frame(700.0);
    // The block leaves the tree with its press, selection and toolbar live.
    h.app.show = false;
    h.frame(710.0);
    h.pointer(PointerPhase::Up, beta, true);
    h.frame(720.0);
    assert!(h.root.selection_toolbar().is_none());
    assert_eq!(h.copied(), None);
    h.app.show = true;
    h.frame(730.0);
    assert!(h.scene.highlights.is_empty(), "a new block starts empty");
}

/// What the probe toolbar saw: the verbs it was built for and where it was
/// painted.
#[derive(Default)]
struct ProbeLog {
    actions: Vec<SelectionToolbarActions>,
    painted: Option<Rect>,
}

type Probe = Arc<Mutex<ProbeLog>>;

/// A stand-in for a design system's toolbar: a fixed-size rect that answers
/// a tap with Copy, the way the baseline bar's items dispatch their verb.
struct ProbeToolbar(Probe);

struct ProbeToolbarWidget(Probe);

impl View<()> for ProbeToolbar {
    type Element = ProbeToolbarWidget;
    fn build(&self, _ctx: &mut BuildCtx<'_>) -> ProbeToolbarWidget {
        ProbeToolbarWidget(Arc::clone(&self.0))
    }
    fn rebuild(
        &self,
        _: &Self,
        element: &mut ProbeToolbarWidget,
        _: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        element.0 = Arc::clone(&self.0);
        ChangeFlags::NONE
    }
}

impl Widget for ProbeToolbarWidget {
    fn layout(&mut self, _ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        bc.constrain(Size::new(80.0, 30.0))
    }
    fn paint(&mut self, ctx: &mut PaintCtx, _scene: &mut dyn PaintScene) {
        let rect = Rect::from_origin_size(ctx.origin(), ctx.size());
        self.0.lock().unwrap_or_else(|e| e.into_inner()).painted = Some(rect);
    }
    fn event(&mut self, ctx: &mut EventCtx, event: &InputEvent) -> EventResult {
        match event {
            InputEvent::Pointer(p) if p.phase == PointerPhase::Up => {
                ctx.dispatch_edit_command(EditCommand::Copy);
                EventResult::Handled
            }
            InputEvent::Pointer(_) => EventResult::Handled,
            _ => EventResult::Ignored,
        }
    }
}

#[test]
fn an_installed_toolbar_builder_floats_copy_and_select_all_and_its_copy_lands() {
    let _policy = framework_policy();
    let probe: Probe = Arc::default();
    let captured = Arc::clone(&probe);
    let builder: SelectionToolbarBuilder =
        Arc::new(move |request: &SelectionToolbarRequest, _window: Size| {
            captured
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .actions
                .push(request.actions);
            frust_core::any(ProbeToolbar(Arc::clone(&captured)))
        });
    set_selection_toolbar_builder(builder);
    let mut h = Harness::new(block);
    let beta = h.at(TEXT, 6..10);
    h.hold(beta);
    h.pointer(PointerPhase::Up, beta, true);
    // One frame mounts the pod, the next paints it in place.
    h.frame(700.0);
    h.frame(710.0);
    let (actions, painted) = {
        let log = probe.lock().unwrap_or_else(|e| e.into_inner());
        (log.actions.clone(), log.painted)
    };
    assert_eq!(
        actions,
        [SelectionToolbarActions {
            copy: true,
            cut: false,
            paste: false,
            select_all: true,
        }]
    );
    let bar = painted.expect("the floated pod painted");
    assert!(
        bar.y1 <= h.origin.y + 1.0 || bar.y0 >= h.origin.y,
        "{bar:?}"
    );
    let _ = h.root.take_clipboard_write();
    h.pointer(PointerPhase::Down, bar.center(), true);
    h.pointer(PointerPhase::Up, bar.center(), true);
    assert_eq!(h.root.take_clipboard_write().as_deref(), Some("beta"));
    h.frame(720.0);
    assert!(
        h.root.selection_toolbar().is_some_and(|r| !r.present_menu),
        "a verb spent puts the bar away, the selection stays"
    );
    // Back to the baseline bar for the tests after this one.
    set_selection_toolbar_builder(Arc::new(|request: &SelectionToolbarRequest, _: Size| {
        frust_widgets::selection_toolbar(request)
    }));
}
