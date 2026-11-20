//! `radar_chart` headless: a real `RenderRoot` painting into a recording scene.
//!
//! Writing a `Categories` requests a process-wide repaint, so every test holds `STATICS`.

use std::any::Any;
use std::f64::consts::{FRAC_PI_2, TAU};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use frust::authoring::scene::GlyphRun;
use frust::authoring::text::{TextContext, TextStyle};
use frust::authoring::{
    BezPath, Brush, ChangeFlags, EventOutcome, InputEvent, PaintScene, Point, PointerButton,
    PointerEvent, PointerPhase, Rect, Role, Size,
};
use frust::kurbo::PathEl;
use frust::{Color, FrameTime};
use frust_charts::{
    Categories, CategoryRow, RadarChartView, RadarShape, RadarStyle, Tone, YDomain,
    point_on_circle, radar_chart,
};
use frust_core::RenderRoot;
use frust_reactive::ReactiveRuntime;

static STATICS: Mutex<()> = Mutex::new(());

fn hold() -> MutexGuard<'static, ()> {
    STATICS.lock().unwrap_or_else(PoisonError::into_inner)
}

const RED: Color = Color::from_rgb8(0xd0, 0x10, 0x10);
const BLUE: Color = Color::from_rgb8(0x10, 0x20, 0xd0);
const GREY: Color = Color::from_rgb8(0x80, 0x80, 0x80);
const EPS: f64 = 1e-6;
/// The title size with no theme installed.
const TITLE_SIZE: f32 = 11.0;
/// The space between the outer ring and a title's anchor.
const TITLE_GAP: f64 = 4.0;

/// One recorded path: its vertices (subpath end points, in scene space), how many subpaths it
/// has and whether it holds curves.
#[derive(Clone, Debug)]
struct Shape {
    points: Vec<Point>,
    subpaths: usize,
    curved: bool,
}

impl Shape {
    fn of(origin: Point, path: &BezPath) -> Self {
        let elements = path.elements();
        Self {
            points: elements
                .iter()
                .filter_map(|el| el.end_point())
                .map(|p| p + origin.to_vec2())
                .collect(),
            subpaths: elements
                .iter()
                .filter(|el| matches!(el, PathEl::MoveTo(_)))
                .count(),
            curved: elements.iter().any(|el| matches!(el, PathEl::CurveTo(..))),
        }
    }
}

/// One recorded draw.
#[derive(Clone, Debug)]
enum Op {
    Fill(Shape, Color),
    Stroke(Shape, f64, Color),
    Glyphs(Vec<Point>),
    Other,
}

#[derive(Default)]
struct Rec {
    ops: Vec<Op>,
    clips: Vec<Rect>,
    depth: usize,
}

fn solid(brush: &Brush) -> Color {
    match brush {
        Brush::Solid(color) => *color,
        other => panic!("expected a solid brush, got {other:?}"),
    }
}

impl Rec {
    fn strokes_in(&self, color: Color) -> Vec<(Shape, f64)> {
        self.ops
            .iter()
            .filter_map(|op| match op {
                Op::Stroke(shape, width, c) if *c == color => Some((shape.clone(), *width)),
                _ => None,
            })
            .collect()
    }

    /// The web's strokes: every ring, inner to outer, then the spokes.
    fn web(&self) -> Vec<Shape> {
        self.strokes_in(GREY).into_iter().map(|(s, _)| s).collect()
    }

    fn fills(&self) -> Vec<(Shape, Color)> {
        self.ops
            .iter()
            .filter_map(|op| match op {
                Op::Fill(shape, color) => Some((shape.clone(), *color)),
                _ => None,
            })
            .collect()
    }

    /// The glyph origins of every run, in draw order.
    fn runs(&self) -> Vec<Vec<Point>> {
        self.ops
            .iter()
            .filter_map(|op| match op {
                Op::Glyphs(points) => Some(points.clone()),
                _ => None,
            })
            .collect()
    }
}

