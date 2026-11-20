//! The `gauge` widget headless: a real `RenderRoot` painting into a recording scene.
//!
//! A `Samples` write requests a process-wide repaint, so every test holds `STATICS`.

use std::f64::consts::{PI, TAU};
use std::sync::{Mutex, MutexGuard, PoisonError};

use frust::authoring::scene::{DashPattern, StrokeCap, StrokeStyle};
use frust::authoring::{
    BezPath, Brush, BuildCtx, ChangeFlags, EventCtx, EventResult, InputEvent, Node, PaintScene,
    Point, PointerButton, PointerEvent, PointerPhase, Rect, Role, Shape, Size, View, Widget,
};
use frust::kurbo::PathEl;
use frust::{Color, FrameTime, SizedBox, Theme, colored_box};
use frust_charts::{GaugeCap, GaugeView, NeedleStyle, Samples, Tone, gauge, point_on_circle};
use frust_core::RenderRoot;

static STATICS: Mutex<()> = Mutex::new(());

fn hold() -> MutexGuard<'static, ()> {
    STATICS.lock().unwrap_or_else(PoisonError::into_inner)
}

const WINDOW: Size = Size::new(200.0, 200.0);
const RED: Color = Color::from_rgb8(0xff, 0, 0);
const BLUE: Color = Color::from_rgb8(0, 0, 0xff);
const GREEN: Color = Color::from_rgb8(0, 0xff, 0);
const GREY: Color = Color::from_rgb8(0x80, 0x80, 0x80);
const BLACK: Color = Color::from_rgb8(0, 0, 0);
const EPS: f64 = 1e-6;
/// How far a Bezier arc's control hull may stray past its circle.
const CURVE_EPS: f64 = 0.05;

/// The default dial in `WINDOW`: centred, radius 100, track 10 px thick.
const CENTER: Point = Point::new(100.0, 100.0);
const MID: f64 = 95.0;
const INNER: f64 = 90.0;

#[derive(Clone, Debug)]
enum Op {
    Stroke {
        path: BezPath,
        width: f64,
        cap: StrokeCap,
        color: Color,
    },
    Fill {
        path: BezPath,
        color: Color,
    },
    Rect(Rect, Color),
}

#[derive(Default)]
struct Rec {
    ops: Vec<Op>,
}

fn solid(brush: &Brush) -> Color {
    match brush {
        Brush::Solid(color) => *color,
        other => panic!("expected a solid brush, got {other:?}"),
    }
}

fn shifted(origin: Point, path: &BezPath) -> BezPath {
    let mut path = path.clone();
    path.apply_affine(frust::authoring::Affine::translate(origin.to_vec2()));
    path
}

impl PaintScene for Rec {
    fn fill_rect(&mut self, origin: Point, size: Size, color: Color) {
        self.ops
            .push(Op::Rect(Rect::from_origin_size(origin, size), color));
    }
    fn fill_rounded_rect(&mut self, origin: Point, size: Size, _radius: f64, color: Color) {
        self.fill_rect(origin, size, color);
    }
    fn fill_path(&mut self, origin: Point, path: &BezPath, brush: &Brush) {
        self.ops.push(Op::Fill {
            path: shifted(origin, path),
            color: solid(brush),
        });
    }
    fn stroke_path(&mut self, _origin: Point, _path: &BezPath, _width: f64, _brush: &Brush) {
        panic!("a gauge strokes with an explicit style");
    }
    fn stroke_path_styled(
        &mut self,
        origin: Point,
        path: &BezPath,
        width: f64,
        dash: Option<DashPattern>,
        style: StrokeStyle,
        brush: &Brush,
    ) {
        assert!(dash.is_none(), "a gauge draws no dashes");
        self.ops.push(Op::Stroke {
            path: shifted(origin, path),
            width,
            cap: style.cap,
            color: solid(brush),
        });
    }
    fn draw_text(&mut self, _origin: Point, _text: &str) {}
}

impl Rec {
    fn strokes(&self) -> Vec<(BezPath, f64, StrokeCap, Color)> {
        self.ops
            .iter()
            .filter_map(|op| match op {
                Op::Stroke {
                    path,
                    width,
                    cap,
                    color,
                } => Some((path.clone(), *width, *cap, *color)),
                _ => None,
            })
            .collect()
    }

