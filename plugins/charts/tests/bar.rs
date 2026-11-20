//! `bar_chart` headless: a real `RenderRoot` painting into a recording scene.
//!
//! Writing a `Categories` list requests a process-wide repaint, so every test holds `STATICS`.

use std::any::Any;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use frust::authoring::scene::GlyphRun;
use frust::authoring::text::TextContext;
use frust::authoring::{
    ChangeFlags, CornerRadii, EventOutcome, InputEvent, PaintScene, Point, PointerButton,
    PointerEvent, PointerPhase, Rect, Role, Size,
};
use frust::{Color, FrameTime, Theme};
use frust_charts::{
    AxisSide, BarChartView, BarMode, Categories, CategoryRow, Tone, YAxis, YDomain, bar_chart,
    ticks,
};
use frust_core::RenderRoot;
use frust_reactive::ReactiveRuntime;

static STATICS: Mutex<()> = Mutex::new(());

fn hold() -> MutexGuard<'static, ()> {
    STATICS.lock().unwrap_or_else(PoisonError::into_inner)
}

const RED: Color = Color::from_rgb8(0xd0, 0x10, 0x10);
const BLUE: Color = Color::from_rgb8(0x10, 0x20, 0xd0);
const GREEN: Color = Color::from_rgb8(0x10, 0xa0, 0x30);
const GREY: Color = Color::from_rgb8(0x80, 0x80, 0x80);
const EPS: f64 = 1e-6;

/// One recorded draw.
#[derive(Clone, Debug)]
enum Op {
    Rect(Rect, Color),
    Rounded(Rect, CornerRadii, Color),
    /// A glyph run's glyph origins, in scene coordinates.
    Glyphs(Vec<Point>),
}

#[derive(Default)]
struct Rec {
    ops: Vec<(Op, usize)>,
    clips: Vec<Rect>,
    depth: usize,
}

impl Rec {
    /// Every filled bar or rule, square or rounded, with its colour.
    fn fills(&self) -> Vec<(Rect, Color)> {
        self.ops
            .iter()
            .filter_map(|(op, _)| match op {
                Op::Rect(r, c) | Op::Rounded(r, _, c) => Some((*r, *c)),
                Op::Glyphs(_) => None,
            })
            .collect()
    }

    /// The fills in `color`.
    fn in_color(&self, color: Color) -> Vec<Rect> {
        self.fills()
            .into_iter()
            .filter(|(_, c)| *c == color)
            .map(|(r, _)| r)
            .collect()
    }

    /// The rounded fills and their radii.
    fn rounded(&self) -> Vec<(Rect, CornerRadii)> {
        self.ops
            .iter()
            .filter_map(|(op, _)| match op {
                Op::Rounded(r, radii, _) => Some((*r, *radii)),
                _ => None,
            })
            .collect()
    }

    fn runs(&self) -> Vec<Vec<Point>> {
        self.ops
            .iter()
            .filter_map(|(op, _)| match op {
                Op::Glyphs(points) => Some(points.clone()),
                _ => None,
            })
            .collect()
    }
}

impl PaintScene for Rec {
    fn fill_rect(&mut self, origin: Point, size: Size, color: Color) {
        let op = Op::Rect(Rect::from_origin_size(origin, size), color);
        self.ops.push((op, self.depth));
    }
    fn fill_rounded_rect(&mut self, origin: Point, size: Size, radius: f64, color: Color) {
        self.fill_rounded_rect_radii(origin, size, CornerRadii::uniform(radius), color);
    }
    fn fill_rounded_rect_radii(
        &mut self,
        origin: Point,
        size: Size,
        radii: CornerRadii,
        color: Color,
    ) {
        let op = Op::Rounded(Rect::from_origin_size(origin, size), radii, color);
        self.ops.push((op, self.depth));
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
        self.ops.push((Op::Glyphs(points), self.depth));
    }
    fn draw_text(&mut self, _origin: Point, _text: &str) {}
}

struct Rig {
    rt: &'static ReactiveRuntime,
    root: RenderRoot<(), BarChartView>,
    text: TextContext,
    size: Size,
}

impl Rig {
    fn new(width: f64, height: f64) -> Self {
        Self {
            rt: ReactiveRuntime::init(Arc::new(|| {})),
            root: RenderRoot::new(),
            text: TextContext::new(),
            size: Size::new(width, height),
        }
    }

