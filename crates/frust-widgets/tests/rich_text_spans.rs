//! `rich_text` span backgrounds and links through a real `RenderRoot`: the
//! fragments a wrapped span paints, link taps (hit, wrapped second fragment,
//! miss, slop, cancel), the callback's id, the link semantics node and its
//! Click, the hover cursor, and the equal-view rebuild short-circuit.

use frust_core::accesskit::{Action, Role};
use frust_core::{
    BuildCtx, ChangeFlags, CursorIcon, FrameTime, InputEvent, PaintScene, PointerButton,
    PointerEvent, PointerPhase, RenderRoot, View,
};
use frust_scene::GlyphRun;
use frust_text::TextContext;
use frust_widgets::{RichTextView, rich_text, span};
use kurbo::{Point, Rect, Size};
use peniko::Color;

#[derive(Default)]
struct App {
    opened: Vec<String>,
}

/// Records the rounded fills: one per background fragment.
#[derive(Default)]
struct Fills(Vec<Rect>);

impl PaintScene for Fills {
    fn fill_rect(&mut self, _o: Point, _s: Size, _c: Color) {}
    fn fill_rounded_rect(&mut self, o: Point, s: Size, _radius: f64, _c: Color) {
        self.0.push(Rect::from_origin_size(o, s));
    }
    fn draw_text(&mut self, _o: Point, _t: &str) {}
    fn draw_glyph_run(&mut self, _run: GlyphRun) {}
}

fn grey() -> Color {
    Color::from_rgb8(220, 220, 220)
}

/// "see " + a link wrapping across lines at 120 px + " after", with a
/// background on the link so its fragments are visible to the recorder.
fn paragraph() -> RichTextView {
    rich_text([
        span("see "),
        span("alpha beta gamma delta epsilon")
            .background(grey())
            .link("docs"),
        span(" after"),
    ])
    .on_link(|app: &mut App, id| app.opened.push(id.to_owned()))
}

struct Harness {
    root: RenderRoot<App, RichTextView>,
    app: App,
    _tcx: TextContext,
}

impl Harness {
    fn new(width: f64) -> Self {
        let mut root: RenderRoot<App, RichTextView> = RenderRoot::new();
        let mut app = App::default();
        root.rebuild(&mut |_: &mut App| paragraph(), &mut app);
        let mut tcx = TextContext::new();
        root.layout_with_text(Size::new(width, 400.0), &mut tcx as &mut dyn std::any::Any);
        Self {
            root,
            app,
            _tcx: tcx,
        }
    }

    /// The painted background fragments, in line order.
    fn fragments(&mut self) -> Vec<Rect> {
        let mut fills = Fills::default();
        self.root.paint(&mut fills, FrameTime::default());
        fills.0
    }

    fn pointer(&mut self, phase: PointerPhase, at: Point) -> bool {
        let event = InputEvent::Pointer(PointerEvent {
            phase,
            position: at,
            button: PointerButton::Primary,
        });
        self.root.event(&mut self.app, &event).handled
    }

    fn tap(&mut self, at: Point) -> (bool, bool) {
        let down = self.pointer(PointerPhase::Down, at);
        let up = self.pointer(PointerPhase::Up, at);
        (down, up)
    }
}

#[test]
fn a_wrapped_background_paints_one_fragment_per_line() {
    let mut h = Harness::new(120.0);
    let fragments = h.fragments();
    assert!(fragments.len() >= 2, "{fragments:?}");
    assert!(
        fragments.windows(2).all(|w| w[0].y1 <= w[1].y0 + 0.01),
        "one fragment per line, top to bottom: {fragments:?}"
    );
    assert!(
        fragments
            .iter()
            .all(|r| r.width() > 0.0 && r.height() > 0.0)
    );
}

#[test]
fn tapping_the_first_fragment_reports_the_link_id() {
    let mut h = Harness::new(120.0);
    let first = h.fragments()[0];
    assert_eq!(h.tap(first.center()), (true, true));
    assert_eq!(h.app.opened, ["docs"]);
}

#[test]
fn tapping_the_wrapped_second_fragment_reports_the_link_id() {
    let mut h = Harness::new(120.0);
    let second = h.fragments()[1];
    h.tap(second.center());
    assert_eq!(h.app.opened, ["docs"]);
}

#[test]
fn a_tap_off_the_link_does_nothing_and_is_not_consumed() {
    let mut h = Harness::new(120.0);
    let first = h.fragments()[0];
    // Inside "see ", left of the link's padded background.
    let plain = Point::new(2.0, first.center().y);
    assert_eq!(h.tap(plain), (false, false));
    // Below the paragraph.
    assert_eq!(h.tap(Point::new(5.0, 390.0)), (false, false));
    assert!(h.app.opened.is_empty());
}

