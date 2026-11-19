//! The meter widgets headless: a real `RenderRoot` painting into a recording scene.
//!
//! A `Samples` write requests a process-wide repaint, so every test holds `STATICS`.

use std::sync::{Mutex, MutexGuard, PoisonError};

use frust::authoring::{
    BuildCtx, ChangeFlags, CornerRadii, EventCtx, EventResult, InputEvent, Node, PaintScene, Point,
    PointerButton, PointerEvent, PointerPhase, Role, Size, View, Widget,
};
use frust::{Color, FrameTime, Theme};
use frust_charts::{MeterView, Samples, Segment, Tone, meter, meter_segments, meter_strip};
use frust_core::RenderRoot;

static STATICS: Mutex<()> = Mutex::new(());

fn hold() -> MutexGuard<'static, ()> {
    STATICS.lock().unwrap_or_else(PoisonError::into_inner)
}

const WINDOW: Size = Size::new(200.0, 100.0);
const RED: Color = Color::from_rgb8(0xff, 0, 0);
const BLUE: Color = Color::from_rgb8(0, 0, 0xff);
const GREEN: Color = Color::from_rgb8(0, 0xff, 0);
const GREY: Color = Color::from_rgb8(0x80, 0x80, 0x80);

#[derive(Clone, Debug, PartialEq)]
enum Op {
    Rect(Point, Size, Color),
    Rounded(Point, Size, f64, Color),
    Radii(Point, Size, CornerRadii, Color),
    Clip(Point, Size, f64),
    Pop,
}

#[derive(Default)]
struct Rec {
    ops: Vec<Op>,
}

impl PaintScene for Rec {
    fn fill_rect(&mut self, origin: Point, size: Size, color: Color) {
        self.ops.push(Op::Rect(origin, size, color));
    }
    fn fill_rounded_rect(&mut self, origin: Point, size: Size, radius: f64, color: Color) {
        self.ops.push(Op::Rounded(origin, size, radius, color));
    }
    fn fill_rounded_rect_radii(&mut self, o: Point, s: Size, radii: CornerRadii, color: Color) {
        self.ops.push(Op::Radii(o, s, radii, color));
    }
    fn push_clip(&mut self, origin: Point, size: Size) {
        self.ops.push(Op::Clip(origin, size, 0.0));
    }
    fn push_clip_rounded(&mut self, origin: Point, size: Size, radius: f64) {
        self.ops.push(Op::Clip(origin, size, radius));
    }
    fn pop_clip(&mut self) {
        self.ops.push(Op::Pop);
    }
    fn draw_text(&mut self, _origin: Point, _text: &str) {}
}

struct Harness<V: View<()>> {
    root: RenderRoot<(), V>,
    window: Size,
}

impl<V: View<()>> Harness<V> {
    fn new(view: V, window: Size) -> Self {
        Self::themed(view, window, None)
    }

    fn themed(view: V, window: Size, theme: Option<Theme>) -> Self {
        let mut root: RenderRoot<(), V> = RenderRoot::new();
        if let Some(theme) = theme {
            root.set_theme(Box::new(theme));
        }
        let mut view = Some(view);
        root.rebuild(&mut |_: &mut ()| view.take().expect("built once"), &mut ());
        root.layout(window);
        Self { root, window }
    }

    fn paint(&mut self) -> Vec<Op> {
        self.root.layout(self.window);
        let mut rec = Rec::default();
        self.root
            .paint(&mut rec, FrameTime::from_nanos(1_000_000_000));
        rec.ops
    }

    /// The nodes this tree adds under the window node.
    fn nodes(&self) -> Vec<Node> {
        let update = self.root.semantics();
        update
            .nodes
            .into_iter()
            .filter(|(id, _)| *id != update.root)
            .map(|(_, node)| node)
            .collect()
    }
}

fn paint<V: View<()>>(view: V) -> Vec<Op> {
    Harness::new(view, WINDOW).paint()
}

