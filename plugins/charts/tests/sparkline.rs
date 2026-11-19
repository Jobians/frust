//! The sparkline headless: a real `RenderRoot` painting into a recording scene.
//!
//! A `Series` write requests a process-wide repaint, so every test holds `STATICS`.

use std::sync::{Mutex, MutexGuard, PoisonError};

use frust::authoring::{
    BezPath, Brush, BuildCtx, ChangeFlags, EventCtx, EventResult, InputEvent, Node, PaintScene,
    Point, PointerButton, PointerEvent, PointerPhase, Role, Size, View, Widget,
};
use frust::{Color, FrameTime};
use frust_charts::{Series, SparkStyle, SparklineView, Tone, YDomain, sparkline};
use frust_core::RenderRoot;

static STATICS: Mutex<()> = Mutex::new(());

fn hold() -> MutexGuard<'static, ()> {
    STATICS.lock().unwrap_or_else(PoisonError::into_inner)
}

const RED: Color = Color::from_rgb8(0xff, 0, 0);
const BLUE: Color = Color::from_rgb8(0, 0, 0xff);
const GREEN: Color = Color::from_rgb8(0, 0xff, 0);

#[derive(Clone, Debug, PartialEq)]
enum Op {
    Rect(Point, Size, Color),
    Dot(Point, Size, f64, Color),
    Stroke(Vec<Point>, f64, Color),
    Clip(Point, Size),
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
        self.ops.push(Op::Dot(origin, size, radius, color));
    }
    fn stroke_path(&mut self, origin: Point, path: &BezPath, width: f64, brush: &Brush) {
        let points = path
            .elements()
            .iter()
            .filter_map(|el| match el {
                frust::kurbo::PathEl::MoveTo(p) | frust::kurbo::PathEl::LineTo(p) => {
                    Some(*p + origin.to_vec2())
                }
                _ => None,
            })
            .collect();
        let Brush::Solid(color) = brush else {
            panic!("solid brush expected");
        };
        self.ops.push(Op::Stroke(points, width, *color));
    }
    fn push_clip(&mut self, origin: Point, size: Size) {
        self.ops.push(Op::Clip(origin, size));
    }
    fn pop_clip(&mut self) {
        self.ops.push(Op::Pop);
    }
    fn draw_text(&mut self, _origin: Point, _text: &str) {}
}

struct Harness {
    root: RenderRoot<(), SparklineView>,
    window: Size,
}