    fn rebuild(&mut self, view: impl Fn() -> BarChartView) -> ChangeFlags {
        let mut logic = |_: &mut ()| view();
        self.rt
            .with_owner(|| self.root.rebuild(&mut logic, &mut ()))
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
        self.root
            .layout_with_text(self.size, &mut self.text as &mut dyn Any);
        self.repaint()
    }

    fn paint(&mut self) -> Rec {
        self.frame().0
    }
}

fn rig(width: f64, height: f64, view: impl Fn() -> BarChartView) -> Rig {
    let mut rig = Rig::new(width, height);
    rig.rebuild(view);
    rig
}

fn rows(values: &[&[f32]]) -> Categories {
    Categories::from_rows(
        values
            .iter()
            .enumerate()
            .map(|(i, v)| CategoryRow::new(format!("c{i}"), v.iter().copied())),
    )
}

/// A bare chart (no labels) with a fixed domain, colouring columns red, blue, green.
fn bare(categories: &Categories, domain: YDomain) -> BarChartView {
    bar_chart(categories.clone())
        .category_labels(false)
        .domain(domain)
        .colors([RED, BLUE, GREEN])
}

fn whole(r: Rect) -> bool {
    [r.x0, r.y0, r.x1, r.y1].iter().all(|v| v.fract() == 0.0)
}

fn inside(r: Rect, bounds: Rect) -> bool {
    r.x0 >= bounds.x0 - EPS
        && r.y0 >= bounds.y0 - EPS
        && r.x1 <= bounds.x1 + EPS
        && r.y1 <= bounds.y1 + EPS
}

/// Every draw is issued under one clip equal to the widget bounds, every fill lies inside
/// the bounds on whole pixels, and every glyph origin lies inside the bounds.
fn assert_contained(rec: &Rec, size: Size) {
    let bounds = Rect::from_origin_size(Point::ZERO, size);
    assert_eq!(rec.clips, [bounds]);
    for (op, depth) in &rec.ops {
        assert_eq!(*depth, 1, "{op:?} drawn outside the clip");
    }
    for (r, _) in rec.fills() {
        assert!(inside(r, bounds), "{r:?} outside {bounds:?}");
        assert!(whole(r), "{r:?} off the pixel grid");
    }
    for p in rec.runs().into_iter().flatten() {
        assert!(inside(Rect::from_points(p, p), bounds), "glyph at {p:?}");
    }
}

#[test]
fn grouped_bars_stand_side_by_side_on_whole_pixels() {
    let _g = hold();
    // Three slots of 114 px; two bars each: (114 - 8 - 2) / 2 = 52 px, a 106 px group
    // centred 4 px into its slot.
    let data = rows(&[&[50.0, 25.0], &[100.0, 0.0], &[10.0, 75.0]]);
    let mut rig = rig(342.0, 160.0, || bare(&data, YDomain::fixed(0.0, 100.0)));
    let rec = rig.paint();
    assert_eq!(
        rec.in_color(RED),
        [
            Rect::new(4.0, 80.0, 56.0, 160.0),
            Rect::new(118.0, 0.0, 170.0, 160.0),
            Rect::new(232.0, 144.0, 284.0, 160.0),
        ]
    );
    // A zero draws nothing.
    assert_eq!(
        rec.in_color(BLUE),
        [
            Rect::new(58.0, 120.0, 110.0, 160.0),
            Rect::new(286.0, 40.0, 338.0, 160.0),
        ]
    );
    assert_contained(&rec, Size::new(342.0, 160.0));
}

#[test]
fn a_gap_draws_nothing_and_short_rows_leave_their_slots_empty() {
    let _g = hold();
    let data = Categories::from_rows([
        CategoryRow::with_gaps("a", [Some(40.0), None, Some(20.0)]),
        CategoryRow::new("b", [10.0]),
    ]);
    let mut rig = rig(342.0, 160.0, || bare(&data, YDomain::fixed(0.0, 100.0)));
    let rec = rig.paint();
    assert_eq!(rec.in_color(RED).len(), 2);
    assert!(rec.in_color(BLUE).is_empty());
    assert_eq!(rec.in_color(GREEN).len(), 1);
    // Both groups are three bars wide, the second holding only its first.
    let (first, second) = (rec.in_color(RED)[0], rec.in_color(RED)[1]);
    assert!(
        (second.x0 - first.x0 - 171.0).abs() <= 1.0,
        "{first:?} {second:?}"
    );
}