/// The fill a single meter painted: its rounded rect inside the rounded clip.
fn single_fill(ops: &[Op]) -> Option<(Point, Size, f64, Color)> {
    match ops {
        [Op::Rounded(..)] => None,
        [
            Op::Rounded(_, size, radius, _),
            Op::Clip(_, clip, clip_r),
            Op::Rounded(o, s, r, c),
            Op::Pop,
        ] => {
            assert_eq!((clip, clip_r), (size, radius), "clipped to the track shape");
            Some((*o, *s, *r, *c))
        }
        other => panic!("unexpected meter ops: {other:?}"),
    }
}

fn flags(old: MeterView, new: MeterView) -> ChangeFlags {
    let mut counter = 0u64;
    let mut ctx = BuildCtx::new(&mut counter);
    let mut widget = <MeterView as View<()>>::build(&old, &mut ctx);
    <MeterView as View<()>>::rebuild(&new, &old, &mut widget, &mut ctx)
}

#[test]
fn fill_follows_the_value_and_clamps_at_every_thickness() {
    let _g = hold();
    for thickness in [4.0f32, 6.0, 8.0] {
        let t = f64::from(thickness);
        for (percent, width) in [(0.0, None), (12.0, Some(24.0)), (50.0, Some(100.0))]
            .into_iter()
            .chain([(100.0, Some(200.0)), (150.0, Some(200.0))])
        {
            let ops = paint(meter(percent).thickness(thickness));
            let Op::Rounded(origin, track, radius, _) = ops[0] else {
                panic!("the track paints first: {ops:?}");
            };
            assert_eq!((origin, track), (Point::ZERO, Size::new(200.0, t)));
            assert_eq!(radius, t / 2.0, "fully rounded by default");
            let fill = single_fill(&ops).map(|(o, s, _, _)| {
                assert_eq!((o, s.height), (Point::ZERO, t));
                s.width
            });
            assert_eq!(fill, width, "{percent}% at thickness {thickness}");
        }
    }
}

#[test]
fn a_tiny_positive_value_still_draws_one_pixel() {
    let _g = hold();
    let fill = single_fill(&paint(meter(0.1))).expect("a fill");
    assert_eq!(fill.1.width, 1.0);
    assert!(fill.2 <= 0.5, "the fill's radius fits its length");
}

#[test]
fn range_radius_and_length_shape_the_meter() {
    let _g = hold();
    let ops = paint(meter(15.0).range(10.0, 20.0).radius(1.0).length(80.0));
    let Op::Rounded(_, track, radius, _) = ops[0] else {
        panic!("track first");
    };
    assert_eq!((track, radius), (Size::new(80.0, 6.0), 1.0));
    assert_eq!(single_fill(&ops).unwrap().1.width, 40.0);
}

#[test]
fn tone_picks_the_fill_colour_at_the_thresholds() {
    let _g = hold();
    let tone = Tone::load(GREEN, BLUE, RED);
    for (value, color) in [(59.0, GREEN), (60.0, BLUE), (84.9, BLUE), (85.0, RED)] {
        let fill = single_fill(&paint(meter(value).tone(tone.clone()).color(GREY)));
        assert_eq!(fill.unwrap().3, color, "value {value}");
    }
}

#[test]
fn colours_come_from_the_caller_then_the_theme_then_the_fallbacks() {
    let _g = hold();
    let ops = paint(meter(50.0));
    assert_eq!(
        ops[0],
        Op::Rounded(
            Point::ZERO,
            Size::new(200.0, 6.0),
            3.0,
            MeterView::FALLBACK_TRACK
        )
    );
    assert_eq!(single_fill(&ops).unwrap().3, Tone::FALLBACK_CALM);

    let theme = Theme::neutral();
    let scheme = *theme.scheme();
    let ops = Harness::themed(meter(50.0), WINDOW, Some(theme.clone())).paint();
    let Op::Rounded(.., track) = ops[0] else {
        panic!("track first")
    };
    assert_eq!(track, scheme.surface_container_highest);
    assert_eq!(single_fill(&ops).unwrap().3, scheme.primary);

    let ops = Harness::themed(meter(50.0).color(RED).track(GREY), WINDOW, Some(theme)).paint();
    let Op::Rounded(.., track) = ops[0] else {
        panic!("track first")
    };
    assert_eq!(track, GREY);
    assert_eq!(single_fill(&ops).unwrap().3, RED);
}

