//! `pie_chart` headless: a real `RenderRoot` painting into a recording scene, with pointer
//! events for the centre child.
//!
//! Writing a `Categories` list requests a process-wide repaint, so every test holds `STATICS`.

use std::any::Any;
use std::f64::consts::{FRAC_PI_2, PI, TAU};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use frust::authoring::scene::GlyphRun;
use frust::authoring::text::TextContext;
use frust::authoring::{
    BezPath, Brush, ChangeFlags, EventOutcome, InputEvent, PaintCtx, PaintScene, Point,
    PointerButton, PointerEvent, PointerPhase, Rect, Role, Shape, Size,
};
use frust::kurbo::{Affine, PathEl};
use frust::{Color, FrameTime, canvas};
use frust_charts::{Categories, CategoryRow, MeterView, PieChartView, Tone, angle_of, pie_chart};
use frust_core::RenderRoot;
use frust_reactive::ReactiveRuntime;

static STATICS: Mutex<()> = Mutex::new(());

fn hold() -> MutexGuard<'static, ()> {
    STATICS.lock().unwrap_or_else(PoisonError::into_inner)
}

const RED: Color = Color::from_rgb8(0xd0, 0x10, 0x10);
const BLUE: Color = Color::from_rgb8(0x10, 0x20, 0xd0);
const GREEN: Color = Color::from_rgb8(0x10, 0xa0, 0x30);
const SIDE: f64 = 200.0;
const CENTER: Point = Point::new(100.0, 100.0);
const EPS: f64 = 1e-6;

/// What the app saw: taps on the centre child.
#[derive(Default)]
struct Taps {
    count: usize,
}

/// The filled paths (in window coordinates) and the rects a paint issued.
#[derive(Default)]
struct Rec {
    fills: Vec<(BezPath, Color)>,
    rects: Vec<(Rect, Color)>,
    transforms: Vec<Affine>,
}

impl Rec {
    fn transform(&self) -> Affine {
        self.transforms
            .iter()
            .fold(Affine::IDENTITY, |acc, t| acc * *t)
    }
}

impl PaintScene for Rec {
    fn fill_rect(&mut self, origin: Point, size: Size, color: Color) {
        let rect = self
            .transform()
            .transform_rect_bbox(Rect::from_origin_size(origin, size));
        self.rects.push((rect, color));
    }
    fn fill_path(&mut self, origin: Point, path: &BezPath, brush: &Brush) {
        let Brush::Solid(color) = brush else {
            panic!("a pie fills with solid brushes, got {brush:?}");
        };
        let placed = self.transform() * Affine::translate(origin.to_vec2()) * path.clone();
        self.fills.push((placed, *color));
    }
    fn push_transform(&mut self, transform: Affine) {
        self.transforms.push(transform);
    }
    fn pop_transform(&mut self) {
        self.transforms.pop();
    }
    fn draw_glyph_run(&mut self, _run: GlyphRun) {}
    fn draw_text(&mut self, _origin: Point, _text: &str) {}
}

type View = PieChartView<Taps>;

struct Rig {
    rt: &'static ReactiveRuntime,
    root: RenderRoot<Taps, View>,
    text: TextContext,
    taps: Taps,
}

impl Rig {
    fn new(view: impl Fn() -> View) -> Self {
        let mut rig = Self {
            rt: ReactiveRuntime::init(Arc::new(|| {})),
            root: RenderRoot::new(),
            text: TextContext::new(),
            taps: Taps::default(),
        };
        rig.rebuild(view);
        rig
    }

    fn rebuild(&mut self, view: impl Fn() -> View) -> ChangeFlags {
        let mut logic = |_: &mut Taps| view();
        let (root, taps) = (&mut self.root, &mut self.taps);
        self.rt.with_owner(|| root.rebuild(&mut logic, taps))
    }

    fn paint_at(&mut self, width: f64, height: f64) -> Rec {
        self.root
            .layout_with_text(Size::new(width, height), &mut self.text as &mut dyn Any);
        let mut rec = Rec::default();
        self.root
            .paint(&mut rec, FrameTime::from_nanos(1_000_000_000));
        rec
    }