impl PaintScene for Rec {
    fn fill_rect(&mut self, _: Point, _: Size, _: Color) {
        self.ops.push(Op::Other);
    }
    fn fill_rounded_rect(&mut self, _: Point, _: Size, _: f64, _: Color) {
        self.ops.push(Op::Other);
    }
    fn fill_path(&mut self, origin: Point, path: &BezPath, brush: &Brush) {
        self.ops
            .push(Op::Fill(Shape::of(origin, path), solid(brush)));
    }
    fn stroke_path(&mut self, origin: Point, path: &BezPath, width: f64, brush: &Brush) {
        self.ops
            .push(Op::Stroke(Shape::of(origin, path), width, solid(brush)));
    }
    fn push_clip(&mut self, origin: Point, size: Size) {
        self.clips.push(Rect::from_origin_size(origin, size));
        self.depth += 1;
    }
    fn pop_clip(&mut self) {
        self.depth -= 1;
    }
    fn draw_glyph_run(&mut self, run: GlyphRun) {
        let points = run
            .glyphs
            .iter()
            .map(|g| run.transform * Point::new(f64::from(g.x), f64::from(g.y)))
            .collect();
        self.ops.push(Op::Glyphs(points));
    }
    fn draw_text(&mut self, _origin: Point, _text: &str) {}
}

type View = RadarChartView;

struct Rig {
    rt: &'static ReactiveRuntime,
    root: RenderRoot<(), View>,
    text: TextContext,
    size: Size,
}

impl Rig {
    fn rebuild(&mut self, view: impl Fn() -> View) -> ChangeFlags {
        let mut logic = |_: &mut ()| view();
        self.rt
            .with_owner(|| self.root.rebuild(&mut logic, &mut ()))
    }

    fn layout(&mut self) {
        self.root
            .layout_with_text(self.size, &mut self.text as &mut dyn Any);
    }

    /// A repaint-only frame, as a data write gets: no rebuild and no layout. Returns what was
    /// drawn and whether the paint requested a layout.
    fn repaint(&mut self) -> (Rec, bool) {
        let mut rec = Rec::default();
        let outcome = self
            .root
            .paint(&mut rec, FrameTime::from_nanos(1_000_000_000));
        assert_eq!(rec.depth, 0, "every pushed clip is popped");
        (rec, outcome.needs_layout)
    }

    /// A full frame: layout, then paint.
    fn frame(&mut self) -> (Rec, bool) {
        self.layout();
        self.repaint()
    }
}

fn rig(width: f64, height: f64, view: impl Fn() -> View) -> Rig {
    let mut rig = Rig {
        rt: ReactiveRuntime::init(Arc::new(|| {})),
        root: RenderRoot::new(),
        text: TextContext::new(),
        size: Size::new(width, height),
    };
    rig.rebuild(view);
    rig
}

/// Five axes of known values with one gap.
fn five_axes() -> Categories {
    Categories::from_rows([
        CategoryRow::with_gaps("Speed", [Some(100.0), Some(20.0)]),
        CategoryRow::with_gaps("Power", [Some(50.0), Some(40.0)]),
        CategoryRow::with_gaps("Range", [Some(75.0), Some(60.0)]),
        CategoryRow::with_gaps("Armour", [Some(25.0), Some(80.0)]),
        CategoryRow::with_gaps("Stealth", [None, Some(100.0)]),
    ])
}

fn axis_angle(index: usize, count: usize) -> f64 {
    -FRAC_PI_2 + TAU * index as f64 / count as f64
}

fn distance(a: Point, b: Point) -> f64 {
    (a - b).hypot()
}

fn assert_near(actual: Point, expected: Point, what: &str) {
    assert!(
        distance(actual, expected) < EPS,
        "{what}: {actual:?} != {expected:?}"
    );
}

/// The shaped size of `text` at the default title style.
fn title_size(text: &str) -> Size {
    TextContext::new()
        .layout(text, &TextStyle::new(TITLE_SIZE, GREY), None)
        .size()
}

/// The leftmost glyph origin of a run: the title's left edge.
fn left(run: &[Point]) -> f64 {
    run.iter().map(|p| p.x).fold(f64::INFINITY, f64::min)
}