#[test]
fn stacked_bars_pile_up_to_the_row_total() {
    let _g = hold();
    let data = rows(&[&[30.0, 20.0, 10.0], &[60.0, 40.0, 0.0]]);
    let mut rig = rig(342.0, 160.0, || {
        bare(&data, YDomain::fixed(0.0, 100.0)).mode(BarMode::Stacked)
    });
    let rec = rig.paint();
    // One bar per category: (171 - 8) wide, 4 px into its slot; segments abut.
    let (r, b, g) = (rec.in_color(RED), rec.in_color(BLUE), rec.in_color(GREEN));
    assert_eq!(
        r,
        [
            Rect::new(4.0, 112.0, 167.0, 160.0),
            Rect::new(175.0, 64.0, 338.0, 160.0),
        ]
    );
    assert_eq!(
        b,
        [
            Rect::new(4.0, 80.0, 167.0, 112.0),
            Rect::new(175.0, 0.0, 338.0, 64.0),
        ]
    );
    assert_eq!(g, [Rect::new(4.0, 64.0, 167.0, 80.0)]);
}

#[test]
fn a_stacked_domain_covers_the_stack_totals() {
    let _g = hold();
    let data = rows(&[&[60.0, 50.0], &[20.0, 10.0]]);
    let mut grouped = rig(342.0, 160.0, || {
        bar_chart(data.clone()).category_labels(false)
    });
    let mut stacked = rig(342.0, 160.0, || {
        bar_chart(data.clone())
            .category_labels(false)
            .mode(BarMode::Stacked)
    });
    let auto = |hi: f32| YDomain::auto().resolve_range(Some((0.0, hi)));
    let top_of = |rec: &Rec| {
        rec.fills()
            .iter()
            .map(|(r, _)| r.y0)
            .fold(f64::INFINITY, f64::min)
    };
    let y = |value: f32, (min, max): (f32, f32)| {
        (160.0 - f64::from((value - min) / (max - min)) * 160.0).round()
    };
    assert_eq!(top_of(&grouped.paint()), y(60.0, auto(60.0)));
    assert_eq!(top_of(&stacked.paint()), y(110.0, auto(110.0)));
    assert_eq!(auto(110.0), (0.0, 150.0));
}

#[test]
fn negative_values_grow_down_from_the_zero_line() {
    let _g = hold();
    let data = rows(&[&[25.0, -25.0]]);
    let mut rig = rig(342.0, 160.0, || bare(&data, YDomain::fixed(-50.0, 50.0)));
    let rec = rig.paint();
    assert_eq!(rec.in_color(RED)[0].y0, 40.0);
    assert_eq!(rec.in_color(RED)[0].y1, 80.0);
    assert_eq!(rec.in_color(BLUE)[0].y0, 80.0);
    assert_eq!(rec.in_color(BLUE)[0].y1, 120.0);

    // Stacked, the negative parts stack below the zero line on their own.
    let mixed = rows(&[&[20.0, -10.0, 10.0, -15.0]]);
    let mut stacked = Rig::new(342.0, 160.0);
    stacked.rebuild(|| {
        bar_chart(mixed.clone())
            .category_labels(false)
            .domain(YDomain::fixed(-50.0, 50.0))
            .mode(BarMode::Stacked)
            .colors([RED, BLUE, GREEN, GREY])
    });
    let rec = stacked.paint();
    let spans: Vec<(f64, f64)> = rec.fills().iter().map(|(r, _)| (r.y0, r.y1)).collect();
    assert_eq!(
        spans,
        [(48.0, 80.0), (80.0, 96.0), (32.0, 48.0), (96.0, 120.0)]
    );
}