    fn paint(&mut self) -> Rec {
        self.paint_at(SIDE, SIDE)
    }

    fn pointer(&mut self, phase: PointerPhase, x: f64, y: f64) -> EventOutcome {
        let event = InputEvent::Pointer(PointerEvent {
            phase,
            position: Point::new(x, y),
            button: PointerButton::Primary,
        });
        self.root.event(&mut self.taps, &event)
    }

    /// A press and release at one point; whether the press was handled.
    fn tap(&mut self, x: f64, y: f64) -> bool {
        let down = self.pointer(PointerPhase::Down, x, y);
        self.pointer(PointerPhase::Up, x, y);
        down.handled
    }
}

fn rows(values: &[f32]) -> Categories {
    Categories::from_rows(
        values
            .iter()
            .enumerate()
            .map(|(i, v)| (format!("r{i}"), [*v])),
    )
}

/// Every end point of `path`.
fn vertices(path: &BezPath) -> Vec<Point> {
    path.elements()
        .iter()
        .filter_map(|el| el.end_point())
        .collect()
}

fn subpaths(path: &BezPath) -> usize {
    path.elements()
        .iter()
        .filter(|el| matches!(el, PathEl::MoveTo(_)))
        .count()
}

fn has_line(path: &BezPath) -> bool {
    path.elements()
        .iter()
        .any(|el| matches!(el, PathEl::LineTo(_)))
}

/// `angle` wrapped into `-PI..PI`.
fn wrap(angle: f64) -> f64 {
    let a = angle.rem_euclid(TAU);
    if a >= PI { a - TAU } else { a }
}

/// The angular span of `path`'s vertices on the circle of `radius` around the centre,
/// measured around `mid`.
fn span_on(path: &BezPath, radius: f64, mid: f64) -> f64 {
    let offsets: Vec<f64> = vertices(path)
        .into_iter()
        .filter(|p| (p.distance(CENTER) - radius).abs() < 1e-6)
        .map(|p| wrap(angle_of(p, CENTER) - mid))
        .collect();
    assert!(!offsets.is_empty(), "no vertex on the circle of {radius}");
    let max = offsets.iter().copied().fold(f64::MIN, f64::max);
    let min = offsets.iter().copied().fold(f64::MAX, f64::min);
    max - min
}

/// The middle angle of each section of `values`, from 12 o'clock clockwise.
fn mids(values: &[f64]) -> Vec<f64> {
    let total: f64 = values.iter().sum();
    let mut before = 0.0;
    values
        .iter()
        .map(|v| {
            let mid = -FRAC_PI_2 + TAU * (before + v / 2.0) / total;
            before += v;
            mid
        })
        .collect()
}

#[test]
fn sections_take_their_share_of_the_turn_in_row_order_from_12_oclock() {
    let _g = hold();
    let c = rows(&[1.0, 2.0, 3.0, 2.0]);
    let mut rig = Rig::new(|| pie_chart(c.clone()).colors([RED, BLUE]));
    let rec = rig.paint();
    assert_eq!(rec.fills.len(), 4);
    let values = [1.0, 2.0, 3.0, 2.0];
    let mut sum = 0.0;
    for (((path, _), mid), value) in rec.fills.iter().zip(mids(&values)).zip(values) {
        let span = span_on(path, 100.0, mid);
        assert!((span - TAU * value / 8.0).abs() < 1e-9, "{span}");
        sum += span;
    }
    assert!((sum - TAU).abs() < 1e-9, "{sum}");
    // A pie wedge reaches the centre, and the first one starts at 12 o'clock.
    assert!(vertices(&rec.fills[0].0).contains(&CENTER));
    let first = vertices(&rec.fills[0].0)[1];
    assert!(first.distance(Point::new(100.0, 0.0)) < EPS, "{first:?}");
    // The first section (an eighth) lies right of 12 o'clock.
    assert!(vertices(&rec.fills[0].0).iter().all(|p| p.x >= 100.0 - EPS));
    let colors: Vec<Color> = rec.fills.iter().map(|(_, c)| *c).collect();
    assert_eq!(colors, [RED, BLUE, RED, BLUE]);
}