#[test]
fn each_vertex_sits_on_its_axis_at_its_share_of_the_domain() {
    let _g = hold();
    let data = five_axes();
    let mut rig = rig(200.0, 200.0, || {
        radar_chart(data.clone())
            .titles(false)
            .domain(YDomain::fixed(0.0, 100.0))
            .web(GREY, 1.0)
            .dataset_styles([RadarStyle::color(RED)])
    });
    let (rec, asked) = rig.frame();
    assert!(!asked);
    let center = Point::new(100.0, 100.0);
    // The outer ring sits 2 px inside the widget's half side.
    let radius = 98.0;
    let strokes = rec.strokes_in(RED);
    assert_eq!(strokes.len(), 1);
    let (shape, width) = &strokes[0];
    assert_eq!(*width, 1.5, "the default stroke width");
    assert_eq!((shape.subpaths, shape.curved), (1, false));
    let expected = [1.0, 0.5, 0.75, 0.25, 0.0];
    assert_eq!(shape.points.len(), expected.len(), "one vertex per axis");
    for (i, (&point, share)) in shape.points.iter().zip(expected).enumerate() {
        let at = point_on_circle(center, radius * share, axis_angle(i, 5));
        assert_near(point, at, &format!("axis {i}"));
    }
    // The first axis points straight up and the gap sits at the centre.
    assert_near(shape.points[0], Point::new(100.0, 2.0), "the first axis");
    assert_near(shape.points[4], center, "the gap");
}

#[test]
fn the_default_domain_runs_from_zero_to_a_nice_top_over_every_dataset() {
    let _g = hold();
    let data =
        Categories::from_rows([("a", [40.0, 10.0]), ("b", [80.0, 20.0]), ("c", [20.0, 5.0])]);
    let mut rig = rig(200.0, 200.0, || {
        radar_chart(data.clone())
            .titles(false)
            .dataset_styles([RadarStyle::color(RED), RadarStyle::color(BLUE)])
    });
    // The largest value, 80, rounds up to a top of 100.
    let (rec, _) = rig.frame();
    let center = Point::new(100.0, 100.0);
    let red = &rec.strokes_in(RED)[0].0;
    assert_near(
        red.points[1],
        point_on_circle(center, 98.0 * 0.8, axis_angle(1, 3)),
        "80",
    );
    let blue = &rec.strokes_in(BLUE)[0].0;
    assert_near(
        blue.points[0],
        point_on_circle(center, 98.0 * 0.1, axis_angle(0, 3)),
        "10",
    );

    // A value past a fixed domain is clamped to the outer ring, one below it to the centre.
    data.set_value(1, 0, 250.0);
    data.set_value(2, 0, -30.0);
    let mut fixed = self::rig(200.0, 200.0, || {
        radar_chart(data.clone())
            .titles(false)
            .domain(YDomain::fixed(0.0, 100.0))
            .dataset_styles([RadarStyle::color(RED)])
    });
    let (rec, _) = fixed.frame();
    let red = &rec.strokes_in(RED)[0].0;
    assert_near(
        red.points[1],
        point_on_circle(center, 98.0, axis_angle(1, 3)),
        "clamped high",
    );
    assert_near(red.points[2], center, "clamped low");
}

#[test]
fn rings_are_evenly_spaced_out_to_the_outer_radius() {
    let _g = hold();
    let data = five_axes();
    let mut rig = rig(200.0, 200.0, || {
        radar_chart(data.clone())
            .titles(false)
            .rings(4)
            .web(GREY, 1.0)
    });
    let (rec, _) = rig.frame();
    let center = Point::new(100.0, 100.0);
    let web = rec.web();
    assert_eq!(web.len(), 5, "four rings, then the spokes");
    for (k, ring) in web[..4].iter().enumerate() {
        let r = 98.0 * (k + 1) as f64 / 4.0;
        assert_eq!(ring.points.len(), 5, "a pentagon");
        for (i, &point) in ring.points.iter().enumerate() {
            assert_near(point, point_on_circle(center, r, axis_angle(i, 5)), "ring");
        }
    }
    let spokes = &web[4];
    assert_eq!(spokes.subpaths, 5, "one spoke per axis");
    for (i, pair) in spokes.points.chunks(2).enumerate() {
        assert_near(pair[0], center, "a spoke starts at the centre");
        let tip = point_on_circle(center, 98.0, axis_angle(i, 5));
        assert_near(pair[1], tip, "a spoke ends on the outer ring");
    }

    // No rings leaves the spokes alone; a zero web width draws no web at all.
    let mut bare = self::rig(200.0, 200.0, || {
        radar_chart(data.clone())
            .titles(false)
            .rings(0)
            .web(GREY, 1.0)
    });
    assert_eq!(bare.frame().0.web().len(), 1);
    let mut hidden = self::rig(200.0, 200.0, || {
        radar_chart(data.clone()).titles(false).web(GREY, 0.0)
    });
    assert!(hidden.frame().0.web().is_empty());
}

