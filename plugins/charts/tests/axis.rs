//! The y axis: tick maths, tick formats, and `time_series` labels that follow an automatic
//! domain, headless on a real `RenderRoot` painting into a recording scene.
//!
//! Writing a `Series` requests a process-wide repaint, so every test holds `STATICS`.

use std::any::Any;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use frust::authoring::scene::GlyphRun;
use frust::authoring::text::TextContext;
use frust::authoring::{BezPath, Brush, ChangeFlags, PaintScene, Point, Rect, Size};
use frust::{Color, FrameTime};
use frust_charts::{
    AxisSide, Corner, PlotStyle, Series, SeriesStyle, TickFormat, TimeSeriesView, YAxis, YDomain,
    ticks, time_series,
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

// Tick maths.

/// `(min, max, target_count)` and the ticks expected for it.
type TickCase = ((f32, f32, usize), &'static [f32]);

#[test]
fn tick_tables() {
    let cases: [TickCase; 12] = [
        ((0.0, 100.0, 5), &[0.0, 25.0, 50.0, 75.0, 100.0]),
        ((0.0, 4.2, 3), &[0.0, 2.0, 4.0]),
        ((0.0, 0.37, 3), &[0.0, 0.1, 0.2, 0.3]),
        ((0.0, 1e9, 4), &[0.0, 5e8, 1e9]),
        // The nice tops an automatic domain resolves to.
        ((0.0, 50.0, 3), &[0.0, 25.0, 50.0]),
        ((0.0, 100.0, 3), &[0.0, 50.0, 100.0]),
        // Negative ranges.
        ((-50.0, 50.0, 5), &[-50.0, -25.0, 0.0, 25.0, 50.0]),
        ((-37.0, 81.0, 4), &[-25.0, 0.0, 25.0, 50.0, 75.0]),
        // A reversed range is swapped.
        ((100.0, 0.0, 5), &[0.0, 25.0, 50.0, 75.0, 100.0]),
        // Degenerate and non-finite ranges are widened the way `YDomain` widens them.
        ((5.0, 5.0, 3), &[5.0, 7.5, 10.0]),
        ((0.0, 0.0, 3), &[0.0, 0.5, 1.0]),
        ((f32::NAN, f32::NAN, 3), &[0.0, 0.5, 1.0]),
    ];
    for ((min, max, target), expected) in cases {
        assert_eq!(
            ticks(min, max, target),
            expected,
            "ticks({min}, {max}, {target})"
        );
    }
    // A target below 2 counts as 2.
    assert_eq!(ticks(0.0, 100.0, 0), ticks(0.0, 100.0, 2));
    assert_eq!(ticks(0.0, 100.0, 1), [0.0, 100.0]);
}

#[test]
fn ticks_are_whole_multiples_inside_the_range() {
    for (min, max) in [(0.0, 7.3), (-3.3, 12.9), (1e-6, 3e-6), (12_345.0, 98_765.0)] {
        for target in 2..12 {
            let t = ticks(min, max, target);
            assert!(!t.is_empty(), "{min}..{max} x{target}");
            assert!(t.windows(2).all(|w| w[0] < w[1]), "{t:?}");
            assert!(t.iter().all(|v| (min..=max).contains(v)), "{t:?}");
            // Evenly spaced: every gap is the first gap.
            if let [a, b, ..] = t[..] {
                let step = f64::from(b) - f64::from(a);
                for w in t.windows(2) {
                    let gap = f64::from(w[1]) - f64::from(w[0]);
                    assert!((gap - step).abs() <= step * 1e-5, "{t:?}");
                }
            }
        }
    }
}

#[test]
fn a_thousand_ticks_do_not_drift() {
    let t = ticks(0.0, 100.0, 1000);
    assert_eq!(t.len(), 1001);
    for (i, v) in t.iter().enumerate() {
        assert_eq!(*v, (i as f64 / 10.0) as f32, "tick {i}");
    }
    assert_eq!(t.last(), Some(&100.0));
    // Repeated addition of the same step does drift, which is what indexing avoids.
    let mut sum = 0.0f32;
    for _ in 0..1000 {
        sum += 0.1;
    }
    assert_ne!(sum, 100.0);
}

// Formats.

#[test]
fn plain_and_percent_trim_their_decimals() {
    let plain = TickFormat::plain();
    let cases = [
        (25.0, "25"),
        (2.5, "2.5"),
        (0.1, "0.1"),
        (0.3, "0.3"),
        (0.0025, "0.0025"),
        (-12.5, "-12.5"),
        (-0.0, "0"),
        (0.0, "0"),
        (1e9, "1000000000"),
        (1.0 / 3.0, "0.333333"),
    ];
    for (value, text) in cases {
        assert_eq!(plain.format(value), text, "{value}");
    }
    assert_eq!(TickFormat::default().format(7.5), "7.5");
    assert_eq!(TickFormat::percent().format(50.0), "50 %");
    assert_eq!(TickFormat::percent().format(12.5), "12.5 %");
    assert_eq!(plain.format(f32::NAN), "NaN");
}

#[test]
fn metric_scales_by_thousands() {
    let metric = TickFormat::metric();
    let cases = [
        (0.0, "0"),
        (0.5, "0.5"),
        (950.0, "950"),
        (1000.0, "1k"),
        (1500.0, "1.5k"),
        (-1500.0, "-1.5k"),
        (12_500.0, "12.5k"),
        (2.5e8, "250M"),
        (1e9, "1G"),
        (2e12, "2T"),
        (5e15, "5000T"),
        // A mantissa that rounds up to 1000 moves to the next unit.
        (999_999.0, "1M"),
    ];
    for (value, text) in cases {
        assert_eq!(metric.format(value), text, "{value}");
    }
}

#[test]
fn bytes_scale_by_1024_with_an_optional_rate() {
    let bytes = TickFormat::bytes();
    let cases = [
        (0.0, "0 B"),
        (512.0, "512 B"),
        (1024.0, "1 KiB"),
        (1536.0, "1.5 KiB"),
        (1_000_000.0, "977 KiB"),
        (1_048_576.0, "1 MiB"),
        (5.0 * 1024.0 * 1024.0 * 1024.0, "5 GiB"),
        (2.0 * 1024f32.powi(4), "2 TiB"),
        (1024f32.powi(5), "1 PiB"),
    ];
    for (value, text) in cases {
        assert_eq!(bytes.format(value), text, "{value}");
    }
    assert_eq!(TickFormat::bytes_per_second().format(1536.0), "1.5 KiB/s");
    assert_eq!(TickFormat::bytes_per_second().format(0.0), "0 B/s");
}

#[test]
fn a_custom_format_compares_by_identity() {
    let celsius = TickFormat::custom(|v| format!("{v}°"));
    assert_eq!(celsius.format(21.5), "21.5°");
    assert_eq!(celsius, celsius.clone());
    assert_ne!(celsius, TickFormat::custom(|v| format!("{v}°")));
    assert_eq!(TickFormat::plain(), TickFormat::plain());
    assert_eq!(TickFormat::bytes(), TickFormat::bytes());
    assert_ne!(TickFormat::bytes(), TickFormat::bytes_per_second());
    assert_ne!(TickFormat::plain(), TickFormat::percent());
    assert_eq!(format!("{:?}", TickFormat::metric()), "TickFormat::Metric");
}

// The time_series axis.

/// One recorded glyph run.
#[derive(Clone, Debug)]
struct Run {
    glyphs: usize,
    font_size: f32,
    color: Option<Color>,
    /// Glyph origins in scene space.
    points: Vec<Point>,
}

/// One recorded draw.
#[derive(Clone, Debug)]
enum Op {
    Rect(Rect, Color),
    Stroke(Vec<Point>),
    Glyphs(Run),
    Other,
}

#[derive(Default)]
struct Rec {
    ops: Vec<Op>,
    clips: Vec<Rect>,
}

impl Rec {
    fn rules(&self) -> Vec<Rect> {
        self.ops
            .iter()
            .filter_map(|op| match op {
                Op::Rect(r, c) if *c == GREY => Some(*r),
                _ => None,
            })
            .collect()
    }

    fn rule_ys(&self) -> Vec<f64> {
        self.rules().iter().map(|r| r.y0).collect()
    }

    fn strokes(&self) -> Vec<Vec<Point>> {
        self.ops
            .iter()
            .filter_map(|op| match op {
                Op::Stroke(points) => Some(points.clone()),
                _ => None,
            })
            .collect()
    }

    /// The glyph runs, in draw order.
    fn runs(&self) -> Vec<Run> {
        self.ops
            .iter()
            .filter_map(|op| match op {
                Op::Glyphs(run) => Some(run.clone()),
                _ => None,
            })
            .collect()
    }

    /// The glyph count of every run, in draw order.
    fn glyph_counts(&self) -> Vec<usize> {
        self.runs().iter().map(|r| r.glyphs).collect()
    }
}

impl PaintScene for Rec {
    fn fill_rect(&mut self, origin: Point, size: Size, color: Color) {
        self.ops
            .push(Op::Rect(Rect::from_origin_size(origin, size), color));
    }
    fn fill_rounded_rect(&mut self, _: Point, _: Size, _: f64, _: Color) {
        self.ops.push(Op::Other);
    }
    fn fill_path(&mut self, _: Point, _: &BezPath, _: &Brush) {
        self.ops.push(Op::Other);
    }
    fn stroke_path(&mut self, origin: Point, path: &BezPath, _: f64, _: &Brush) {
        let points = path
            .elements()
            .iter()
            .filter_map(|el| el.end_point())
            .map(|p| p + origin.to_vec2())
            .collect();
        self.ops.push(Op::Stroke(points));
    }
    fn push_clip(&mut self, origin: Point, size: Size) {
        self.clips.push(Rect::from_origin_size(origin, size));
    }
    fn pop_clip(&mut self) {}
    fn draw_glyph_run(&mut self, run: GlyphRun) {
        let color = match &run.brush {
            Brush::Solid(color) => Some(*color),
            _ => None,
        };
        let points = run
            .glyphs
            .iter()
            .map(|g| run.transform * Point::new(f64::from(g.x), f64::from(g.y)))
            .collect();
        self.ops.push(Op::Glyphs(Run {
            glyphs: run.glyphs.len(),
            font_size: run.font_size,
            color,
            points,
        }));
    }
    fn draw_text(&mut self, _origin: Point, _text: &str) {}
}

type View = TimeSeriesView<()>;

struct Rig {
    rt: &'static ReactiveRuntime,
    root: RenderRoot<(), View>,
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
        (rec, outcome.needs_layout)
    }

    /// A full frame: layout, then paint.
    fn frame(&mut self) -> (Rec, bool) {
        self.layout();
        self.repaint()
    }
}

fn rig(width: f64, height: f64, view: impl Fn() -> View) -> Rig {
    let mut rig = Rig::new(width, height);
    rig.rebuild(view);
    rig
}

fn series_of(capacity: usize, slots: &[f32]) -> Series {
    let series = Series::new(capacity);
    series.extend(slots.iter().map(|&v| Some(v)));
    series
}

/// Every glyph origin of every run lies inside `bounds`.
fn assert_glyphs_inside(rec: &Rec, bounds: Rect) {
    let runs = rec.runs();
    assert!(!runs.is_empty(), "no labels drawn");
    for run in runs {
        for p in run.points {
            assert!(
                p.x >= bounds.x0 - EPS
                    && p.x <= bounds.x1 + EPS
                    && p.y >= bounds.y0 - EPS
                    && p.y <= bounds.y1 + EPS,
                "glyph at {p:?} outside {bounds:?}"
            );
        }
    }
}

#[test]
fn an_auto_domain_crossing_a_nice_boundary_requests_layout_once() {
    let _g = hold();
    let s = series_of(4, &[10.0, 40.0]);
    let mut rig = rig(342.0, 56.0, || {
        time_series()
            .style(PlotStyle::Line)
            .domain(YDomain::auto_from_zero())
            .grid_color(GREY)
            .y_axis(YAxis::new())
            .series(s.clone(), SeriesStyle::color(RED))
    });
    // 0..50: ticks 0, 25 and 50, laid out by the first frame.
    let (rec, asked) = rig.frame();
    assert!(!asked);
    assert_eq!(rec.glyph_counts(), [1, 2, 2]);
    assert_eq!(rec.rule_ys(), [0.0, 28.0, 55.0]);

    // 60 moves the domain to 0..100. The repaint draws the new rules at once, keeps the old
    // labels (each at its own value) and asks for one layout.
    s.push(60.0);
    let (rec, asked) = rig.repaint();
    assert!(asked);
    assert_eq!(rec.rule_ys(), [0.0, 28.0, 55.0]);
    assert_eq!(rec.glyph_counts(), [1, 2, 2]);
    // "50" now sits at the middle, not the top.
    let fifty = &rec.runs()[2].points;
    assert!(fifty.iter().all(|p| p.y > 28.0 && p.y < 42.0), "{fifty:?}");

    // Later repaints before that layout do not ask again, even with more data.
    assert!(!rig.repaint().1);
    s.push(70.0);
    assert!(!rig.repaint().1);

    // The layout shapes 0, 50 and 100.
    let (rec, asked) = rig.frame();
    assert!(!asked);
    assert_eq!(rec.glyph_counts(), [1, 2, 3]);
    assert!(!rig.repaint().1);
    assert!(!rig.frame().1);

    // Falling to 0..5 asks once more, and the layout shapes 0, 2.5 and 5.
    for v in [1.0, 2.0, 3.0, 4.0] {
        s.push(v);
    }
    assert!(rig.repaint().1);
    assert!(!rig.repaint().1);
    let (rec, _) = rig.frame();
    assert_eq!(rec.glyph_counts(), [1, 3, 1]);
}

#[test]
fn a_fixed_domain_never_requests_layout_from_paint() {
    let _g = hold();
    let s = series_of(8, &[10.0]);
    let mut rig = rig(342.0, 56.0, || {
        time_series()
            .domain(YDomain::percent())
            .grid_color(GREY)
            .y_axis(YAxis::new().format(TickFormat::percent()))
            .series(s.clone(), SeriesStyle::color(RED))
    });
    let (rec, asked) = rig.frame();
    assert!(!asked);
    assert_eq!(rec.runs().len(), 3);
    for i in 0..40 {
        s.push((i * 37 % 250) as f32 - 50.0);
        assert!(!rig.repaint().1, "push {i}");
        if i % 5 == 0 {
            assert!(!rig.frame().1, "frame {i}");
        }
    }
}

#[test]
fn tick_rules_replace_the_grid_fractions() {
    let _g = hold();
    let s = Series::new(8);
    let mut rig = rig(342.0, 56.0, || {
        time_series()
            .grid([0.1, 0.2, 0.3, 0.4])
            .grid_color(GREY)
            .y_axis(YAxis::new().tick_count(5))
            .series(s.clone(), SeriesStyle::color(RED))
    });
    let (rec, _) = rig.frame();
    // 0, 25, 50, 75 and 100 of 0..100, on whole pixel rows inside the plot.
    assert_eq!(rec.rule_ys(), [0.0, 14.0, 28.0, 42.0, 55.0]);
    for rule in rec.rules() {
        assert_eq!((rule.x0, rule.x1, rule.height()), (0.0, 342.0, 1.0));
    }
    assert_eq!(rec.runs().len(), 5);
}

#[test]
fn a_mirrored_plot_ticks_each_half_and_labels_the_middle_axis_once() {
    let _g = hold();
    let up = series_of(4, &[30.0, 40.0]);
    let down = series_of(4, &[300.0, 400.0]);
    let mut rig = rig(342.0, 56.0, || {
        time_series()
            .style(PlotStyle::Line)
            .domain(YDomain::auto_from_zero())
            .grid_color(GREY)
            .y_axis(YAxis::new())
            .mirrored(
                (up.clone(), SeriesStyle::color(RED)),
                (down.clone(), SeriesStyle::color(BLUE)),
            )
    });
    let (rec, asked) = rig.frame();
    assert!(!asked);
    // Upper 0..50: 0 (on the middle axis), 25, 50; lower 0..500: 250, 500 (its 0 is the
    // middle axis, already labelled).
    assert_eq!(rec.glyph_counts(), [1, 2, 2, 3, 3]);
    assert_eq!(rec.rule_ys(), [0.0, 14.0, 27.0, 42.0, 55.0]);
    assert_glyphs_inside(&rec, Rect::new(0.0, 0.0, 342.0, 56.0));
}

#[test]
fn labels_stay_inside_the_bounds_at_342_by_56() {
    let _g = hold();
    let s = series_of(30, &[5.0, 95.0, 40.0, 1234.0]);
    let bounds = Rect::new(0.0, 0.0, 342.0, 56.0);
    for side in [AxisSide::Left, AxisSide::Right] {
        for gutter in [false, true] {
            for domain in [YDomain::percent(), YDomain::auto_from_zero()] {
                let mut rig = rig(342.0, 56.0, || {
                    time_series()
                        .domain(domain)
                        .grid_color(GREY)
                        .label(Corner::TopRight, "max")
                        .y_axis(
                            YAxis::new()
                                .side(side)
                                .gutter(gutter)
                                .tick_count(4)
                                .format(TickFormat::metric()),
                        )
                        .series(s.clone(), SeriesStyle::color(RED))
                });
                let (rec, _) = rig.frame();
                assert_eq!(rec.clips, [bounds]);
                assert!(rec.runs().len() >= 3, "{side:?} {gutter} {domain:?}");
                assert_glyphs_inside(&rec, bounds);
                for rule in rec.rules() {
                    assert!(rule.x0 >= 0.0 && rule.x1 <= 342.0 && rule.y0 >= 0.0);
                    assert!(rule.y1 <= 56.0, "{rule:?}");
                }
            }
        }
    }
}

#[test]
fn a_gutter_narrows_the_plot_by_the_widest_label() {
    let _g = hold();
    let s = series_of(4, &[10.0, 60.0, 90.0]);
    let view = |side: AxisSide| {
        let s = s.clone();
        move || {
            time_series()
                .style(PlotStyle::Line)
                .grid_color(GREY)
                .y_axis(YAxis::new().side(side).gutter(true))
                .series(s.clone(), SeriesStyle::color(RED))
        }
    };

    let mut left = rig(342.0, 56.0, view(AxisSide::Left));
    let (rec, _) = left.frame();
    let rules = rec.rules();
    let gutter = rules[0].x0;
    assert!(gutter > 0.0 && gutter.fract() == 0.0, "{gutter}");
    assert!(rules.iter().all(|r| r.x0 == gutter && r.x1 == 342.0));
    let strokes = rec.strokes();
    assert_eq!(strokes[0].last().unwrap().x, 342.0);
    // The oldest of 4 slots sits at the plot's left edge, the gutter.
    let xs: Vec<f64> = strokes[0].iter().map(|p| p.x).collect();
    assert!(xs.iter().all(|&x| x > gutter), "{xs:?}");
    for run in rec.runs() {
        assert!(run.points.iter().all(|p| p.x < gutter), "{run:?}");
    }

    let mut right = rig(342.0, 56.0, view(AxisSide::Right));
    let (rec, _) = right.frame();
    let rules = rec.rules();
    let edge = rules[0].x1;
    assert!(edge < 342.0 && rules.iter().all(|r| r.x0 == 0.0 && r.x1 == edge));
    assert!(rec.strokes()[0].last().unwrap().x <= edge + EPS);
    for run in rec.runs() {
        assert!(run.points.iter().all(|p| p.x > edge), "{run:?}");
    }
    // Both gutters fit the same labels.
    assert_eq!(342.0 - edge, gutter);
}

#[test]
fn labels_default_to_the_corner_label_style() {
    let _g = hold();
    let s = Series::new(4);
    let mut inherited = rig(342.0, 56.0, || {
        time_series()
            .label_style(11.0, BLUE)
            .y_axis(YAxis::new())
            .series(s.clone(), SeriesStyle::color(RED))
    });
    let runs = inherited.frame().0.runs();
    assert!(!runs.is_empty());
    assert!(
        runs.iter()
            .all(|r| r.font_size == 11.0 && r.color == Some(BLUE)),
        "{runs:?}"
    );

    let mut own = rig(342.0, 56.0, || {
        time_series()
            .label_style(11.0, BLUE)
            .y_axis(YAxis::new().size(8.0).color(RED))
            .series(s.clone(), SeriesStyle::color(RED))
    });
    let runs = own.frame().0.runs();
    assert!(
        runs.iter()
            .all(|r| r.font_size == 8.0 && r.color == Some(RED)),
        "{runs:?}"
    );
}

#[test]
fn axis_changes_need_layout() {
    let _g = hold();
    let s = Series::new(8);
    let format = TickFormat::custom(|v| format!("{v} ms"));
    let base = || {
        time_series()
            .series(s.clone(), SeriesStyle::color(RED))
            .y_axis(YAxis::new().format(format.clone()))
    };
    let mut rig = rig(342.0, 56.0, base);
    rig.frame();
    // The same closure, cloned, is the same format.
    assert_eq!(rig.rebuild(base), ChangeFlags::NONE);

    let changed: [&dyn Fn() -> View; 7] = [
        &|| base().y_axis(YAxis::new().format(format.clone()).side(AxisSide::Right)),
        &|| base().y_axis(YAxis::new().format(format.clone()).gutter(true)),
        &|| base().y_axis(YAxis::new().format(format.clone()).tick_count(5)),
        &|| base().y_axis(YAxis::new().format(format.clone()).size(12.0)),
        &|| base().y_axis(YAxis::new().format(format.clone()).color(BLUE)),
        // A fresh closure is a new format.
        &|| base().y_axis(YAxis::new().format(TickFormat::custom(|v| format!("{v} ms")))),
        &|| time_series().series(s.clone(), SeriesStyle::color(RED)),
    ];
    for next in changed {
        assert_eq!(rig.rebuild(next), ChangeFlags::LAYOUT | ChangeFlags::PAINT);
        rig.rebuild(base);
        rig.frame();
    }
}

#[test]
fn removing_the_axis_restores_the_grid_and_the_full_plot() {
    let _g = hold();
    let s = series_of(4, &[10.0, 20.0]);
    let with = || {
        time_series()
            .style(PlotStyle::Line)
            .grid([0.5])
            .grid_color(GREY)
            .y_axis(YAxis::new().gutter(true))
            .series(s.clone(), SeriesStyle::color(RED))
    };
    let without = || {
        time_series()
            .style(PlotStyle::Line)
            .grid([0.5])
            .grid_color(GREY)
            .series(s.clone(), SeriesStyle::color(RED))
    };
    let mut rig = rig(342.0, 56.0, with);
    let (rec, _) = rig.frame();
    assert_eq!(rec.rules().len(), 3);
    rig.rebuild(without);
    let (rec, asked) = rig.frame();
    assert!(!asked);
    assert!(rec.runs().is_empty());
    assert_eq!(rec.rule_ys(), [28.0]);
    assert_eq!((rec.rules()[0].x0, rec.rules()[0].x1), (0.0, 342.0));
}