#[test]
fn sweeps_sum_to_a_full_turn_minus_the_gaps() {
    let _g = hold();
    let values = [1.0, 2.0, 3.0, 4.0];
    let c = rows(&[1.0, 2.0, 3.0, 4.0]);
    let mut rig = Rig::new(|| pie_chart(c.clone()).donut(0.5).gap(6.0));
    let rec = rig.paint();
    assert_eq!(rec.fills.len(), 4);
    let rim_gap = 2.0 * (3.0f64 / 100.0).asin();
    let hole_gap = 2.0 * (3.0f64 / 50.0).asin();
    let mut outer_sum = 0.0;
    for (((path, _), mid), value) in rec.fills.iter().zip(mids(&values)).zip(values) {
        let outer = span_on(path, 100.0, mid);
        let inner = span_on(path, 50.0, mid);
        assert!((outer - (TAU * value / 10.0 - rim_gap)).abs() < 1e-9);
        assert!((inner - (TAU * value / 10.0 - hole_gap)).abs() < 1e-9);
        outer_sum += outer;
    }
    assert!((outer_sum - (TAU - 4.0 * rim_gap)).abs() < 1e-9);
}

#[test]
fn a_gap_is_as_wide_at_the_hole_as_at_the_rim() {
    let _g = hold();
    for corner in [0.0, 8.0] {
        let c = rows(&[1.0, 1.0, 1.0, 1.0]);
        let mut rig = Rig::new(|| {
            pie_chart(c.clone())
                .donut(0.5)
                .gap(10.0)
                .corner_radius(corner)
        });
        let rec = rig.paint();
        assert_eq!(rec.fills.len(), 4);
        // Quadrants clockwise from 12 o'clock: x/y signs of each section from the centre.
        let quadrants = [(1.0, -1.0), (1.0, 1.0), (-1.0, 1.0), (-1.0, -1.0)];
        for ((path, _), (sx, sy)) in rec.fills.iter().zip(quadrants) {
            let points = vertices(path);
            for p in &points {
                assert!((p.x - 100.0) * sx >= 5.0 - EPS, "{corner}: {p:?}");
                assert!((p.y - 100.0) * sy >= 5.0 - EPS, "{corner}: {p:?}");
                assert!(p.distance(CENTER) <= 100.0 + EPS);
                assert!(p.distance(CENTER) >= 50.0 - EPS);
            }
            if corner == 0.0 {
                // The edges run exactly 5 px from each boundary, at the rim and at the hole.
                for radius in [100.0, 50.0] {
                    let on: Vec<&Point> = points
                        .iter()
                        .filter(|p| (p.distance(CENTER) - radius).abs() < 1e-6)
                        .collect();
                    let near_x = on
                        .iter()
                        .map(|p| (p.x - 100.0) * sx)
                        .fold(f64::MAX, f64::min);
                    let near_y = on
                        .iter()
                        .map(|p| (p.y - 100.0) * sy)
                        .fold(f64::MAX, f64::min);
                    assert!((near_x - 5.0).abs() < 1e-9, "{radius}: {near_x}");
                    assert!((near_y - 5.0).abs() < 1e-9, "{radius}: {near_y}");
                }
            }
        }
    }
}

#[test]
fn a_gapped_pie_section_ends_in_a_point_off_the_centre() {
    let _g = hold();
    let c = rows(&[1.0, 1.0, 1.0, 1.0]);
    let mut rig = Rig::new(|| pie_chart(c.clone()).gap(10.0));
    let rec = rig.paint();
    assert_eq!(rec.fills.len(), 4);
    let (path, _) = &rec.fills[0];
    // The offset edges of a quarter meet at (5, -5) from the centre.
    assert!(vertices(path).contains(&Point::new(105.0, 95.0)));
    assert!(
        vertices(path)
            .iter()
            .all(|p| p.x >= 105.0 - EPS && p.y <= 95.0 + EPS)
    );
}