#[test]
fn a_circle_web_draws_round_rings_over_the_same_datasets() {
    let _g = hold();
    let data = five_axes();
    let view = |shape: RadarShape| {
        let data = data.clone();
        move || {
            radar_chart(data.clone())
                .titles(false)
                .shape(shape)
                .rings(3)
                .web(GREY, 1.0)
                .dataset_styles([RadarStyle::color(RED)])
        }
    };
    let (polygon, _) = rig(200.0, 200.0, view(RadarShape::Polygon)).frame();
    let (circle, _) = rig(200.0, 200.0, view(RadarShape::Circle)).frame();
    let center = Point::new(100.0, 100.0);

    let rings = circle.web();
    assert_eq!(rings.len(), 4);
    for (k, ring) in rings[..3].iter().enumerate() {
        let r = 98.0 * (k + 1) as f64 / 3.0;
        assert!(ring.curved, "a circle is drawn with curves");
        assert_near(ring.points[0], Point::new(100.0, 100.0 - r), "starts at 12");
        for &point in &ring.points {
            assert!((distance(point, center) - r).abs() < EPS);
        }
    }
    assert!(polygon.web()[..3].iter().all(|ring| !ring.curved));
    // The spokes and the dataset are the same either way.
    let points = |rec: &Rec| rec.strokes_in(RED)[0].0.points.clone();
    assert_eq!(points(&polygon), points(&circle));
    assert_eq!(polygon.web()[3].points, circle.web()[3].points);
}

#[test]
fn datasets_paint_in_column_order_each_fill_before_its_stroke() {
    let _g = hold();
    let data = Categories::from_rows([
        ("a", [10.0, 20.0, 30.0, 40.0]),
        ("b", [10.0, 20.0, 30.0, 40.0]),
        ("c", [10.0, 20.0, 30.0, 40.0]),
    ]);
    let mut rig = rig(200.0, 200.0, || {
        radar_chart(data.clone())
            .titles(false)
            .web(GREY, 1.0)
            .dataset_styles([
                RadarStyle::color(RED),
                RadarStyle::color(BLUE).fill_alpha(0.5).stroke_width(2.0),
                RadarStyle::color(RED).fill_alpha(0.0),
            ])
    });
    let (rec, _) = rig.frame();
    let draws: Vec<(&str, Color, f64)> = rec
        .ops
        .iter()
        .filter_map(|op| match op {
            Op::Fill(_, color) => Some(("fill", *color, 0.0)),
            Op::Stroke(_, width, color) if *color != GREY => Some(("stroke", *color, *width)),
            _ => None,
        })
        .collect();
    // The fourth dataset has no style: the fallback colour with the default stroke and fill.
    let calm = Tone::FALLBACK_CALM;
    assert_eq!(
        draws,
        [
            ("fill", RED.with_alpha(0.25), 0.0),
            ("stroke", RED, 1.5),
            ("fill", BLUE.with_alpha(0.5), 0.0),
            ("stroke", BLUE, 2.0),
            ("stroke", RED, 1.5),
            ("fill", calm.with_alpha(0.25), 0.0),
            ("stroke", calm, 1.5),
        ]
    );
    // The web is drawn under every dataset.
    let first_dataset = rec
        .ops
        .iter()
        .position(|op| matches!(op, Op::Fill(..)))
        .unwrap();
    let last_web = rec
        .ops
        .iter()
        .rposition(|op| matches!(op, Op::Stroke(_, _, c) if *c == GREY))
        .unwrap();
    assert!(last_web < first_dataset);
    assert_eq!(rec.clips, [Rect::new(0.0, 0.0, 200.0, 200.0)]);
    // Each fill covers its own stroke's polygon.
    let fills = rec.fills();
    assert_eq!(fills[0].0.points, rec.strokes_in(RED)[0].0.points);
}