impl Harness {
    fn new(view: SparklineView, window: Size) -> Self {
        let mut root: RenderRoot<(), SparklineView> = RenderRoot::new();
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

/// The ops between the clip push and pop (everything, when nothing was drawn: empty).
fn body(ops: &[Op]) -> &[Op] {
    match ops {
        [] => ops,
        [Op::Clip(..), inner @ .., Op::Pop] => inner,
        other => panic!("unexpected framing: {other:?}"),
    }
}

fn rects(ops: &[Op]) -> Vec<(Point, Size, Color)> {
    body(ops)
        .iter()
        .map(|op| match op {
            Op::Rect(o, s, c) => (*o, *s, *c),
            other => panic!("not a rect: {other:?}"),
        })
        .collect()
}

fn line(width: f32) -> SparkStyle {
    SparkStyle::Line { width }
}

fn flags(old: SparklineView, new: SparklineView) -> ChangeFlags {
    let mut counter = 0u64;
    let mut ctx = BuildCtx::new(&mut counter);
    let mut widget = <SparklineView as View<()>>::build(&old, &mut ctx);
    <SparklineView as View<()>>::rebuild(&new, &old, &mut widget, &mut ctx)
}

#[test]
fn blocks_are_right_aligned_whole_pixel_columns_one_per_present_slot() {
    let _g = hold();
    let series = Series::new(100);
    series.extend([Some(50.0), None, Some(100.0), Some(0.0)]);
    let ops = Harness::new(sparkline(series).color(RED), Size::new(160.0, 24.0)).paint();
    let columns = rects(&ops);
    assert_eq!(columns.len(), 3);
    // Pitch 3, right edge 160: the newest (0) spans 158..160, the 100 spans 155..157, the
    // gap slot leaves 152..154 empty, and the oldest (50) spans 149..151.
    let expected = [
        (Point::new(149.0, 12.0), Size::new(2.0, 12.0)),
        (Point::new(155.0, 0.0), Size::new(2.0, 24.0)),
        (Point::new(158.0, 23.0), Size::new(2.0, 1.0)),
    ];
    for ((origin, size, color), (eo, es)) in columns.iter().zip(expected) {
        assert_eq!((*origin, *size), (eo, es));
        assert_eq!(*color, RED);
        assert_eq!((origin.x.fract(), size.width.fract()), (0.0, 0.0));
    }
}

#[test]
fn blocks_that_do_not_fit_drop_the_oldest() {
    let _g = hold();
    let series = Series::new(100);
    series.extend((0..100).map(|i| Some(i as f32)));
    let ops = Harness::new(sparkline(series), Size::new(30.0, 24.0)).paint();
    let columns = rects(&ops);
    assert_eq!(columns.len(), 10, "30 px at a pitch of 3");
    assert_eq!(columns.last().unwrap().0.x + 2.0, 30.0);
    assert!(columns.iter().all(|(o, _, _)| o.x >= 0.0));
}

#[test]
fn blocks_take_a_colour_from_their_own_value() {
    let _g = hold();
    let series = Series::new(10);
    series.extend([Some(59.0), Some(60.0), Some(85.0)]);
    let tone = Tone::load(GREEN, BLUE, RED);
    let ops = Harness::new(sparkline(series).tone(tone), Size::new(160.0, 24.0)).paint();
    let colors: Vec<Color> = rects(&ops).iter().map(|r| r.2).collect();
    assert_eq!(colors, [GREEN, BLUE, RED]);
}

#[test]
fn a_line_strokes_one_path_per_run_and_a_lone_sample_is_a_dot() {
    let _g = hold();
    let series = Series::new(10);
    series.extend([
        Some(10.0),
        Some(20.0),
        Some(30.0),
        None,
        Some(40.0),
        Some(50.0),
        None,
        Some(60.0),
    ]);
    let ops = Harness::new(
        sparkline(series).style(line(1.0)).color(RED),
        Size::new(100.0, 24.0),
    )
    .paint();
    let inner = body(&ops);
    let strokes: Vec<&Vec<Point>> = inner
        .iter()
        .filter_map(|op| match op {
            Op::Stroke(points, w, c) if *w == 1.0 && *c == RED => Some(points),
            _ => None,
        })
        .collect();
    assert_eq!(strokes.len(), 2);
    assert_eq!((strokes[0].len(), strokes[1].len()), (3, 2));
    let gap_x = strokes[0].last().unwrap().x..strokes[1][0].x;
    assert!(gap_x.start < gap_x.end, "no path spans the gap");
    let dots: Vec<_> = inner
        .iter()
        .filter(|op| matches!(op, Op::Dot(..)))
        .collect();
    assert_eq!(dots.len(), 1);
}

#[test]
fn a_line_takes_the_tone_of_the_latest_value() {
    let _g = hold();
    let series = Series::new(10);
    series.extend([Some(10.0), Some(90.0)]);
    let tone = Tone::load(GREEN, BLUE, RED);
    let ops = Harness::new(
        sparkline(series).style(line(1.0)).tone(tone),
        Size::new(100.0, 24.0),
    )
    .paint();
    assert!(matches!(&body(&ops)[0], Op::Stroke(_, _, c) if *c == RED));
}

#[test]
fn a_push_followed_by_paint_changes_the_output_with_no_rebuild() {
    let _g = hold();
    let series = Series::new(20);
    series.push(50.0);
    let mut harness = Harness::new(sparkline(series.clone()), Size::new(60.0, 24.0));
    let first = harness.paint();
    assert_eq!(rects(&first).len(), 1);
    series.push(80.0);
    assert!(harness.root.repaint_pending());
    let second = harness.paint();
    assert_eq!(rects(&second).len(), 2);
    assert_ne!(first, second);
    assert_eq!(harness.paint(), second, "an unchanged repaint is identical");
}

#[test]
fn identical_rebuilds_return_no_flags_and_changes_return_the_right_ones() {
    let _g = hold();
    let series = Series::new(8);
    let base = || sparkline(series.clone());
    assert_eq!(flags(base(), base()), ChangeFlags::NONE);
    assert_eq!(
        flags(base().size(5.0, 5.0), base().size(5.0, 5.0)),
        ChangeFlags::NONE
    );
    assert_eq!(flags(base(), base().color(RED)), ChangeFlags::PAINT);
    assert_eq!(flags(base(), base().style(line(2.0))), ChangeFlags::PAINT);
    assert_eq!(
        flags(base(), base().tone(Tone::new(RED))),
        ChangeFlags::PAINT
    );
    assert_eq!(
        flags(base(), base().domain(YDomain::auto_from_zero())),
        ChangeFlags::PAINT
    );
    assert_eq!(flags(base(), sparkline(Series::new(8))), ChangeFlags::PAINT);
    assert_eq!(
        flags(base(), base().size(10.0, 10.0)),
        ChangeFlags::LAYOUT | ChangeFlags::PAINT
    );
}

#[test]
fn input_is_ignored() {
    let _g = hold();
    let down = InputEvent::Pointer(PointerEvent {
        phase: PointerPhase::Down,
        position: Point::new(10.0, 3.0),
        button: PointerButton::Primary,
    });
    let mut counter = 0u64;
    let view = sparkline(Series::new(4));
    let mut widget = <SparklineView as View<()>>::build(&view, &mut BuildCtx::new(&mut counter));
    let mut state = ();
    let mut ctx = EventCtx::new(&mut state, Point::ZERO, Size::new(160.0, 24.0));
    assert!(matches!(
        widget.event(&mut ctx, &down),
        EventResult::Ignored
    ));
    let mut harness = Harness::new(sparkline(Series::new(4)), Size::new(160.0, 24.0));
    harness.paint();
    assert!(!harness.root.event(&mut (), &down).handled);
    assert!(!harness.root.is_pointer_captured());
}

#[test]
fn typical_sizes_paint_within_bounds() {
    let _g = hold();
    for (w, h) in [(160.0, 24.0), (108.0, 16.0)] {
        let series = Series::new(200);
        series.extend((0..200).map(|i| Some((i * 7 % 101) as f32)));
        for style in [SparkStyle::default(), line(1.0), line(2.0)] {
            let window = Size::new(w, h);
            let ops = Harness::new(
                sparkline(series.clone())
                    .style(style)
                    .size(w as f32, h as f32),
                window,
            )
            .paint();
            for op in &ops {
                match op {
                    Op::Rect(o, s, _) => {
                        assert!(o.x >= 0.0 && o.y >= 0.0, "{op:?}");
                        assert!(o.x + s.width <= w && o.y + s.height <= h, "{op:?}");
                    }
                    Op::Stroke(points, ..) => {
                        for p in points {
                            assert!((0.0..=w).contains(&p.x) && (0.0..=h).contains(&p.y));
                        }
                    }
                    _ => {}
                }
            }
            assert!(matches!(&ops[0], Op::Clip(o, s) if *o == Point::ZERO && *s == window));
        }
    }
}

#[test]
fn empty_gap_only_and_zero_size_paint_nothing_and_do_not_panic() {
    let _g = hold();
    let gaps = Series::new(5);
    gaps.extend([None, None, None]);
    for series in [Series::new(1), Series::new(5), gaps] {
        for style in [SparkStyle::default(), line(1.0)] {
            let ops = Harness::new(
                sparkline(series.clone()).style(style),
                Size::new(160.0, 24.0),
            )
            .paint();
            assert!(ops.is_empty(), "{ops:?}");
        }
    }
    let series = Series::new(5);
    series.push(50.0);
    for window in [Size::ZERO, Size::new(0.0, 24.0), Size::new(160.0, 0.0)] {
        let ops = Harness::new(sparkline(series.clone()), window).paint();
        assert!(ops.is_empty());
    }
}

#[test]
fn semantics_need_a_label_and_carry_the_value_description() {
    let _g = hold();
    let window = Size::new(160.0, 24.0);
    assert!(
        Harness::new(sparkline(Series::new(4)), window)
            .nodes()
            .is_empty()
    );
    let view = sparkline(Series::new(4))
        .semantics("CPU 3")
        .semantic_value("42 percent");
    let [node] = Harness::new(view, window)
        .nodes()
        .try_into()
        .expect("one node");
    assert_eq!(node.role(), Role::Image);
    assert_eq!(node.label(), Some("CPU 3"));
    assert_eq!(node.description(), Some("42 percent"));
}