#[test]
fn a_donut_leaves_a_hole_of_its_fraction() {
    let _g = hold();
    let c = rows(&[1.0, 3.0]);
    let mut rig = Rig::new(|| pie_chart(c.clone()).donut(0.25));
    let rec = rig.paint();
    assert_eq!(rec.fills.len(), 2);
    for (path, _) in &rec.fills {
        let nearest = vertices(path)
            .iter()
            .map(|p| p.distance(CENTER))
            .fold(f64::MAX, f64::min);
        assert!((nearest - 25.0).abs() < EPS, "{nearest}");
    }
}

#[test]
fn a_single_positive_value_draws_a_whole_ring_without_a_seam() {
    let _g = hold();
    let c = Categories::from_rows([
        CategoryRow::with_gaps("gap", [None]),
        CategoryRow::new("zero", [0.0]),
        CategoryRow::new("only", [5.0]),
        CategoryRow::new("negative", [-3.0]),
        CategoryRow::new("nan", [f32::NAN]),
        CategoryRow::with_gaps("empty", []),
    ]);
    for donut in [0.0, 0.5] {
        let mut rig = Rig::new(|| {
            pie_chart(c.clone())
                .donut(donut)
                .gap(8.0)
                .colors([RED, BLUE, GREEN])
        });
        let rec = rig.paint();
        assert_eq!(rec.fills.len(), 1, "{donut}");
        let (path, color) = &rec.fills[0];
        assert_eq!(*color, GREEN, "the colour of row 2");
        assert!(!has_line(path), "no seam");
        assert_eq!(subpaths(path), if donut > 0.0 { 2 } else { 1 });
        let bounds = path.bounding_box();
        assert!((bounds.width() - 200.0).abs() < EPS && (bounds.height() - 200.0).abs() < EPS);
    }
}

#[test]
fn no_positive_value_draws_an_empty_track_ring() {
    let _g = hold();
    let zero = rows(&[0.0, -1.0, 0.0]);
    let mut rig = Rig::new(|| pie_chart(zero.clone()).donut(0.6));
    let rec = rig.paint();
    assert_eq!(rec.fills.len(), 1);
    let (path, color) = &rec.fills[0];
    assert_eq!(*color, MeterView::FALLBACK_TRACK);
    assert!(!has_line(path));
    assert_eq!(subpaths(path), 2);

    let empty = Categories::new();
    let mut rig = Rig::new(|| pie_chart(empty.clone()).track(BLUE));
    let rec = rig.paint();
    assert_eq!(rec.fills.len(), 1);
    assert_eq!(rec.fills[0].1, BLUE);
}

#[test]
fn colours_default_to_the_fallback_palette_by_row_index() {
    let _g = hold();
    let c = rows(&[1.0, 0.0, 1.0]);
    let mut rig = Rig::new(|| pie_chart(c.clone()));
    let rec = rig.paint();
    assert_eq!(rec.fills.len(), 2);
    assert_eq!(rec.fills[0].1, Tone::FALLBACK_CALM);
    // Row 2 keeps its own colour though row 1 draws nothing.
    assert_eq!(rec.fills[1].1, Tone::FALLBACK_BUSY);
}

#[test]
fn counter_clockwise_sections_run_left_from_the_start() {
    let _g = hold();
    let c = rows(&[1.0, 3.0]);
    let mut rig = Rig::new(|| pie_chart(c.clone()).clockwise(false));
    let rec = rig.paint();
    assert!(
        vertices(&rec.fills[0].0)
            .iter()
            .all(|p| p.x <= 100.0 + EPS && p.y <= 100.0 + EPS)
    );
    let mut rig = Rig::new(|| pie_chart(c.clone()).start_angle(0.0));
    let rec = rig.paint();
    // A quarter from 3 o'clock clockwise lies in the bottom-right quadrant.
    assert!(
        vertices(&rec.fills[0].0)
            .iter()
            .all(|p| p.x >= 100.0 - EPS && p.y >= 100.0 - EPS)
    );
}