#[test]
fn the_default_domain_always_contains_zero() {
    let _g = hold();
    for (values, bottom_or_top) in [(&[5.0f32, 10.0][..], 160.0), (&[-5.0, -10.0][..], 0.0)] {
        let data = rows(&[values]);
        let mut rig = rig(342.0, 160.0, || {
            bar_chart(data.clone())
                .category_labels(false)
                .colors([RED, BLUE])
        });
        let rec = rig.paint();
        for (r, _) in rec.fills() {
            assert!(r.y0 == bottom_or_top || r.y1 == bottom_or_top, "{r:?}");
        }
        // The larger magnitude reaches the far edge: 0..10 and -10..0 are already nice.
        let blue = rec.in_color(BLUE)[0];
        assert_eq!(blue.height(), 160.0, "{blue:?}");
    }
}

#[test]
fn a_tiny_value_still_draws_one_pixel() {
    let _g = hold();
    let data = rows(&[&[0.01, -0.01]]);
    let mut rig = rig(342.0, 160.0, || bare(&data, YDomain::fixed(-100.0, 100.0)));
    let rec = rig.paint();
    assert_eq!(rec.in_color(RED)[0].height(), 1.0);
    assert_eq!(rec.in_color(BLUE)[0].height(), 1.0);
    assert_eq!(rec.in_color(RED)[0].y1, rec.in_color(BLUE)[0].y0);
}

#[test]
fn only_the_outer_end_is_rounded() {
    let _g = hold();
    let data = rows(&[&[50.0, -50.0]]);
    let mut grouped = rig(342.0, 160.0, || {
        bare(&data, YDomain::fixed(-100.0, 100.0)).radius(4.0)
    });
    let rec = grouped.paint();
    let radii: Vec<CornerRadii> = rec.rounded().iter().map(|(_, r)| *r).collect();
    assert_eq!(
        radii,
        [
            CornerRadii::new(4.0, 4.0, 0.0, 0.0),
            CornerRadii::new(0.0, 0.0, 4.0, 4.0),
        ]
    );

    // A stack rounds only its last segment, at its outer end.
    let stack = rows(&[&[20.0, 30.0, -10.0, -5.0]]);
    let mut stacked = Rig::new(342.0, 160.0);
    stacked.rebuild(|| {
        bar_chart(stack.clone())
            .category_labels(false)
            .domain(YDomain::fixed(-100.0, 100.0))
            .mode(BarMode::Stacked)
            .colors([RED, BLUE, GREEN, GREY])
            .radius(4.0)
    });
    let rec = stacked.paint();
    assert_eq!(rec.fills().len(), 4);
    let rounded = rec.rounded();
    assert_eq!(rounded.len(), 2);
    assert_eq!(rounded[0].0, rec.in_color(BLUE)[0]);
    assert_eq!(rounded[0].1, CornerRadii::new(4.0, 4.0, 0.0, 0.0));
    assert_eq!(rounded[1].0, rec.in_color(GREY)[0]);
    assert_eq!(rounded[1].1, CornerRadii::new(0.0, 0.0, 4.0, 4.0));

    // A radius never exceeds half the thickness or the length.
    let thin = rows(&[&[1.0]]);
    let mut short = Rig::new(342.0, 160.0);
    short.rebuild(|| {
        bare(&thin, YDomain::fixed(0.0, 100.0))
            .bar_width(6.0)
            .radius(10.0)
    });
    let (rect, radii) = short.paint().rounded()[0];
    assert_eq!(rect.size(), Size::new(6.0, 2.0));
    assert_eq!(radii, CornerRadii::new(2.0, 2.0, 0.0, 0.0));

    // Without a radius every bar is square.
    let mut square = rig(342.0, 160.0, || bare(&data, YDomain::fixed(-100.0, 100.0)));
    assert!(square.paint().rounded().is_empty());
}

#[test]
fn horizontal_mirrors_vertical() {
    let _g = hold();
    let data = rows(&[&[50.0, 25.0], &[100.0, -30.0], &[10.0, 75.0]]);
    let view = || bare(&data, YDomain::fixed(-40.0, 100.0)).radius(3.0);
    let mut vertical = rig(150.0, 150.0, view);
    let mut horizontal = rig(150.0, 150.0, || view().horizontal());
    let v = vertical.paint();
    let h = horizontal.paint();
    let flip = |r: Rect| Rect::new(150.0 - r.y1, r.x0, 150.0 - r.y0, r.x1);
    let flipped: Vec<(Rect, Color)> = v.fills().iter().map(|(r, c)| (flip(*r), *c)).collect();
    assert_eq!(h.fills(), flipped);
    let radii = |rec: &Rec| -> Vec<CornerRadii> { rec.rounded().iter().map(|(_, r)| *r).collect() };
    // Up becomes right and down becomes left.
    let turn =
        |r: CornerRadii| CornerRadii::new(r.bottom_left, r.top_left, r.top_right, r.bottom_right);
    let turned: Vec<CornerRadii> = radii(&v).into_iter().map(turn).collect();
    assert_eq!(radii(&h), turned);
    assert_contained(&h, Size::new(150.0, 150.0));
}