#[test]
fn a_gap_draws_the_track_only() {
    let _g = hold();
    assert_eq!(single_fill(&paint(meter(None))), None);
    assert_eq!(single_fill(&paint(meter(f32::NAN))), None);
    let samples = Samples::new(2);
    assert_eq!(single_fill(&paint(meter(samples.slot(1)))), None);
}

#[test]
fn vertical_fills_from_the_bottom() {
    let _g = hold();
    let ops = Harness::new(meter(25.0).vertical(), Size::new(100.0, 200.0)).paint();
    let Op::Rounded(_, track, radius, _) = ops[0] else {
        panic!("track first");
    };
    assert_eq!((track, radius), (Size::new(6.0, 200.0), 3.0));
    let (origin, size, ..) = single_fill(&ops).unwrap();
    assert_eq!(
        (origin, size),
        (Point::new(0.0, 150.0), Size::new(6.0, 50.0))
    );
}

fn segment_rects(ops: &[Op]) -> Vec<(Point, Size, Color)> {
    let [
        Op::Rounded(_, track, radius, _),
        Op::Clip(_, clip, clip_r),
        rest @ ..,
    ] = ops
    else {
        panic!("track then rounded clip: {ops:?}");
    };
    assert_eq!(
        (clip, clip_r),
        (track, radius),
        "segments draw inside the track shape"
    );
    let [rects @ .., Op::Pop] = rest else {
        panic!("the clip is popped last: {ops:?}");
    };
    rects
        .iter()
        .map(|op| match op {
            Op::Rect(o, s, c) => (*o, *s, *c),
            other => panic!("segments are plain rects: {other:?}"),
        })
        .collect()
}

#[test]
fn segments_abut_in_proportion_inside_a_rounded_clip() {
    let _g = hold();
    let ops = paint(meter_segments([
        Segment::new(30.0, RED),
        Segment::new(20.0, BLUE),
    ]));
    let rects = segment_rects(&ops);
    assert_eq!(
        rects,
        [
            (Point::ZERO, Size::new(60.0, 6.0), RED),
            (Point::new(60.0, 0.0), Size::new(40.0, 6.0), BLUE),
        ]
    );
    assert_eq!(
        rects[0].0.x + rects[0].1.width,
        rects[1].0.x,
        "no gap between segments"
    );
}

#[test]
fn segments_clamp_their_total_to_the_track() {
    let _g = hold();
    let ops = paint(meter_segments([
        Segment::new(60.0, RED),
        Segment::new(None, GREY),
        Segment::new(70.0, BLUE),
        Segment::new(10.0, GREEN),
    ]));
    assert_eq!(
        segment_rects(&ops),
        [
            (Point::ZERO, Size::new(120.0, 6.0), RED),
            (Point::new(120.0, 0.0), Size::new(80.0, 6.0), BLUE),
        ]
    );
    assert_eq!(
        paint(meter_segments([Segment::new(None, RED)])).len(),
        1,
        "track only"
    );
}

#[test]
fn a_live_slot_repaints_with_no_rebuild() {
    let _g = hold();
    let samples = Samples::new(4);
    samples.set(2, 10.0);
    let mut harness = Harness::new(meter(samples.slot(2)), WINDOW);
    assert_eq!(single_fill(&harness.paint()).unwrap().1.width, 20.0);
    assert!(!harness.root.repaint_pending());

    samples.set(2, 75.0);
    assert!(harness.root.repaint_pending(), "a write asks for a repaint");
    assert_eq!(single_fill(&harness.paint()).unwrap().1.width, 150.0);
    assert!(!harness.root.repaint_pending());

    samples.set_gap(2);
    assert_eq!(single_fill(&harness.paint()), None);
}