#[test]
fn titles_are_anchored_by_angle_outside_the_web() {
    let _g = hold();
    let data = Categories::from_rows([
        ("North", [50.0]),
        ("East", [50.0]),
        ("South", [50.0]),
        ("West", [50.0]),
    ]);
    let mut rig = rig(240.0, 240.0, || radar_chart(data.clone()).web(GREY, 1.0));
    let (rec, _) = rig.frame();
    let center = Point::new(120.0, 120.0);
    let [north, east, south, west] = ["North", "East", "South", "West"].map(title_size);

    // The web shrinks until every title fits: from 118 to the tightest title's room.
    let outer = &rec.web()[3];
    let radius = distance(outer.points[0], center);
    let room = [
        118.0,
        120.0 - north.height - TITLE_GAP,
        120.0 - east.width - TITLE_GAP,
        120.0 - south.height - TITLE_GAP,
        120.0 - west.width - TITLE_GAP,
    ];
    let expected = room.into_iter().fold(f64::INFINITY, f64::min);
    assert!((radius - expected).abs() < EPS, "{radius} != {expected}");
    let reach = radius + TITLE_GAP;

    let runs = rec.runs();
    assert_eq!(runs.len(), 4, "one run per title, in axis order");
    // A pixel of slack covers the snap to whole pixels.
    let near = |a: f64, b: f64| (a - b).abs() <= 1.0;
    // North: centred above the web, its bottom on the anchor.
    assert!(near(left(&runs[0]), center.x - north.width / 2.0));
    assert!(runs[0].iter().all(|p| p.y <= center.y - reach + 1.0));
    // East: left-aligned to the anchor, centred on the axis.
    assert!(near(left(&runs[1]), center.x + reach));
    assert!(runs[1].iter().all(|p| (p.y - center.y).abs() < east.height));
    // South: centred below the web, its top on the anchor.
    assert!(near(left(&runs[2]), center.x - south.width / 2.0));
    assert!(runs[2].iter().all(|p| p.y >= center.y + reach));
    // West: right-aligned to the anchor.
    assert!(near(left(&runs[3]) + west.width, center.x - reach));
    // Titles draw after the datasets.
    let last_glyphs = rec.ops.iter().rposition(|op| matches!(op, Op::Glyphs(_)));
    let last_fill = rec.ops.iter().rposition(|op| matches!(op, Op::Fill(..)));
    assert!(last_fill < last_glyphs);
}

#[test]
fn long_titles_shrink_the_web_and_stay_inside_the_widget() {
    let _g = hold();
    let names = [
        "Throughput",
        "Latency",
        "Error budget",
        "Saturation",
        "Uptime",
    ];
    let data = Categories::from_rows(names.map(|n| (n, [60.0])));
    let mut rig = rig(160.0, 160.0, || radar_chart(data.clone()).web(GREY, 1.0));
    let (rec, _) = rig.frame();
    let center = Point::new(80.0, 80.0);
    let radius = distance(rec.web()[3].points[0], center);
    assert!(radius > 0.0 && radius < 78.0 - 10.0, "{radius}");
    let runs = rec.runs();
    assert_eq!(runs.len(), names.len());
    for (run, name) in runs.iter().zip(names) {
        let size = title_size(name);
        let x = left(run);
        assert!(x >= -EPS && x + size.width <= 160.0 + 1.0, "{name} at {x}");
        assert!(run.iter().all(|p| p.y >= 0.0 && p.y <= 160.0), "{name}");
    }

    // Titles too large for the widget leave no web, and nothing panics.
    let tiny = Categories::from_rows(names.map(|n| (n, [60.0])));
    let mut cramped = self::rig(30.0, 30.0, || radar_chart(tiny.clone()).web(GREY, 1.0));
    let (rec, _) = cramped.frame();
    assert!(rec.web().is_empty());
    assert_eq!(rec.runs().len(), names.len());
}