#[test]
fn bars_shrink_to_fit_down_to_one_pixel() {
    let _g = hold();
    let data = rows(&[&[10.0, 20.0], &[30.0, 40.0]]);
    let mut wide = rig(342.0, 160.0, || {
        bare(&data, YDomain::fixed(0.0, 100.0)).bar_width(20.0)
    });
    let rec = wide.paint();
    assert!(rec.fills().iter().all(|(r, _)| r.width() == 20.0));

    let mut narrow = rig(60.0, 160.0, || {
        bare(&data, YDomain::fixed(0.0, 100.0)).bar_width(20.0)
    });
    // Slots of 30: (30 - 8 - 2) / 2 = 10 px.
    assert!(
        narrow
            .paint()
            .fills()
            .iter()
            .all(|(r, _)| r.width() == 10.0)
    );

    let many = Categories::from_rows((0..100).map(|i| CategoryRow::new(i.to_string(), [5.0, 9.0])));
    let mut crowded = rig(120.0, 160.0, || bare(&many, YDomain::fixed(0.0, 10.0)));
    let rec = crowded.paint();
    assert_eq!(rec.fills().len(), 200);
    assert!(
        rec.fills()
            .iter()
            .all(|(r, _)| r.width() == 1.0 && whole(*r))
    );
}

#[test]
fn a_write_repaints_the_bars_with_no_rebuild() {
    let _g = hold();
    let data = rows(&[&[50.0], &[20.0]]);
    let mut rig = rig(342.0, 160.0, || bare(&data, YDomain::fixed(0.0, 100.0)));
    let rec = rig.paint();
    assert_eq!(rec.in_color(RED)[0].y0, 80.0);

    data.set_value(0, 0, 25.0);
    let (rec, needs_layout) = rig.repaint();
    assert!(!needs_layout, "a value write needs no layout");
    assert_eq!(rec.in_color(RED)[0].y0, 120.0);

    data.set_gap(1, 0);
    let (rec, _) = rig.repaint();
    assert_eq!(rec.in_color(RED).len(), 1);

    data.set_rows([
        ("a", [10.0, 10.0]),
        ("b", [10.0, 10.0]),
        ("c", [10.0, 10.0]),
    ]);
    let (rec, _) = rig.repaint();
    assert_eq!(rec.in_color(RED).len(), 3);
    assert_eq!(rec.in_color(BLUE).len(), 3);
}

#[test]
fn a_label_change_requests_one_layout() {
    let _g = hold();
    let data = Categories::from_rows([("Mon", [3.0]), ("Tue", [5.0])]);
    let mut rig = rig(342.0, 160.0, || bar_chart(data.clone()));
    let (rec, needs_layout) = rig.frame();
    assert!(!needs_layout);
    assert_eq!(rec.runs().len(), 2);

    data.set_label(1, "Wednesday");
    let (rec, needs_layout) = rig.repaint();
    assert!(needs_layout, "a new label asks for a layout");
    assert_eq!(rec.runs().len(), 2, "this frame draws the labels it has");
    let (_, again) = rig.repaint();
    assert!(!again, "a paint asks once per change");

    let (rec, needs_layout) = rig.frame();
    assert!(!needs_layout);
    assert_eq!(rec.runs().len(), 2);
    assert_eq!(rec.runs()[1].len(), "Wednesday".len());

    // With labels off, a label change asks for nothing.
    let mut quiet = Rig::new(342.0, 160.0);
    quiet.rebuild(|| bar_chart(data.clone()).category_labels(false));
    quiet.frame();
    data.set_label(0, "Sunday");
    assert!(!quiet.repaint().1);
}