#[test]
fn rebuild_flags_match_what_changed() {
    let _g = hold();
    assert_eq!(flags(meter(10.0), meter(10.0)), ChangeFlags::NONE);
    assert_eq!(flags(meter(10.0), meter(20.0)), ChangeFlags::PAINT);
    assert_eq!(
        flags(meter(10.0), meter(10.0).color(RED)),
        ChangeFlags::PAINT
    );
    assert_eq!(
        flags(meter(10.0), meter(10.0).semantics("CPU")),
        ChangeFlags::PAINT
    );
    let layout = ChangeFlags::LAYOUT | ChangeFlags::PAINT;
    assert_eq!(flags(meter(10.0), meter(10.0).thickness(8.0)), layout);
    assert_eq!(flags(meter(10.0), meter(10.0).length(50.0)), layout);
    assert_eq!(flags(meter(10.0), meter(10.0).vertical()), layout);

    let samples = Samples::new(2);
    let live = || meter(samples.slot(0)).tone(Tone::load(GREEN, BLUE, RED));
    assert_eq!(
        flags(live(), live()),
        ChangeFlags::NONE,
        "same slot, same options"
    );
    assert_eq!(flags(live(), meter(samples.slot(1))), ChangeFlags::PAINT);
    let other = Samples::new(2);
    assert_eq!(
        flags(meter(samples.slot(0)), meter(other.slot(0))),
        ChangeFlags::PAINT
    );

    let segs = |b: f32| meter_segments([Segment::new(1.0, RED), Segment::new(b, BLUE)]);
    assert_eq!(flags(segs(2.0), segs(2.0)), ChangeFlags::NONE);
    assert_eq!(flags(segs(2.0), segs(3.0)), ChangeFlags::PAINT);
}

#[test]
fn a_fixed_value_change_through_a_root_rebuild_is_paint_only() {
    let _g = hold();
    let mut root: RenderRoot<f32, MeterView> = RenderRoot::new();
    let mut logic = |value: &mut f32| meter(*value);
    let mut value = 10.0;
    root.rebuild(&mut logic, &mut value);
    root.layout(WINDOW);
    root.take_change_flags();
    assert_eq!(root.rebuild(&mut logic, &mut value), ChangeFlags::NONE);
    value = 40.0;
    assert_eq!(root.rebuild(&mut logic, &mut value), ChangeFlags::PAINT);
}

type Bar = (Point, Size, CornerRadii);
type Fill = (Point, Size, Color);

/// The strip's tracks and fills, in paint order.
fn strip_ops(ops: &[Op]) -> (Vec<Bar>, Vec<Fill>) {
    let (mut tracks, mut fills) = (Vec::new(), Vec::new());
    for op in ops {
        match op {
            Op::Radii(o, s, r, c) if *c == GREY => tracks.push((*o, *s, *r)),
            Op::Radii(o, s, _, c) => fills.push((*o, *s, *c)),
            other => panic!("a strip draws per-bar rounded rects only: {other:?}"),
        }
    }
    (tracks, fills)
}

fn check_strip(count: usize, width: f64) -> Vec<Bar> {
    let samples = Samples::new(count);
    samples.set_all((0..count).map(|i| Some((i * 100 / count) as f32)));
    let ops = Harness::new(meter_strip(samples).track(GREY), Size::new(width, 100.0)).paint();
    let (tracks, fills) = strip_ops(&ops);
    assert_eq!(tracks.len(), count, "one bar per value");
    assert_eq!(fills.len(), count - 1, "the zero bar has no fill");
    for (o, s, _) in &tracks {
        assert_eq!((o.y, s.height), (0.0, 40.0));
        assert!(o.x >= 0.0 && o.x + s.width <= width, "inside the strip");
    }
    tracks
}

#[test]
fn strip_bars_are_whole_pixels_with_equal_gaps() {
    let _g = hold();
    for (count, bar, left) in [(8, 23.0, 2.0), (32, 4.0, 10.0)] {
        let tracks = check_strip(count, 200.0);
        assert_eq!(tracks[0].0.x, left, "the leftover is the left margin");
        for (o, s, r) in &tracks {
            assert_eq!(s.width, bar);
            assert_eq!(o.x.fract(), 0.0);
            assert_eq!(*r, CornerRadii::new(2.0, 2.0, 0.0, 0.0), "square bottoms");
        }
        for pair in tracks.windows(2) {
            assert_eq!(
                pair[1].0.x - (pair[0].0.x + pair[0].1.width),
                2.0,
                "equal gaps"
            );
        }
    }
}