#[test]
fn a_write_repaints_new_sections_with_no_rebuild() {
    let _g = hold();
    let c = rows(&[1.0, 1.0]);
    let mut rig = Rig::new(|| pie_chart(c.clone()).colors([RED, BLUE]));
    let before = rig.paint();
    assert_eq!(before.fills.len(), 2);
    let mid = mids(&[1.0, 1.0])[0];
    assert!((span_on(&before.fills[0].0, 100.0, mid) - PI).abs() < 1e-9);

    c.set_value(0, 0, 3.0);
    let after = rig.paint();
    let mid = mids(&[3.0, 1.0])[0];
    assert!((span_on(&after.fills[0].0, 100.0, mid) - 1.5 * PI).abs() < 1e-9);

    c.set_value(1, 0, 0.0);
    let single = rig.paint();
    assert_eq!(single.fills.len(), 1);
    assert_eq!(single.fills[0].1, RED);
    assert!(!has_line(&single.fills[0].0));

    // An unchanged list paints the same sections again.
    let again = rig.paint();
    assert_eq!(again.fills.len(), 1);
    assert_eq!(vertices(&again.fills[0].0), vertices(&single.fills[0].0));
}

#[test]
fn everything_stays_within_bounds() {
    let _g = hold();
    let c = rows(&[0.2, 5.0, 1.0, 9.0, 0.01, 3.0]);
    let sizes = [(200.0, 200.0), (300.0, 120.0), (90.0, 400.0), (7.0, 7.0)];
    for (width, height) in sizes {
        for (donut, gap, corner) in [(0.0, 0.0, 0.0), (0.4, 4.0, 6.0), (0.8, 12.0, 30.0)] {
            let mut rig = Rig::new(|| {
                pie_chart(c.clone())
                    .donut(donut)
                    .gap(gap)
                    .corner_radius(corner)
            });
            let rec = rig.paint_at(width, height);
            let side = width.min(height);
            // Arcs are cubic approximations that stray up to about 0.03 % of the radius.
            let slack = side * 0.0003;
            for (path, _) in &rec.fills {
                let b = path.bounding_box();
                assert!(
                    b.x0 >= -slack
                        && b.y0 >= -slack
                        && b.x1 <= side + slack
                        && b.y1 <= side + slack,
                    "{width}x{height} {donut} {gap} {corner}: {b:?}"
                );
            }
        }
    }
}

#[test]
fn size_fixes_the_diameter() {
    let _g = hold();
    let c = rows(&[1.0, 1.0]);
    let mut rig = Rig::new(|| pie_chart(c.clone()).size(80.0));
    let rec = rig.paint();
    let b = rec.fills[0]
        .0
        .bounding_box()
        .union(rec.fills[1].0.bounding_box());
    assert!(
        (b.width() - 80.0).abs() < EPS && (b.height() - 80.0).abs() < EPS,
        "{b:?}"
    );
}

fn tappable_box(size: Option<Size>) -> frust::CanvasView<Taps> {
    let canvas = canvas(|scene: &mut dyn PaintScene, size: Size, _ctx: &PaintCtx| {
        scene.fill_rect(Point::ZERO, size, GREEN);
    })
    .on_tap(|taps: &mut Taps| taps.count += 1);
    match size {
        Some(size) => canvas.size(size),
        None => canvas,
    }
}

#[test]
fn the_centre_child_sits_in_the_hole_and_takes_a_tap() {
    let _g = hold();
    let c = rows(&[1.0, 2.0]);
    let mut rig = Rig::new(|| {
        pie_chart(c.clone())
            .donut(0.6)
            .center(tappable_box(Some(Size::new(40.0, 40.0))))
    });
    let rec = rig.paint();
    assert_eq!(rec.fills.len(), 2);
    assert_eq!(rec.rects, [(Rect::new(80.0, 80.0, 120.0, 120.0), GREEN)]);

    assert!(rig.tap(100.0, 100.0));
    assert_eq!(rig.taps.count, 1);
    // On the ring, and in the hole but outside the child: tap-transparent.
    assert!(!rig.tap(10.0, 100.0));
    assert!(!rig.tap(100.0, 55.0));
    assert_eq!(rig.taps.count, 1);
}