#[test]
fn crowded_labels_skip_every_other_rather_than_overlap() {
    let _g = hold();
    let days = [
        "Monday",
        "Tuesday",
        "Wednesday",
        "Thursday",
        "Friday",
        "Saturday",
        "Sunday",
    ];
    let data = Categories::from_rows(days.iter().map(|d| (*d, [1.0])));
    let spans = |rec: &Rec| -> Vec<(f64, f64)> {
        rec.runs()
            .iter()
            .map(|run| {
                let xs = run.iter().map(|p| p.x);
                let lo = xs.clone().fold(f64::INFINITY, f64::min);
                (lo, xs.fold(f64::NEG_INFINITY, f64::max))
            })
            .collect()
    };
    let mut wide = rig(1000.0, 160.0, || bar_chart(data.clone()));
    assert_eq!(wide.paint().runs().len(), 7);

    let mut narrow = rig(240.0, 160.0, || bar_chart(data.clone()));
    let rec = narrow.paint();
    let shown = spans(&rec);
    assert!(shown.len() < 7 && shown.len() >= 2, "{shown:?}");
    for pair in shown.windows(2) {
        assert!(pair[0].1 < pair[1].0, "labels overlap: {pair:?}");
    }
    // The first label is always drawn, and the stride is even: every k-th.
    let first = rec.runs()[0].len();
    assert_eq!(first, "Monday".len());
    assert_contained(&rec, Size::new(240.0, 160.0));
}

#[test]
fn a_value_axis_draws_rules_and_labels_in_a_gutter() {
    let _g = hold();
    let data = rows(&[&[30.0], &[80.0]]);
    let view = |side: AxisSide| {
        bar_chart(data.clone())
            .category_labels(false)
            .domain(YDomain::fixed(0.0, 100.0))
            .grid_color(GREY)
            .colors([RED])
            .y_axis(YAxis::new().side(side))
    };
    let mut left = rig(342.0, 160.0, || view(AxisSide::Left));
    let rec = left.paint();
    let rules = rec.in_color(GREY);
    let expected: Vec<f64> = ticks(0.0, 100.0, 3)
        .iter()
        .rev()
        .map(|t| {
            (160.0 - f64::from(*t) / 100.0 * 160.0 - 0.5)
                .round()
                .clamp(0.0, 159.0)
        })
        .collect();
    assert_eq!(rules.iter().map(|r| r.y0).collect::<Vec<_>>(), expected);
    let gutter = rules[0].x0;
    assert!(gutter > 0.0, "the gutter narrows the plot");
    assert!(
        rules
            .iter()
            .all(|r| r.x0 == gutter && r.x1 == 342.0 && r.height() == 1.0)
    );
    assert!(rec.in_color(RED).iter().all(|r| r.x0 >= gutter));
    assert_eq!(rec.runs().len(), expected.len());
    assert!(rec.runs().iter().flatten().all(|p| p.x < gutter));
    assert_contained(&rec, Size::new(342.0, 160.0));

    let mut right = rig(342.0, 160.0, || view(AxisSide::Right));
    let rec = right.paint();
    let rules = rec.in_color(GREY);
    assert!(rules.iter().all(|r| r.x0 == 0.0 && r.x1 == 342.0 - gutter));
    assert!(rec.runs().iter().flatten().all(|p| p.x > 342.0 - gutter));
}

#[test]
fn a_horizontal_value_axis_runs_along_the_bottom() {
    let _g = hold();
    let data = rows(&[&[30.0], &[80.0]]);
    let mut rig = rig(342.0, 160.0, || {
        bar_chart(data.clone())
            .horizontal()
            .domain(YDomain::fixed(0.0, 100.0))
            .grid_color(GREY)
            .colors([RED])
            .y_axis(YAxis::new())
    });
    let rec = rig.paint();
    let rules = rec.in_color(GREY);
    assert_eq!(rules.len(), 3);
    let band_top = rules[0].y1;
    assert!(band_top < 160.0, "the axis band shortens the plot");
    assert!(
        rules
            .iter()
            .all(|r| r.width() == 1.0 && r.y0 == 0.0 && r.y1 == band_top)
    );
    let names = rules[0].x0;
    assert!(names > 0.0, "category labels sit in a column on the left");
    // Two category labels on the left, three tick labels below the plot.
    let runs = rec.runs();
    assert_eq!(runs.len(), 5);
    assert!(runs[..3].iter().flatten().all(|p| p.y > band_top));
    assert!(
        runs[3..]
            .iter()
            .flatten()
            .all(|p| p.x < names && p.y < band_top)
    );
    assert_contained(&rec, Size::new(342.0, 160.0));
}