#[test]
fn fewer_than_three_axes_draw_the_web_only() {
    let _g = hold();
    let two = Categories::from_rows([("a", [50.0]), ("b", [70.0])]);
    let mut rig = rig(200.0, 200.0, || {
        radar_chart(two.clone())
            .titles(false)
            .rings(2)
            .web(GREY, 1.0)
            .dataset_styles([RadarStyle::color(RED)])
    });
    let (rec, _) = rig.frame();
    assert!(rec.fills().is_empty());
    assert!(rec.strokes_in(RED).is_empty());
    let web = rec.web();
    assert_eq!(web.len(), 3);
    assert!(
        web[..2].iter().all(|ring| ring.curved),
        "no polygon: round rings"
    );
    assert_eq!(web[2].subpaths, 2);

    let empty = Categories::new();
    let mut none = self::rig(200.0, 200.0, || {
        radar_chart(empty.clone()).rings(2).web(GREY, 1.0)
    });
    let (rec, asked) = none.frame();
    assert!(!asked);
    assert_eq!(rec.web().len(), 2, "rings only, no spokes");
    assert!(rec.runs().is_empty());
}

#[test]
fn a_value_write_repaints_the_polygon_with_no_rebuild() {
    let _g = hold();
    let data = five_axes();
    let mut rig = rig(200.0, 200.0, || {
        radar_chart(data.clone())
            .domain(YDomain::fixed(0.0, 100.0))
            .web(GREY, 1.0)
            .dataset_styles([RadarStyle::color(RED)])
    });
    let (before, _) = rig.frame();
    let radius = distance(before.web()[3].points[0], Point::new(100.0, 100.0));
    let points = |rec: &Rec| rec.strokes_in(RED)[0].0.points.clone();

    // A repaint with no write draws the same polygon.
    let (again, asked) = rig.repaint();
    assert!(!asked);
    assert_eq!(points(&again), points(&before));

    data.set_value(4, 0, 50.0);
    let (after, asked) = rig.repaint();
    assert!(!asked, "a value write needs no layout");
    let center = Point::new(100.0, 100.0);
    let moved = point_on_circle(center, radius * 0.5, axis_angle(4, 5));
    assert_near(points(&after)[4], moved, "the written axis");
    assert_eq!(points(&after)[..4], points(&before)[..4]);
    assert_eq!(after.runs().len(), 5, "the titles still draw");
}

#[test]
fn a_label_change_requests_one_layout() {
    let _g = hold();
    let data = five_axes();
    let mut rig = rig(200.0, 200.0, || {
        radar_chart(data.clone()).dataset_styles([RadarStyle::color(RED)])
    });
    let (rec, asked) = rig.frame();
    assert!(!asked);
    let glyphs = |rec: &Rec| rec.runs().iter().map(Vec::len).collect::<Vec<_>>();
    assert_eq!(glyphs(&rec), [5, 5, 5, 6, 7]);

    // The repaint draws the titles it has and asks once for a layout to reshape them.
    data.set_label(0, "Top speed");
    let (rec, asked) = rig.repaint();
    assert!(asked);
    assert_eq!(glyphs(&rec), [5, 5, 5, 6, 7]);
    assert!(!rig.repaint().1, "asked once per change");
    data.set_value(0, 0, 10.0);
    assert!(!rig.repaint().1, "a value write does not ask again");

    let (rec, asked) = rig.frame();
    assert!(!asked);
    // "Top speed" shapes as two words.
    let first: usize = glyphs(&rec)[0];
    assert!(first >= 8, "{first}");
    assert!(!rig.repaint().1);

    // A new axis count asks too; the stale titles skip that frame.
    data.set_rows([("a", [1.0]), ("b", [2.0]), ("c", [3.0])]);
    let (rec, asked) = rig.repaint();
    assert!(asked);
    assert!(rec.runs().is_empty());
    assert_eq!(rec.strokes_in(RED)[0].0.points.len(), 3);
    assert_eq!(rig.frame().0.runs().len(), 3);
}