#[test]
fn an_expanding_centre_child_is_held_to_the_square_inside_the_hole() {
    let _g = hold();
    let c = rows(&[1.0]);
    let mut rig = Rig::new(|| pie_chart(c.clone()).donut(0.5).center(tappable_box(None)));
    let rec = rig.paint();
    let [(rect, _)] = rec.rects[..] else {
        panic!("one child rect, got {:?}", rec.rects);
    };
    let side = 50.0 * std::f64::consts::SQRT_2;
    assert!((rect.width() - side).abs() < EPS, "{rect:?}");
    for corner in [
        Point::new(rect.x0, rect.y0),
        Point::new(rect.x1, rect.y0),
        Point::new(rect.x0, rect.y1),
        Point::new(rect.x1, rect.y1),
    ] {
        // Within the hole, give or take the child's whole-pixel origin.
        assert!(corner.distance(CENTER) <= 50.0 + 0.75, "{corner:?}");
    }
}

#[test]
fn the_centre_child_comes_and_goes_with_a_rebuild() {
    let _g = hold();
    let c = rows(&[1.0, 1.0]);
    let base = || pie_chart(c.clone()).donut(0.5);
    let with_child = || base().center(tappable_box(Some(Size::new(20.0, 20.0))));
    let mut rig = Rig::new(base);
    assert!(rig.paint().rects.is_empty());
    let flags = rig.rebuild(with_child);
    assert!(flags.contains(ChangeFlags::LAYOUT));
    assert_eq!(rig.paint().rects.len(), 1);
    assert!(rig.tap(100.0, 100.0));
    rig.rebuild(base);
    assert!(rig.paint().rects.is_empty());
    assert!(!rig.tap(100.0, 100.0));
    assert_eq!(rig.taps.count, 1);
}

#[test]
fn without_a_centre_child_input_is_ignored() {
    let _g = hold();
    let c = rows(&[1.0, 1.0]);
    let mut rig = Rig::new(|| pie_chart(c.clone()).donut(0.5));
    rig.paint();
    assert!(!rig.tap(100.0, 100.0));
    assert!(!rig.tap(100.0, 10.0));
}

#[test]
fn a_rebuild_reports_layout_only_for_size_and_hole() {
    let _g = hold();
    let c = rows(&[1.0, 1.0]);
    let base = || pie_chart(c.clone());
    let mut rig = Rig::new(base);
    rig.paint();
    assert_eq!(rig.rebuild(base), ChangeFlags::NONE);

    let paint_only: [&dyn Fn() -> View; 6] = [
        &|| base().gap(2.0),
        &|| base().corner_radius(3.0),
        &|| base().start_angle(1.0),
        &|| base().clockwise(false),
        &|| base().colors([RED]),
        &|| base().semantics("Share"),
    ];
    for next in paint_only {
        assert_eq!(rig.rebuild(next), ChangeFlags::PAINT);
        rig.rebuild(base);
    }
    let layout: [&dyn Fn() -> View; 2] = [&|| base().donut(0.5), &|| base().size(50.0)];
    for next in layout {
        assert_eq!(rig.rebuild(next), ChangeFlags::LAYOUT | ChangeFlags::PAINT);
        rig.rebuild(base);
    }
    let other = rows(&[1.0, 1.0]);
    assert_eq!(rig.rebuild(|| pie_chart(other.clone())), ChangeFlags::PAINT);
}

#[test]
fn semantics_is_an_image_node_only_when_labelled() {
    let _g = hold();
    let c = rows(&[1.0, 1.0]);
    let images = |rig: &Rig| {
        rig.root
            .semantics()
            .nodes
            .into_iter()
            .filter(|(_, n)| n.role() == Role::Image)
            .map(|(_, n)| n.label().map(str::to_owned))
            .collect::<Vec<_>>()
    };
    let mut plain = Rig::new(|| pie_chart(c.clone()));
    plain.paint();
    assert!(images(&plain).is_empty());

    let mut labelled = Rig::new(|| pie_chart(c.clone()).semantics("Market share"));
    labelled.paint();
    assert_eq!(images(&labelled), [Some("Market share".to_owned())]);

    let mut centred = Rig::new(|| {
        pie_chart(c.clone())
            .donut(0.5)
            .semantics("Market share")
            .center(tappable_box(Some(Size::new(20.0, 20.0))))
    });
    centred.paint();
    assert_eq!(images(&centred), [Some("Market share".to_owned())]);
}