#[test]
fn an_auto_axis_follows_the_data_and_asks_for_layout_once() {
    let _g = hold();
    let data = rows(&[&[3.0], &[4.0]]);
    let mut rig = rig(342.0, 160.0, || {
        bar_chart(data.clone())
            .category_labels(false)
            .y_axis(YAxis::new())
    });
    let (_, needs_layout) = rig.frame();
    assert!(!needs_layout);
    data.set_value(0, 0, 3.5);
    assert!(!rig.repaint().1, "the same ticks need no layout");
    data.set_value(0, 0, 87.0);
    assert!(rig.repaint().1, "new ticks ask for a layout");
    assert!(!rig.repaint().1, "once");
    assert!(!rig.frame().1);
}

#[test]
fn grid_fractions_draw_when_there_is_no_axis() {
    let _g = hold();
    let data = rows(&[&[30.0]]);
    let mut vertical = rig(342.0, 160.0, || {
        bare(&data, YDomain::fixed(0.0, 100.0))
            .grid([0.0, 0.5, 1.0])
            .grid_color(GREY)
    });
    let ys: Vec<f64> = vertical
        .paint()
        .in_color(GREY)
        .iter()
        .map(|r| r.y0)
        .collect();
    assert_eq!(ys, [0.0, 80.0, 159.0]);
    let mut horizontal = rig(342.0, 160.0, || {
        bare(&data, YDomain::fixed(0.0, 100.0))
            .horizontal()
            .grid([0.0, 0.5, 1.0])
            .grid_color(GREY)
    });
    let xs: Vec<f64> = horizontal
        .paint()
        .in_color(GREY)
        .iter()
        .map(|r| r.x0)
        .collect();
    assert_eq!(xs, [0.0, 171.0, 341.0]);
}

#[test]
fn colours_come_from_the_tone_then_the_caller_then_the_theme_then_the_fallback() {
    let _g = hold();
    let data = rows(&[&[10.0, 90.0, 50.0, 20.0]]);
    let colors =
        |rig: &mut Rig| -> Vec<Color> { rig.paint().fills().iter().map(|(_, c)| *c).collect() };

    let mut fallback = rig(342.0, 160.0, || {
        bar_chart(data.clone()).category_labels(false)
    });
    assert_eq!(
        colors(&mut fallback),
        [
            Tone::FALLBACK_CALM,
            Tone::FALLBACK_BUSY,
            Tone::FALLBACK_CRITICAL,
            Tone::FALLBACK_CALM
        ]
    );

    let theme = Theme::neutral();
    let scheme = theme.scheme();
    let expected = [
        scheme.primary,
        scheme.secondary,
        scheme.tertiary,
        scheme.primary,
    ];
    let mut themed = Rig::new(342.0, 160.0);
    themed.root.set_theme(Box::new(theme));
    themed.rebuild(|| bar_chart(data.clone()).category_labels(false));
    assert_eq!(colors(&mut themed), expected);

    let mut explicit = Rig::new(342.0, 160.0);
    explicit.root.set_theme(Box::new(Theme::neutral()));
    explicit.rebuild(|| {
        bar_chart(data.clone())
            .category_labels(false)
            .colors([RED, BLUE])
    });
    assert_eq!(colors(&mut explicit), [RED, BLUE, RED, BLUE]);

    let tone = Tone::new(GREEN).stop(40.0, RED);
    let mut toned = rig(342.0, 160.0, || {
        bar_chart(data.clone())
            .category_labels(false)
            .colors([BLUE])
            .tone(tone.clone())
    });
    assert_eq!(colors(&mut toned), [GREEN, RED, RED, GREEN]);
}