    fn strokes_in(&self, color: Color) -> Vec<(BezPath, f64, StrokeCap)> {
        self.strokes()
            .into_iter()
            .filter(|(_, _, _, c)| *c == color)
            .map(|(p, w, cap, _)| (p, w, cap))
            .collect()
    }

    fn fills_in(&self, color: Color) -> Vec<BezPath> {
        self.ops
            .iter()
            .filter_map(|op| match op {
                Op::Fill { path, color: c } if *c == color => Some(path.clone()),
                _ => None,
            })
            .collect()
    }
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

    fn paint(&mut self) -> Rec {
        self.root.layout(self.window);
        let mut rec = Rec::default();
        self.root
            .paint(&mut rec, FrameTime::from_nanos(1_000_000_000));
        rec
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

fn paint<V: View<()>>(view: V) -> Rec {
    Harness::new(view, WINDOW).paint()
}

fn g(value: impl Into<frust_charts::MeterValue>) -> GaugeView<()> {
    gauge(value)
}

fn near_point(a: Point, b: Point) -> bool {
    (a - b).hypot() < 1e-6
}

fn first_point(path: &BezPath) -> Point {
    match path.elements().first() {
        Some(PathEl::MoveTo(p)) => *p,
        other => panic!("a path starts with a move: {other:?}"),
    }
}

fn last_point(path: &BezPath) -> Point {
    path.elements()
        .last()
        .and_then(|el| el.end_point())
        .expect("a non-empty path")
}

/// An angle handed to the gauge as `f32` radians, in the `f64` degrees the gauge works with.
fn degrees_of(radians: f32) -> f64 {
    f64::from(radians).to_degrees()
}

fn on_arc(radius: f64, degrees: f64) -> Point {
    point_on_circle(CENTER, radius, degrees.to_radians())
}

/// The arc's start and end points, which pin its sweep given the start angle and direction.
fn assert_arc(path: &BezPath, radius: f64, from_degrees: f64, to_degrees: f64) {
    assert!(
        near_point(first_point(path), on_arc(radius, from_degrees)),
        "starts at {from_degrees}: {:?}",
        first_point(path)
    );
    assert!(
        near_point(last_point(path), on_arc(radius, to_degrees)),
        "ends at {to_degrees}: {:?}",
        last_point(path)
    );
}

/// The total angle an arc path turns through around `CENTER`, from its vertices.
fn swept_degrees(path: &BezPath) -> f64 {
    let mut total = 0.0;
    let mut previous: Option<f64> = None;
    for el in path.elements() {
        let Some(p) = el.end_point() else { continue };
        let angle = (p.y - CENTER.y).atan2(p.x - CENTER.x);
        if let Some(prev) = previous {
            let mut step = angle - prev;
            if step > PI {
                step -= TAU;
            } else if step < -PI {
                step += TAU;
            }
            total += step;
        }
        previous = Some(angle);
    }
    total.to_degrees()
}

#[test]
fn value_arc_sweeps_its_share_of_the_arc_and_clamps() {
    let _g = hold();
    for (percent, sweep) in [(0.0, None), (25.0, Some(67.5)), (100.0, Some(270.0))]
        .into_iter()
        .chain([(150.0, Some(270.0))])
    {
        let rec = paint(g(percent).color(RED).track(GREY));
        let track = rec.strokes_in(GREY);
        assert_eq!(track.len(), 1, "one track at {percent}");
        let (path, width, cap) = &track[0];
        assert_eq!((*width, *cap), (10.0, StrokeCap::Round));
        assert_arc(path, MID, 135.0, 405.0);
        assert!((swept_degrees(path) - 270.0).abs() < 1e-6);

        let value = rec.strokes_in(RED);
        match sweep {
            None => assert!(value.is_empty(), "the minimum draws no value arc"),
            Some(sweep) => {
                assert_eq!(value.len(), 1, "one value arc at {percent}");
                let (path, width, cap) = &value[0];
                assert_eq!((*width, *cap), (10.0, StrokeCap::Round));
                assert_arc(path, MID, 135.0, 135.0 + sweep);
                assert!(
                    (swept_degrees(path) - sweep).abs() < 1e-6,
                    "{percent}: {}",
                    swept_degrees(path)
                );
            }
        }
    }
}

#[test]
fn angles_range_thickness_and_cap_shape_the_arcs() {
    let _g = hold();
    let (start, sweep) = (std::f32::consts::PI, -std::f32::consts::PI);
    let (from, turn) = (degrees_of(start), degrees_of(sweep));
    let rec = paint(
        g(30.0)
            .range(-10.0, 90.0)
            .angles(start, sweep)
            .thickness(20.0)
            .cap(GaugeCap::Butt)
            .color(RED)
            .track(GREY),
    );
    let (track, width, cap) = &rec.strokes_in(GREY)[0];
    assert_eq!((*width, *cap), (20.0, StrokeCap::Butt));
    assert_arc(track, 90.0, from, from + turn);
    assert!(
        (swept_degrees(track) - turn).abs() < 1e-6,
        "counter-clockwise"
    );
    // 30 is 40 % of -10..90: 72 degrees counter-clockwise from 9 o'clock.
    let (value, _, _) = &rec.strokes_in(RED)[0];
    assert!((0.4 * turn + 72.0).abs() < 1e-4);
    assert_arc(value, 90.0, from, from + 0.4 * turn);
    assert!((swept_degrees(value) - 0.4 * turn).abs() < 1e-6);

    let full = paint(
        g(50.0)
            .angles(-std::f32::consts::FRAC_PI_2, 2.0 * std::f32::consts::TAU)
            .track(GREY),
    );
    let (ring, _, cap) = &full.strokes_in(GREY)[0];
    assert_eq!(*cap, StrokeCap::Butt, "a closed ring has no ends to round");
    assert!(
        (swept_degrees(ring) - 360.0).abs() < 1e-6,
        "clamped to one turn"
    );
}

#[test]
fn zones_cover_their_angular_spans_and_clamp_to_the_range() {
    let _g = hold();
    let zones = [
        (0.0, 50.0, GREEN),
        (80.0, 50.0, BLUE),
        (90.0, 150.0, RED),
        (120.0, 130.0, BLACK),
        (f32::NAN, 10.0, BLACK),
    ];
    let rec = paint(g(None).zones(zones).cap(GaugeCap::Butt).track(GREY));
    assert!(
        rec.strokes_in(BLACK).is_empty(),
        "out of range or NaN zones drop"
    );
    for (color, from, to) in [
        (GREEN, 135.0, 270.0),
        (BLUE, 270.0, 351.0),
        (RED, 378.0, 405.0),
    ] {
        let strokes = rec.strokes_in(color);
        assert_eq!(strokes.len(), 1);
        let (path, width, cap) = &strokes[0];
        assert_eq!((*width, *cap), (10.0, StrokeCap::Butt), "zones meet flush");
        assert_arc(path, MID, from, to);
        assert!((swept_degrees(path) - (to - from)).abs() < 1e-6);
    }
    assert!(
        rec.ops.iter().all(|op| !matches!(op, Op::Fill { .. })),
        "butt caps need no cap discs"
    );
    // Paint order: the track, then the zones as given.
    let order: Vec<Color> = rec.strokes().iter().map(|s| s.3).collect();
    assert_eq!(order, [GREY, GREEN, BLUE, RED]);
}

#[test]
fn round_capped_zones_get_a_half_disc_at_each_end_of_the_range() {
    let _g = hold();
    let rec = paint(g(None).zones([(0.0, 40.0, GREEN), (40.0, 100.0, RED)]));
    let green = rec.fills_in(GREEN);
    let red = rec.fills_in(RED);
    assert_eq!((green.len(), red.len()), (1, 1));
    for (cap, end_degrees, outward_degrees) in [(&green[0], 135.0, 45.0), (&red[0], 405.0, 135.0)] {
        let end = on_arc(MID, end_degrees);
        let bounds = cap.bounding_box();
        assert!(
            (bounds.center() - end).hypot() < 5.0 + EPS,
            "the cap sits on the arc end"
        );
        // The disc reaches 5 px past the end, along the arc's direction of travel.
        let tip = point_on_circle(end, 5.0, f64::to_radians(outward_degrees));
        assert!(
            cap.elements()
                .iter()
                .filter_map(|el| el.end_point())
                .any(|p| near_point(p, tip)),
            "{cap:?} reaches {tip:?}"
        );
        assert!(
            cap.elements()
                .iter()
                .filter_map(|el| el.end_point())
                .all(|p| (p - end).hypot() <= 5.0 + EPS),
            "a half-disc of half the thickness"
        );
        // Its flat side is the radial line through the end: no point lies inside the zone.
        let along = Point::new(
            -f64::to_radians(end_degrees).sin(),
            f64::to_radians(end_degrees).cos(),
        )
        .to_vec2();
        let sign = if end_degrees < 200.0 { -1.0 } else { 1.0 };
        assert!(
            cap.elements()
                .iter()
                .filter_map(|el| el.end_point())
                .all(|p| sign * (p - end).dot(along) >= -EPS),
            "the disc bulges outward only"
        );
    }
}

#[test]
fn needle_points_at_the_value_from_a_hub() {
    let _g = hold();
    let needle = NeedleStyle::new().color(BLACK).width(4.0).hub(6.0);
    let rec = paint(g(50.0).needle(needle).color(RED));
    let strokes = rec.strokes_in(BLACK);
    assert_eq!(strokes.len(), 1);
    let (path, width, cap) = &strokes[0];
    assert_eq!((*width, *cap), (4.0, StrokeCap::Round));
    // 50 % of 270 from 135 degrees is 270: straight up; the round tip stays inside the track.
    assert!(near_point(first_point(path), CENTER));
    assert!(near_point(last_point(path), on_arc(INNER - 2.0, 270.0)));
    let hub = rec.fills_in(BLACK);
    assert_eq!(hub.len(), 1);
    let bounds = hub[0].bounding_box();
    assert!((bounds.center() - CENTER).hypot() < 1e-3 && (bounds.width() - 12.0).abs() < 1e-3);
    assert!(
        matches!(rec.ops.last(), Some(Op::Fill { .. })),
        "the needle is drawn last"
    );

    let short = paint(g(0.0).needle(NeedleStyle::new().color(BLACK).length(0.5).hub(0.0)));
    let (path, _, _) = &short.strokes_in(BLACK)[0];
    assert!(near_point(last_point(path), on_arc(48.5, 135.0)));
    assert!(short.fills_in(BLACK).is_empty(), "no hub");
}

#[test]
fn ticks_are_evenly_spaced_marks_inside_the_track() {
    let _g = hold();
    let rec = paint(g(None).ticks(5, 6.0, BLUE));
    let ticks = rec.strokes_in(BLUE);
    assert_eq!(ticks.len(), 1, "every tick in one stroke");
    let (path, width, cap) = &ticks[0];
    assert_eq!((*width, *cap), (1.0, StrokeCap::Butt));
    let moves: Vec<Point> = path
        .elements()
        .iter()
        .filter_map(|el| match el {
            PathEl::MoveTo(p) => Some(*p),
            _ => None,
        })
        .collect();
    let lines: Vec<Point> = path
        .elements()
        .iter()
        .filter_map(|el| match el {
            PathEl::LineTo(p) => Some(*p),
            _ => None,
        })
        .collect();
    assert_eq!((moves.len(), lines.len()), (5, 5));
    for (i, (outer, inner)) in moves.iter().zip(&lines).enumerate() {
        let degrees = 135.0 + 67.5 * i as f64;
        assert!(near_point(*outer, on_arc(INNER - 2.0, degrees)), "tick {i}");
        assert!(near_point(*inner, on_arc(INNER - 8.0, degrees)), "tick {i}");
    }

    let top = -std::f32::consts::FRAC_PI_2;
    let ring = paint(
        g(None)
            .angles(top, std::f32::consts::TAU)
            .ticks(4, 6.0, BLUE),
    );
    let (path, _, _) = &ring.strokes_in(BLUE)[0];
    let starts: Vec<Point> = path
        .elements()
        .iter()
        .filter_map(|el| match el {
            PathEl::MoveTo(p) => Some(*p),
            _ => None,
        })
        .collect();
    assert_eq!(starts.len(), 4);
    for (i, p) in starts.iter().enumerate() {
        let degrees = degrees_of(top) + 90.0 * i as f64;
        assert!(
            near_point(*p, on_arc(INNER - 2.0, degrees)),
            "no doubled tick"
        );
    }
    assert!(
        paint(g(None).ticks(0, 6.0, BLUE))
            .strokes_in(BLUE)
            .is_empty()
    );
}

#[test]
fn a_gap_draws_the_track_only() {
    let _g = hold();
    let needle = NeedleStyle::new().color(BLACK);
    for value in [None, Some(f32::NAN)] {
        let rec = paint(g(value).needle(needle).color(RED).track(GREY));
        assert_eq!(rec.ops.len(), 1, "{:?}", rec.ops);
        assert_eq!(rec.strokes_in(GREY).len(), 1);
    }
}

#[test]
fn a_live_slot_repaints_with_no_rebuild() {
    let _g = hold();
    let samples = Samples::new(3);
    samples.set(1, 25.0);
    let needle = NeedleStyle::new().color(BLACK);
    let mut harness = Harness::new(g(samples.slot(1)).color(RED).needle(needle), WINDOW);
    let rec = harness.paint();
    assert!((swept_degrees(&rec.strokes_in(RED)[0].0) - 67.5).abs() < 1e-6);
    assert!(!harness.root.repaint_pending());

    samples.set(1, 50.0);
    assert!(harness.root.repaint_pending(), "a write asks for a repaint");
    let rec = harness.paint();
    assert!((swept_degrees(&rec.strokes_in(RED)[0].0) - 135.0).abs() < 1e-6);
    let (needle, _, _) = &rec.strokes_in(BLACK)[0];
    assert!(near_point(last_point(needle), on_arc(INNER - 1.5, 270.0)));
    assert!(!harness.root.repaint_pending());

    samples.set_gap(1);
    let rec = harness.paint();
    assert!(rec.strokes_in(RED).is_empty() && rec.strokes_in(BLACK).is_empty());
}

#[test]
fn colours_follow_caller_then_theme_then_fallbacks() {
    let _g = hold();
    let needle = NeedleStyle::new();
    let bare = paint(g(70.0).needle(needle));
    let colors: Vec<Color> = bare.strokes().iter().map(|s| s.3).collect();
    assert_eq!(
        colors,
        [
            frust_charts::MeterView::FALLBACK_TRACK,
            Tone::FALLBACK_CALM,
            GaugeView::<()>::FALLBACK_NEEDLE,
        ]
    );

    let theme = Theme::neutral();
    let scheme = *theme.scheme();
    let themed = Harness::themed(g(70.0).needle(needle), WINDOW, Some(theme)).paint();
    let colors: Vec<Color> = themed.strokes().iter().map(|s| s.3).collect();
    assert_eq!(
        colors,
        [
            scheme.surface_container_highest,
            scheme.primary,
            scheme.on_surface
        ]
    );

    let toned = paint(g(70.0).color(BLUE).tone(Tone::load(GREEN, BLUE, RED)));
    assert_eq!(
        toned.strokes_in(BLUE).len(),
        1,
        "the tone wins over the colour"
    );
    let toned = paint(g(90.0).tone(Tone::load(GREEN, BLUE, RED)));
    assert_eq!(toned.strokes_in(RED).len(), 1);
}

#[test]
fn stays_inside_a_non_square_box() {
    let _g = hold();
    let size = Size::new(240.0, 120.0);
    let view = SizedBox(Some(size.width), Some(size.height)).child(
        g(75.0)
            .thickness(14.0)
            .zones([(0.0, 60.0, GREEN), (60.0, 100.0, RED)])
            .needle(NeedleStyle::new().color(BLACK).length(1.0).hub(8.0))
            .ticks(11, 8.0, BLUE),
    );
    let rec = Harness::new(view, Size::new(400.0, 300.0)).paint();
    let bounds = Rect::from_origin_size(Point::ZERO, size);
    let center = Point::new(120.0, 60.0);
    assert_eq!(
        rec.strokes().len(),
        6,
        "track, two zones, ticks, value, needle"
    );
    for op in &rec.ops {
        match op {
            Op::Stroke { path, width, .. } => {
                for p in path.elements().iter().filter_map(|el| el.end_point()) {
                    let reach = (p - center).hypot() + width / 2.0;
                    assert!(reach <= 60.0 + EPS, "{p:?} + {width} leaves the circle");
                }
                let grown = path.bounding_box().inflate(width / 2.0, width / 2.0);
                assert!(
                    grown.x0 >= bounds.x0 - CURVE_EPS
                        && grown.y0 >= bounds.y0 - CURVE_EPS
                        && grown.x1 <= bounds.x1 + CURVE_EPS
                        && grown.y1 <= bounds.y1 + CURVE_EPS,
                    "{grown:?}"
                );
            }
            Op::Fill { path, .. } => {
                let b = path.bounding_box();
                let e = CURVE_EPS;
                assert!(b.x0 >= -e && b.y0 >= -e && b.x1 <= 240.0 + e && b.y1 <= 120.0 + e);
            }
            Op::Rect(..) => panic!("a gauge draws no rects"),
        }
    }
}

#[test]
fn size_sets_the_side_and_an_unset_size_takes_the_shorter_side() {
    let _g = hold();
    let rec = Harness::new(g(None).track(GREY), Size::new(300.0, 160.0)).paint();
    let (track, _, _) = &rec.strokes_in(GREY)[0];
    let b = track.bounding_box();
    assert!(
        (b.min_y() - 5.0).abs() < CURVE_EPS,
        "radius 80 in a 160 px square: {b:?}"
    );
    let rec = paint(g(None).size(60.0).track(GREY));
    let (track, _, _) = &rec.strokes_in(GREY)[0];
    assert!(near_point(
        first_point(track),
        point_on_circle(Point::new(30.0, 30.0), 25.0, 0.75 * PI)
    ));
}

#[test]
fn a_centre_child_is_laid_out_in_the_middle_under_the_needle() {
    let _g = hold();
    let child = colored_box().size(40.0, 20.0).fill(GREEN);
    let view = g(50.0)
        .center(child)
        .needle(NeedleStyle::new().color(BLACK).hub(0.0))
        .semantics("Speed");
    let mut harness = Harness::new(view, WINDOW);
    let rec = harness.paint();
    let rects: Vec<&Op> = rec
        .ops
        .iter()
        .filter(|op| matches!(op, Op::Rect(..)))
        .collect();
    assert_eq!(rects.len(), 1);
    let Op::Rect(rect, color) = rects[0] else {
        unreachable!()
    };
    assert_eq!(*color, GREEN);
    assert_eq!(*rect, Rect::new(80.0, 90.0, 120.0, 110.0), "centred");
    assert!(
        matches!(rec.ops.last(), Some(Op::Stroke { color, .. }) if *color == BLACK),
        "the needle is on top"
    );
    let [node] = harness.nodes().try_into().expect("one node");
    assert_eq!(node.role(), Role::ProgressIndicator);

    // A child larger than the inscribed square is held inside it.
    let big = colored_box().size(500.0, 500.0).fill(GREEN);
    let rec = paint(g(50.0).center(big));
    let Some(Op::Rect(rect, _)) = rec.ops.iter().find(|op| matches!(op, Op::Rect(..))) else {
        panic!("the child paints")
    };
    let side = INNER * std::f64::consts::SQRT_2;
    assert!((rect.width() - side).abs() < 1e-6 && (rect.center() - CENTER).hypot() < 1e-6);
}

#[test]
fn rebuild_flags_match_what_changed() {
    let _g = hold();
    fn flags(old: GaugeView<()>, new: GaugeView<()>) -> ChangeFlags {
        let mut counter = 0u64;
        let mut ctx = BuildCtx::new(&mut counter);
        let mut widget = old.build(&mut ctx);
        new.rebuild(&old, &mut widget, &mut ctx)
    }
    assert_eq!(flags(g(10.0), g(10.0)), ChangeFlags::NONE);
    assert_eq!(flags(g(10.0), g(20.0)), ChangeFlags::PAINT);
    assert_eq!(flags(g(10.0), g(10.0).color(RED)), ChangeFlags::PAINT);
    assert_eq!(
        flags(g(10.0), g(10.0).zones([(0.0, 5.0, RED)])),
        ChangeFlags::PAINT
    );
    assert_eq!(
        flags(g(10.0), g(10.0).needle(NeedleStyle::new())),
        ChangeFlags::PAINT
    );
    assert_eq!(flags(g(10.0), g(10.0).semantics("x")), ChangeFlags::PAINT);
    let layout = ChangeFlags::LAYOUT | ChangeFlags::PAINT;
    assert_eq!(flags(g(10.0), g(10.0).thickness(4.0)), layout);
    assert_eq!(flags(g(10.0), g(10.0).size(50.0)), layout);
    assert!(flags(g(10.0), g(10.0).center(colored_box())).contains(ChangeFlags::LAYOUT));

    let samples = Samples::new(2);
    let live = || g(samples.slot(0));
    assert_eq!(flags(live(), live()), ChangeFlags::NONE, "same slot");
    assert_eq!(flags(live(), g(samples.slot(1))), ChangeFlags::PAINT);
}

#[test]
fn a_new_option_through_a_rebuild_redraws_the_frame() {
    let _g = hold();
    let mut root: RenderRoot<f32, GaugeView<f32>> = RenderRoot::new();
    let mut logic = |thickness: &mut f32| gauge(50.0).thickness(*thickness).track(GREY);
    let mut thickness = 10.0;
    root.rebuild(&mut logic, &mut thickness);
    root.layout(WINDOW);
    let mut rec = Rec::default();
    root.paint(&mut rec, FrameTime::from_nanos(1));
    assert_eq!(rec.strokes_in(GREY)[0].1, 10.0);
    thickness = 20.0;
    root.rebuild(&mut logic, &mut thickness);
    root.layout(WINDOW);
    let mut rec = Rec::default();
    root.paint(&mut rec, FrameTime::from_nanos(2));
    let (track, width, _) = &rec.strokes_in(GREY)[0];
    assert_eq!(*width, 20.0);
    assert!(near_point(first_point(track), on_arc(90.0, 135.0)));
}

#[test]
fn a_fixed_value_change_through_a_rebuild_is_paint_only_and_redraws_the_arc() {
    let _g = hold();
    let mut root: RenderRoot<f32, GaugeView<f32>> = RenderRoot::new();
    let mut logic = |value: &mut f32| gauge(*value).color(RED).zones([(0.0, 50.0, GREEN)]);
    let mut value = 25.0;
    root.rebuild(&mut logic, &mut value);
    root.layout(WINDOW);
    root.take_change_flags();
    let mut rec = Rec::default();
    root.paint(&mut rec, FrameTime::from_nanos(1));
    assert!((swept_degrees(&rec.strokes_in(RED)[0].0) - 67.5).abs() < 1e-6);
    value = 100.0;
    assert_eq!(root.rebuild(&mut logic, &mut value), ChangeFlags::PAINT);
    let mut rec = Rec::default();
    root.paint(&mut rec, FrameTime::from_nanos(2));
    assert!((swept_degrees(&rec.strokes_in(RED)[0].0) - 270.0).abs() < 1e-6);
    assert_eq!(rec.strokes_in(GREEN).len(), 1, "the zone still draws");
}

#[test]
fn semantics_report_value_range_and_label() {
    let _g = hold();
    let harness = Harness::new(g(150.0).range(0.0, 120.0).semantics("Fan"), WINDOW);
    let [node] = harness.nodes().try_into().expect("one node");
    assert_eq!(node.role(), Role::ProgressIndicator);
    assert_eq!(node.numeric_value(), Some(120.0), "clamped into the range");
    assert_eq!(node.min_numeric_value(), Some(0.0));
    assert_eq!(node.max_numeric_value(), Some(120.0));
    assert_eq!(node.label(), Some("Fan"));

    let [gap] = Harness::new(g(None).semantics("Fan"), WINDOW)
        .nodes()
        .try_into()
        .expect("one node");
    assert_eq!(gap.numeric_value(), None);

    assert!(
        Harness::new(g(40.0), WINDOW).nodes().is_empty(),
        "no label, no node"
    );
}

#[test]
fn gauges_ignore_input() {
    let _g = hold();
    let down = InputEvent::Pointer(PointerEvent {
        phase: PointerPhase::Down,
        position: Point::new(100.0, 100.0),
        button: PointerButton::Primary,
    });
    let mut counter = 0u64;
    let view = g(50.0);
    let mut widget = view.build(&mut BuildCtx::new(&mut counter));
    let mut state = ();
    let mut ctx = EventCtx::new(&mut state, Point::ZERO, WINDOW);
    assert!(matches!(
        widget.event(&mut ctx, &down),
        EventResult::Ignored
    ));

    let mut harness = Harness::new(g(50.0), WINDOW);
    harness.paint();
    assert!(!harness.root.event(&mut (), &down).handled);
    assert!(!harness.root.is_pointer_captured());
    assert!(
        !harness.root.repaint_pending(),
        "a gauge requests no frames"
    );
}