#[test]
fn without_titles_a_label_change_needs_no_layout() {
    let _g = hold();
    let data = five_axes();
    let mut rig = rig(200.0, 200.0, || radar_chart(data.clone()).titles(false));
    let (rec, _) = rig.frame();
    assert!(rec.runs().is_empty());
    data.set_label(0, "Top speed");
    assert!(!rig.repaint().1);
}

#[test]
fn the_chart_is_a_square_sized_to_its_box() {
    let _g = hold();
    let data = five_axes();
    let clips = |size: Option<f32>| {
        let view = || {
            let view = radar_chart(data.clone()).titles(false).web(GREY, 1.0);
            size.map_or(view.clone(), |side| view.size(side))
        };
        let rec = rig(300.0, 200.0, view).frame().0;
        let spokes = rec.web().last().unwrap().clone();
        (rec.clips[0], spokes.points[0])
    };
    // The largest square the box allows, with the web centred in it.
    assert_eq!(
        clips(None),
        (Rect::new(0.0, 0.0, 200.0, 200.0), Point::new(100.0, 100.0))
    );
    assert_eq!(
        clips(Some(120.0)),
        (Rect::new(0.0, 0.0, 120.0, 120.0), Point::new(60.0, 60.0))
    );
}

#[test]
fn spec_changes_report_the_least_work() {
    let _g = hold();
    let data = five_axes();
    let base = || radar_chart(data.clone()).web(GREY, 1.0);
    let mut rig = rig(200.0, 200.0, base);
    rig.frame();
    assert_eq!(rig.rebuild(base), ChangeFlags::NONE);

    let paint_only: [&dyn Fn() -> View; 6] = [
        &|| base().shape(RadarShape::Circle),
        &|| base().rings(2),
        &|| base().domain(YDomain::fixed(0.0, 50.0)),
        &|| base().dataset_styles([RadarStyle::color(RED)]),
        &|| base().web(BLUE, 1.0),
        &|| base().semantics("Skills"),
    ];
    for next in paint_only {
        assert_eq!(rig.rebuild(next), ChangeFlags::PAINT);
        rig.rebuild(base);
    }
    let layout: [&dyn Fn() -> View; 3] = [
        &|| radar_chart(five_axes()).web(GREY, 1.0),
        &|| base().titles(false),
        &|| base().size(100.0),
    ];
    for next in layout {
        assert_eq!(rig.rebuild(next), ChangeFlags::LAYOUT | ChangeFlags::PAINT);
        rig.rebuild(base);
    }
}

#[test]
fn input_is_ignored() {
    let _g = hold();
    let data = five_axes();
    let mut rig = rig(200.0, 200.0, || radar_chart(data.clone()));
    rig.frame();
    for phase in [PointerPhase::Down, PointerPhase::Up] {
        let event = InputEvent::Pointer(PointerEvent {
            phase,
            position: Point::new(100.0, 100.0),
            button: PointerButton::Primary,
        });
        let outcome: EventOutcome = rig.root.event(&mut (), &event);
        assert!(!outcome.handled);
    }
    assert!(!rig.root.is_pointer_captured());
}

#[test]
fn semantics_is_one_image_node_only_with_a_label() {
    let _g = hold();
    let data = five_axes();
    let images = |rig: &Rig| {
        rig.root
            .semantics()
            .nodes
            .into_iter()
            .filter(|(_, n)| n.role() == Role::Image)
            .map(|(_, n)| n.label().map(str::to_owned))
            .collect::<Vec<_>>()
    };
    let mut plain = rig(200.0, 200.0, || radar_chart(data.clone()));
    plain.frame();
    assert!(images(&plain).is_empty());
    let mut labelled = rig(200.0, 200.0, || {
        radar_chart(data.clone()).semantics("Skills")
    });
    labelled.frame();
    assert_eq!(images(&labelled), [Some("Skills".to_owned())]);
}