#[test]
fn rebuild_flags_follow_what_changed() {
    let _g = hold();
    let data = rows(&[&[10.0]]);
    let base = || bar_chart(data.clone());
    let mut rig = rig(342.0, 160.0, base);
    rig.paint();
    assert_eq!(rig.rebuild(base), ChangeFlags::NONE);

    let paint_only: [&dyn Fn() -> BarChartView; 9] = [
        &|| base().mode(BarMode::Stacked),
        &|| base().colors([RED]),
        &|| base().tone(Tone::new(RED)),
        &|| base().bar_width(4.0),
        &|| base().group_gap(2.0),
        &|| base().radius(2.0),
        &|| base().domain(YDomain::percent()),
        &|| base().grid([0.5]),
        &|| base().semantics("Sales"),
    ];
    for next in paint_only {
        assert_eq!(rig.rebuild(next), ChangeFlags::PAINT);
        rig.rebuild(base);
    }

    let layout: [&dyn Fn() -> BarChartView; 6] = [
        &|| bar_chart(rows(&[&[10.0]])),
        &|| base().horizontal(),
        &|| base().category_labels(false),
        &|| base().label_style(12.0, BLUE),
        &|| base().y_axis(YAxis::new()),
        &|| base().height(80.0),
    ];
    for next in layout {
        assert_eq!(rig.rebuild(next), ChangeFlags::LAYOUT | ChangeFlags::PAINT);
        rig.rebuild(base);
    }
}

#[test]
fn input_is_ignored() {
    let _g = hold();
    let data = rows(&[&[10.0, 20.0]]);
    let mut rig = rig(342.0, 160.0, || bar_chart(data.clone()));
    rig.paint();
    for phase in [PointerPhase::Down, PointerPhase::Move, PointerPhase::Up] {
        let event = InputEvent::Pointer(PointerEvent {
            phase,
            position: Point::new(40.0, 120.0),
            button: PointerButton::Primary,
        });
        let outcome: EventOutcome = rig.root.event(&mut (), &event);
        assert!(!outcome.handled);
    }
}

#[test]
fn semantics_is_one_image_node_only_when_labelled() {
    let _g = hold();
    let data = rows(&[&[10.0]]);
    let images = |rig: &Rig| {
        rig.root
            .semantics()
            .nodes
            .into_iter()
            .filter(|(_, n)| n.role() == Role::Image)
            .map(|(_, n)| n.label().map(str::to_owned))
            .collect::<Vec<_>>()
    };
    let mut plain = rig(342.0, 160.0, || bar_chart(data.clone()));
    plain.paint();
    assert!(images(&plain).is_empty());
    let mut labelled = rig(342.0, 160.0, || bar_chart(data.clone()).semantics("Sales"));
    labelled.paint();
    assert_eq!(images(&labelled), [Some("Sales".to_owned())]);
}

#[test]
fn reference_size_paints_within_bounds() {
    let _g = hold();
    let data = Categories::from_rows([
        ("Q1", [12.0, -8.0, 30.0]),
        ("Q2", [15.0, 9.5, -4.0]),
        ("Q3", [22.0, 14.0, 6.0]),
        ("Q4", [-6.0, 18.0, 25.0]),
    ]);
    let size = Size::new(342.0, 160.0);
    for mode in [BarMode::Grouped, BarMode::Stacked] {
        for horizontal in [false, true] {
            for side in [AxisSide::Left, AxisSide::Right] {
                let mut rig = rig(size.width, size.height, || {
                    let view = bar_chart(data.clone())
                        .mode(mode)
                        .radius(3.0)
                        .y_axis(YAxis::new().side(side).tick_count(4))
                        .semantics("Quarterly");
                    if horizontal { view.horizontal() } else { view }
                });
                let rec = rig.paint();
                assert!(!rec.fills().is_empty());
                assert!(!rec.runs().is_empty());
                assert_contained(&rec, size);
            }
        }
    }
}

#[test]
fn an_empty_list_draws_only_rules() {
    let _g = hold();
    let data = Categories::new();
    let mut rig = rig(342.0, 160.0, || {
        bar_chart(data.clone()).grid([0.5]).grid_color(GREY)
    });
    let rec = rig.paint();
    assert_eq!(rec.fills().len(), 1);
    assert_eq!(rec.in_color(GREY).len(), 1);
}

#[test]
fn layout_fills_the_width_and_defaults_the_height() {
    let _g = hold();
    let data = rows(&[&[10.0]]);
    let mut fixed = rig(342.0, 400.0, || bar_chart(data.clone()).height(120.0));
    let rec = fixed.paint();
    let bar = rec.fills()[0].0;
    assert!(bar.y1 <= 120.0 && bar.y1 > 100.0, "{bar:?}");
}