#[test]
fn moving_past_the_slop_cancels_the_tap() {
    let mut h = Harness::new(120.0);
    let at = h.fragments()[0].center();
    assert!(h.pointer(PointerPhase::Down, at));
    h.pointer(PointerPhase::Move, Point::new(at.x, at.y + 30.0));
    h.pointer(PointerPhase::Up, at);
    assert!(h.app.opened.is_empty());

    // Within the slop it still fires.
    h.pointer(PointerPhase::Down, at);
    h.pointer(PointerPhase::Move, Point::new(at.x + 3.0, at.y));
    h.pointer(PointerPhase::Up, Point::new(at.x + 3.0, at.y));
    assert_eq!(h.app.opened, ["docs"]);
}

#[test]
fn a_cancel_from_a_scroll_takeover_drops_the_press() {
    let mut h = Harness::new(120.0);
    let at = h.fragments()[0].center();
    h.pointer(PointerPhase::Down, at);
    h.pointer(PointerPhase::Cancel, at);
    h.pointer(PointerPhase::Up, at);
    assert!(h.app.opened.is_empty());
}

#[test]
fn ending_on_another_part_of_the_text_does_not_fire() {
    let mut h = Harness::new(120.0);
    let at = h.fragments()[0].center();
    h.pointer(PointerPhase::Down, at);
    h.pointer(PointerPhase::Up, Point::new(2.0, at.y));
    assert!(h.app.opened.is_empty());
}

#[test]
fn semantics_expose_the_link_with_a_click_that_fires_it() {
    let mut h = Harness::new(120.0);
    let update = h.root.semantics();
    let (id, link) = update
        .nodes
        .iter()
        .find(|(_, node)| node.role() == Role::Link)
        .cloned()
        .expect("a link node");
    assert_eq!(link.label(), Some("alpha beta gamma delta epsilon"));
    assert!(link.supports_action(Action::Click));
    let paragraph = update
        .nodes
        .iter()
        .find(|(_, node)| node.children().contains(&id))
        .map(|(_, node)| node.clone())
        .expect("the paragraph holds the link");
    assert_eq!(paragraph.role(), Role::Label);
    assert_eq!(
        paragraph.value(),
        Some("see alpha beta gamma delta epsilon after")
    );

    h.root
        .perform_accessibility_action(&mut h.app, id, Action::Click);
    assert_eq!(h.app.opened, ["docs"]);
}

#[test]
fn hovering_a_link_asks_for_the_pointer_cursor() {
    let mut h = Harness::new(120.0);
    let at = h.fragments()[0].center();
    h.pointer(PointerPhase::Move, at);
    assert_eq!(h.root.cursor(), CursorIcon::Pointer);
    h.pointer(PointerPhase::Move, Point::new(2.0, at.y));
    assert_eq!(h.root.cursor(), CursorIcon::Default);
}

#[test]
fn equal_views_skip_rebuild_and_still_run_the_latest_callback() {
    let first = paragraph();
    let second = paragraph();
    assert_eq!(first, second, "a fresh closure keeps the views equal");
    let mut next_id = 0u64;
    let mut ctx = BuildCtx::new(&mut next_id);
    let mut widget = View::<App>::build(&first, &mut ctx);
    let flags = View::<App>::rebuild(&second, &first, &mut widget, &mut ctx);
    assert_eq!(flags, ChangeFlags::NONE);

    // Through the root: the second build's closure is the one that runs.
    let mut root: RenderRoot<App, RichTextView> = RenderRoot::new();
    let mut app = App::default();
    root.rebuild(&mut |_: &mut App| paragraph(), &mut app);
    root.rebuild(
        &mut |_: &mut App| {
            paragraph().on_link(|app: &mut App, id| app.opened.push(format!("new:{id}")))
        },
        &mut app,
    );
    let mut tcx = TextContext::new();
    root.layout_with_text(Size::new(120.0, 400.0), &mut tcx as &mut dyn std::any::Any);
    let mut fills = Fills::default();
    root.paint(&mut fills, FrameTime::default());
    let at = fills.0[0].center();
    for phase in [PointerPhase::Down, PointerPhase::Up] {
        let event = InputEvent::Pointer(PointerEvent {
            phase,
            position: at,
            button: PointerButton::Primary,
        });
        root.event(&mut app, &event);
    }
    assert_eq!(app.opened, ["new:docs"]);
}