#[test]
fn strip_too_narrow_for_whole_pixels_draws_fractional_bars() {
    let _g = hold();
    let tracks = check_strip(32, 20.0);
    assert!(
        tracks
            .iter()
            .all(|(_, s, _)| s.width > 0.0 && s.width < 1.0)
    );
    check_strip(8, 0.5);
    let empty = Harness::new(meter_strip(Samples::new(0)), WINDOW).paint();
    assert!(empty.is_empty());
}

#[test]
fn strip_fills_bottom_up_by_tone_and_repaints_live() {
    let _g = hold();
    let samples = Samples::new(2);
    samples.set_all([Some(50.0), Some(90.0)]);
    let tone = Tone::load(GREEN, BLUE, RED);
    let view = meter_strip(samples.clone()).tone(tone).track(GREY).gap(0.0);
    let mut harness = Harness::new(view, WINDOW);
    let (_, fills) = strip_ops(&harness.paint());
    assert_eq!(
        fills,
        [
            (Point::new(0.0, 20.0), Size::new(100.0, 20.0), GREEN),
            (Point::new(100.0, 4.0), Size::new(100.0, 36.0), RED),
        ]
    );
    samples.set_gap(1);
    assert!(harness.root.repaint_pending());
    let (tracks, fills) = strip_ops(&harness.paint());
    assert_eq!(
        (tracks.len(), fills.len()),
        (2, 1),
        "a gap bar is track only"
    );
}

#[test]
fn meters_ignore_input() {
    let _g = hold();
    let down = InputEvent::Pointer(PointerEvent {
        phase: PointerPhase::Down,
        position: Point::new(10.0, 3.0),
        button: PointerButton::Primary,
    });
    let mut counter = 0u64;
    let view = meter(50.0);
    let mut widget = <MeterView as View<()>>::build(&view, &mut BuildCtx::new(&mut counter));
    let mut state = ();
    let mut ctx = EventCtx::new(&mut state, Point::ZERO, Size::new(200.0, 6.0));
    assert!(matches!(
        widget.event(&mut ctx, &down),
        EventResult::Ignored
    ));

    let mut harness = Harness::new(meter(50.0), WINDOW);
    harness.paint();
    assert!(!harness.root.event(&mut (), &down).handled);
    assert!(!harness.root.is_pointer_captured());

    let mut strip = Harness::new(meter_strip(Samples::new(4)), WINDOW);
    strip.paint();
    assert!(!strip.root.event(&mut (), &down).handled);
    assert!(!strip.root.is_pointer_captured());
}

#[test]
fn semantics_report_value_range_and_label() {
    let _g = hold();
    let harness = Harness::new(meter(150.0).range(0.0, 120.0).semantics("CPU"), WINDOW);
    let [node] = harness.nodes().try_into().expect("one node");
    assert_eq!(node.role(), Role::ProgressIndicator);
    assert_eq!(node.numeric_value(), Some(120.0), "clamped into the range");
    assert_eq!(node.min_numeric_value(), Some(0.0));
    assert_eq!(node.max_numeric_value(), Some(120.0));
    assert_eq!(node.label(), Some("CPU"));

    let segs = meter_segments([Segment::new(30.0, RED), Segment::new(12.5, BLUE)]);
    let [node] = Harness::new(segs, WINDOW)
        .nodes()
        .try_into()
        .expect("one node");
    assert_eq!(node.role(), Role::ProgressIndicator);
    assert_eq!(node.numeric_value(), Some(42.5), "the summed value");
    assert_eq!(node.label(), None);

    let samples = Samples::new(16);
    assert!(
        Harness::new(meter_strip(samples.clone()), WINDOW)
            .nodes()
            .is_empty()
    );
    let labelled = Harness::new(meter_strip(samples).semantics("Cores"), WINDOW);
    let [node] = labelled
        .nodes()
        .try_into()
        .expect("one node, not one per bar");
    assert_eq!(node.role(), Role::Image);
    assert_eq!(node.label(), Some("Cores"));
}
