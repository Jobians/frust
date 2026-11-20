//! `time_series` headless: a real `RenderRoot` painting into a recording scene.
//!
//! Writing a `Series` requests a process-wide repaint, so every test holds `STATICS`.

use std::any::Any;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use frust::authoring::scene::GlyphRun;
use frust::authoring::text::TextContext;
use frust::authoring::{
    BezPath, Brush, ChangeFlags, EventOutcome, InputEvent, PaintScene, Point, PointerButton,
    PointerEvent, PointerPhase, Rect, Role, Shape, Size,
};
use frust::{Color, FrameTime, Theme};
use frust_charts::{
    Corner, PlotStyle, Series, SeriesStyle, TimeSeriesView, Tone, YDomain, auto_domain, time_series,
};
use frust_core::RenderRoot;
use frust_reactive::ReactiveRuntime;

static STATICS: Mutex<()> = Mutex::new(());

fn hold() -> MutexGuard<'static, ()> {
    STATICS.lock().unwrap_or_else(PoisonError::into_inner)
}

const RED: Color = Color::from_rgb8(0xd0, 0x10, 0x10);
const BLUE: Color = Color::from_rgb8(0x10, 0x20, 0xd0);
const EPS: f64 = 1e-6;

/// One recorded draw, with the clip depth it was issued under.
#[derive(Clone, Debug)]
enum Op {
    Fill(Rect, Brush),
    Stroke(Vec<Point>, f64, Brush),
    Rect(Rect, Color),
    RoundedRect(Rect, f64, Color),
    Glyphs(usize),
}

#[derive(Default)]
struct Rec {
    ops: Vec<(Op, usize)>,
    clips: Vec<Rect>,
    clip_depth: usize,
    max_clip_depth: usize,
}

fn vertices(origin: Point, path: &BezPath) -> Vec<Point> {
    path.elements()
        .iter()
        .filter_map(|el| el.end_point())
        .map(|p| p + origin.to_vec2())
        .collect()
}

impl Rec {
    fn push(&mut self, op: Op) {
        self.ops.push((op, self.clip_depth));
    }

    fn fills(&self) -> Vec<(Rect, Brush)> {
        self.ops
            .iter()
            .filter_map(|(op, _)| match op {
                Op::Fill(r, b) => Some((*r, b.clone())),
                _ => None,
            })
            .collect()
    }

    /// Fills with a gradient brush: the areas under a line.
    fn area_fills(&self) -> Vec<Rect> {
        self.fills()
            .into_iter()
            .filter(|(_, b)| matches!(b, Brush::Gradient(_)))
            .map(|(r, _)| r)
            .collect()
    }

    /// Fills with a solid brush: the isolated-sample dots.
    fn dots(&self) -> Vec<Rect> {
        self.fills()
            .into_iter()
            .filter(|(_, b)| matches!(b, Brush::Solid(_)))
            .map(|(r, _)| r)
            .collect()
    }

    fn strokes(&self) -> Vec<(Vec<Point>, f64, Brush)> {
        self.ops
            .iter()
            .filter_map(|(op, _)| match op {
                Op::Stroke(p, w, b) => Some((p.clone(), *w, b.clone())),
                _ => None,
            })
            .collect()
    }

    fn rects(&self) -> Vec<(Rect, Color)> {
        self.ops
            .iter()
            .filter_map(|(op, _)| match op {
                Op::Rect(r, c) => Some((*r, *c)),
                _ => None,
            })
            .collect()
    }

    fn glyph_runs(&self) -> usize {
        self.ops
            .iter()
            .map(|(op, _)| match op {
                Op::Glyphs(n) => *n,
                _ => 0,
            })
            .sum()
    }
}

impl PaintScene for Rec {
    fn fill_rect(&mut self, origin: Point, size: Size, color: Color) {
        self.push(Op::Rect(Rect::from_origin_size(origin, size), color));
    }
    fn fill_rounded_rect(&mut self, origin: Point, size: Size, radius: f64, color: Color) {
        let rect = Rect::from_origin_size(origin, size);
        self.push(Op::RoundedRect(rect, radius, color));
    }
    fn fill_path(&mut self, origin: Point, path: &BezPath, brush: &Brush) {
        let rect = path.bounding_box() + origin.to_vec2();
        self.push(Op::Fill(rect, brush.clone()));
    }
    fn stroke_path(&mut self, origin: Point, path: &BezPath, width: f64, brush: &Brush) {
        self.push(Op::Stroke(vertices(origin, path), width, brush.clone()));
    }
    fn push_clip(&mut self, origin: Point, size: Size) {
        self.clips.push(Rect::from_origin_size(origin, size));
        self.clip_depth += 1;
        self.max_clip_depth = self.max_clip_depth.max(self.clip_depth);
    }
    fn pop_clip(&mut self) {
        self.clip_depth -= 1;
    }
    fn draw_glyph_run(&mut self, _run: GlyphRun) {
        self.push(Op::Glyphs(1));
    }
    fn draw_text(&mut self, _origin: Point, _text: &str) {}
}

type View = TimeSeriesView<()>;

struct Rig {
    rt: &'static ReactiveRuntime,
    root: RenderRoot<(), View>,
    text: TextContext,
}

impl Rig {
    fn new() -> Self {
        Self {
            rt: ReactiveRuntime::init(Arc::new(|| {})),
            root: RenderRoot::new(),
            text: TextContext::new(),
        }
    }

    fn rebuild(&mut self, view: impl Fn() -> View) -> ChangeFlags {
        let mut logic = |_: &mut ()| view();
        self.rt
            .with_owner(|| self.root.rebuild(&mut logic, &mut ()))
    }

    fn paint(&mut self, width: f64, height: f64) -> Rec {
        self.root
            .layout_with_text(Size::new(width, height), &mut self.text as &mut dyn Any);
        let mut rec = Rec::default();
        self.root
            .paint(&mut rec, FrameTime::from_nanos(1_000_000_000));
        assert_eq!(rec.clip_depth, 0, "every pushed clip is popped");
        rec
    }
}

fn rig(view: impl Fn() -> View) -> Rig {
    let mut rig = Rig::new();
    rig.rebuild(view);
    rig
}

fn series_of(capacity: usize, slots: &[Option<f32>]) -> Series {
    let series = Series::new(capacity);
    series.extend(slots.iter().copied());
    series
}

fn solid(brush: &Brush) -> Color {
    match brush {
        Brush::Solid(color) => *color,
        other => panic!("expected a solid brush, got {other:?}"),
    }
}

fn near(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-3
}

fn inside(rect: Rect, bounds: Rect) -> bool {
    rect.x0 >= bounds.x0 - EPS
        && rect.y0 >= bounds.y0 - EPS
        && rect.x1 <= bounds.x1 + EPS
        && rect.y1 <= bounds.y1 + EPS
}

/// Every draw is issued under one clip equal to the widget bounds, and every fill, rect and
/// stroke vertex lies inside the bounds (dots may overhang an edge, inside the clip).
fn assert_contained(rec: &Rec, size: Size) {
    let bounds = Rect::from_origin_size(Point::ZERO, size);
    assert_eq!(rec.clips, [bounds]);
    assert_eq!(rec.max_clip_depth, 1);
    for (op, depth) in &rec.ops {
        assert_eq!(*depth, 1, "{op:?} drawn outside the clip");
        match op {
            Op::Rect(r, _) | Op::RoundedRect(r, _, _) => assert!(inside(*r, bounds), "{r:?}"),
            Op::Fill(r, Brush::Gradient(_)) => assert!(inside(*r, bounds), "{r:?}"),
            Op::Stroke(points, _, _) => {
                for p in points {
                    assert!(inside(Rect::from_points(*p, *p), bounds), "{p:?}");
                }
            }
            Op::Fill(..) | Op::Glyphs(_) => {}
        }
    }
}

#[test]
fn area_draws_one_fill_and_one_stroke_per_run_and_nothing_across_a_gap() {
    let _g = hold();
    // Seven slots in a 10-slot ring: spacing 342 / 9 = 38, the gap (age 3) at x = 228.
    let s = series_of(
        10,
        &[
            Some(10.0),
            Some(20.0),
            Some(30.0),
            None,
            Some(40.0),
            Some(50.0),
            Some(60.0),
        ],
    );
    let mut rig = rig(|| time_series().series(s.clone(), SeriesStyle::color(RED)));
    let rec = rig.paint(342.0, 56.0);
    let fills = rec.area_fills();
    let strokes = rec.strokes();
    assert_eq!(fills.len(), 2);
    assert_eq!(strokes.len(), 2);
    assert!(rec.dots().is_empty());

    let gap_x = 228.0;
    assert!(fills[0].x1 < gap_x && fills[1].x0 > gap_x, "{fills:?}");
    let xs = |points: &[Point]| points.iter().map(|p| p.x).collect::<Vec<_>>();
    assert_eq!(xs(&strokes[0].0), [114.0, 152.0, 190.0]);
    assert_eq!(xs(&strokes[1].0), [266.0, 304.0, 342.0]);
    assert!(strokes.iter().all(|(_, w, b)| *w == 1.5 && solid(b) == RED));
    // The fill sits on the baseline.
    assert!(fills.iter().all(|r| near(r.y1, 56.0)));
}

#[test]
fn the_derived_fill_fades_from_the_top_of_the_plot_to_the_baseline() {
    let _g = hold();
    let s = series_of(4, &[Some(50.0), Some(60.0)]);
    let mut rig = rig(|| time_series().series(s.clone(), SeriesStyle::color(RED)));
    let rec = rig.paint(200.0, 56.0);
    let (_, brush) = rec.fills().remove(0);
    let Brush::Gradient(gradient) = brush else {
        panic!("{brush:?}")
    };
    let frust::peniko::GradientKind::Linear(line) = gradient.kind else {
        panic!("{:?}", gradient.kind)
    };
    assert_eq!((line.start.y, line.end.y), (0.0, 56.0));
    assert_eq!(line.start.x, line.end.x);
    let alphas: Vec<f32> = gradient
        .stops
        .iter()
        .map(|s| s.color.components[3])
        .collect();
    assert!((alphas[0] - 0.40).abs() < 1e-3 && (alphas[1] - 0.03).abs() < 1e-3);
}

#[test]
fn an_isolated_sample_is_a_dot() {
    let _g = hold();
    let s = series_of(
        8,
        &[Some(10.0), None, Some(50.0), None, Some(20.0), Some(30.0)],
    );
    let mut rig = rig(|| time_series().series(s.clone(), SeriesStyle::color(RED)));
    let rec = rig.paint(342.0, 56.0);
    let dots = rec.dots();
    assert_eq!(dots.len(), 2, "{dots:?}");
    assert_eq!(rec.strokes().len(), 1);
    // Diameter max(3, 2 x 1.5) = 3.
    for dot in &dots {
        assert!(near(dot.width(), 3.0) && near(dot.height(), 3.0), "{dot:?}");
    }
    // A wide stroke grows the dot.
    let mut wide =
        self::rig(|| time_series().series(s.clone(), SeriesStyle::color(RED).stroke_width(4.0)));
    let dots = wide.paint(342.0, 56.0).dots();
    assert!(dots.iter().all(|d| near(d.width(), 8.0)), "{dots:?}");
}

#[test]
fn line_style_draws_no_fill() {
    let _g = hold();
    let s = series_of(6, &[Some(10.0), Some(20.0), None, Some(30.0), Some(40.0)]);
    let mut rig = rig(|| {
        time_series()
            .style(PlotStyle::Line)
            .series(s.clone(), SeriesStyle::color(RED))
    });
    let rec = rig.paint(342.0, 56.0);
    assert!(rec.fills().is_empty());
    assert_eq!(rec.strokes().len(), 2);
}

#[test]
fn blocks_draw_one_rect_per_present_slot() {
    let _g = hold();
    let s = series_of(16, &[Some(10.0), None, Some(70.0), Some(90.0)]);
    let tone = Tone::load(BLUE, Color::from_rgb8(0xe0, 0xa0, 0x10), RED);
    let mut rig = rig(|| {
        time_series()
            .style(PlotStyle::Blocks {
                column_width: 4.0,
                gap: 2.0,
            })
            .series(s.clone(), SeriesStyle::color(BLUE).tone(tone.clone()))
    });
    let rec = rig.paint(342.0, 56.0);
    let rects = rec.rects();
    assert_eq!(rects.len(), 3, "{rects:?}");
    assert!(rec.fills().is_empty() && rec.strokes().is_empty());
    let x1s: Vec<f64> = rects.iter().map(|(r, _)| r.x1).collect();
    // Newest at the right edge, the gap slot (age 2) left empty.
    assert_eq!(x1s, [324.0, 336.0, 342.0]);
    assert!(rects.iter().all(|(r, _)| r.width() == 4.0 && r.y1 == 56.0));
    let colors: Vec<Color> = rects.iter().map(|(_, c)| *c).collect();
    assert_eq!(colors, [BLUE, tone.color_at(70.0), RED]);
}

#[test]
fn blocks_take_a_corner_radius() {
    let _g = hold();
    let s = series_of(4, &[Some(50.0), Some(60.0)]);
    let mut rig = rig(|| {
        time_series()
            .style(PlotStyle::Blocks {
                column_width: 6.0,
                gap: 1.0,
            })
            .corner(2.0)
            .series(s.clone(), SeriesStyle::color(RED))
    });
    let rec = rig.paint(200.0, 56.0);
    let rounded: Vec<(f64, Color)> = rec
        .ops
        .iter()
        .filter_map(|(op, _)| match op {
            Op::RoundedRect(_, radius, color) => Some((*radius, *color)),
            _ => None,
        })
        .collect();
    assert_eq!(rounded, [(2.0, RED), (2.0, RED)]);
    assert!(rec.rects().is_empty());
}

#[test]
fn two_series_paint_in_order() {
    let _g = hold();
    let a = series_of(4, &[Some(10.0), Some(20.0)]);
    let b = series_of(4, &[Some(30.0), Some(40.0)]);
    let mut rig = rig(|| {
        time_series()
            .series(a.clone(), SeriesStyle::color(RED))
            .series(b.clone(), SeriesStyle::color(BLUE))
    });
    let rec = rig.paint(342.0, 56.0);
    let order: Vec<&str> = rec
        .ops
        .iter()
        .filter_map(|(op, _)| match op {
            Op::Fill(_, Brush::Gradient(_)) => Some("fill"),
            Op::Stroke(_, _, b) if solid(b) == RED => Some("red"),
            Op::Stroke(_, _, b) if solid(b) == BLUE => Some("blue"),
            _ => None,
        })
        .collect();
    assert_eq!(order, ["fill", "red", "fill", "blue"]);
}

#[test]
fn a_mirrored_pair_hangs_the_lower_series_below_the_middle_axis() {
    let _g = hold();
    let up = series_of(4, &[Some(50.0), Some(50.0)]);
    let down = series_of(4, &[Some(50.0), Some(50.0)]);
    let mut rig = rig(|| {
        time_series().style(PlotStyle::Line).mirrored(
            (up.clone(), SeriesStyle::color(RED)),
            (down.clone(), SeriesStyle::color(BLUE)),
        )
    });
    let rec = rig.paint(342.0, 56.0);
    let strokes = rec.strokes();
    assert_eq!(strokes.len(), 2);
    // Middle axis at 28: 50 % is 14 px above it and 14 px below it.
    assert!(strokes[0].0.iter().all(|p| near(p.y, 14.0)), "{strokes:?}");
    assert!(strokes[1].0.iter().all(|p| near(p.y, 42.0)), "{strokes:?}");
    assert_eq!(solid(&strokes[1].2), BLUE);

    // An area's lower fill hangs from the axis.
    let mut area = self::rig(|| {
        time_series().mirrored(
            (up.clone(), SeriesStyle::color(RED)),
            (down.clone(), SeriesStyle::color(BLUE)),
        )
    });
    let fills = area.paint(342.0, 56.0).area_fills();
    assert!(
        near(fills[0].y1, 28.0) && near(fills[1].y0, 28.0),
        "{fills:?}"
    );
}

#[test]
fn mirrored_halves_resolve_their_own_domain_unless_shared() {
    let _g = hold();
    let up = series_of(4, &[Some(10.0), Some(10.0)]);
    let down = series_of(4, &[Some(50.0), Some(50.0)]);
    let view = |shared: bool| {
        let (up, down) = (up.clone(), down.clone());
        move || {
            time_series()
                .style(PlotStyle::Line)
                .domain(YDomain::auto_from_zero())
                .shared_domain(shared)
                .mirrored(
                    (up.clone(), SeriesStyle::color(RED)),
                    (down.clone(), SeriesStyle::color(BLUE)),
                )
        }
    };
    // Own domains: each half's top is its own data's nice max, so both reach their edge.
    let mut own = rig(view(false));
    let strokes = own.paint(342.0, 56.0).strokes();
    assert!(near(strokes[0].0[0].y, 0.0) && near(strokes[1].0[0].y, 56.0));
    // Shared: 0..50, so the upper 10 sits at a fifth of its half.
    let mut shared = rig(view(true));
    let strokes = shared.paint(342.0, 56.0).strokes();
    assert!(near(strokes[0].0[0].y, 28.0 - 28.0 * 0.2), "{strokes:?}");
    assert!(near(strokes[1].0[0].y, 56.0), "{strokes:?}");
}

#[test]
fn an_auto_domain_uses_the_nice_max_and_a_fixed_one_clamps() {
    let _g = hold();
    let s = series_of(4, &[Some(10.0), Some(42.0)]);
    assert_eq!(auto_domain(&[&s], YDomain::auto_from_zero()), (0.0, 50.0));
    let other = series_of(4, &[Some(70.0)]);
    assert_eq!(
        auto_domain(&[&s, &other], YDomain::auto_from_zero()),
        (0.0, 100.0)
    );
    assert_eq!(auto_domain(&[], YDomain::percent()), (0.0, 100.0));

    let mut auto = rig(|| {
        time_series()
            .style(PlotStyle::Line)
            .domain(YDomain::auto_from_zero())
            .series(s.clone(), SeriesStyle::color(RED))
    });
    let points = auto.paint(342.0, 56.0).strokes()[0].0.clone();
    assert!(near(points[1].y, 56.0 - 56.0 * 42.0 / 50.0), "{points:?}");

    let mut fixed = rig(|| {
        time_series()
            .style(PlotStyle::Line)
            .domain(YDomain::fixed(0.0, 20.0))
            .series(s.clone(), SeriesStyle::color(RED))
    });
    let points = fixed.paint(342.0, 56.0).strokes()[0].0.clone();
    assert!(
        near(points[0].y, 28.0) && near(points[1].y, 0.0),
        "{points:?}"
    );
}

#[test]
fn grid_rules_land_on_whole_pixels() {
    let _g = hold();
    let s = Series::new(8);
    let grey = Color::from_rgb8(0x80, 0x80, 0x80);
    let mut rig = rig(|| {
        time_series()
            .grid([0.0, 0.25, 0.5, 1.0])
            .grid_color(grey)
            .series(s.clone(), SeriesStyle::color(RED))
    });
    let rules = rig.paint(342.0, 56.0).rects();
    assert_eq!(rules.len(), 4);
    for (rule, color) in &rules {
        assert_eq!(*color, grey);
        assert_eq!(rule.height(), 1.0);
        assert_eq!(rule.y0.fract(), 0.0);
        assert!(rule.y0 >= 0.0 && rule.y1 <= 56.0);
        assert_eq!((rule.x0, rule.x1), (0.0, 342.0));
    }
    let ys: Vec<f64> = rules.iter().map(|(r, _)| r.y0).collect();
    assert_eq!(ys, [0.0, 28.0, 42.0, 55.0]);
}

#[test]
fn everything_painted_stays_in_bounds_under_one_clip() {
    let _g = hold();
    let s = series_of(
        20,
        &[
            Some(-50.0),
            Some(100.0),
            Some(250.0),
            None,
            Some(0.0),
            Some(99.0),
        ],
    );
    for style in [
        PlotStyle::Area,
        PlotStyle::Line,
        PlotStyle::Blocks {
            column_width: 5.0,
            gap: 2.0,
        },
    ] {
        let mut rig = rig(|| {
            time_series()
                .style(style)
                .grid([0.0, 0.5, 1.0])
                .series(s.clone(), SeriesStyle::color(RED).stroke_width(3.0))
        });
        let rec = rig.paint(200.0, 40.0);
        assert!(!rec.ops.is_empty());
        assert_contained(&rec, Size::new(200.0, 40.0));
    }
}

#[test]
fn a_push_shows_on_the_next_paint_without_a_rebuild() {
    let _g = hold();
    let s = series_of(4, &[Some(10.0), Some(20.0)]);
    let mut rig = rig(|| {
        time_series()
            .style(PlotStyle::Line)
            .series(s.clone(), SeriesStyle::color(RED))
    });
    let before = rig.paint(342.0, 56.0).strokes()[0].0.clone();
    let again = rig.paint(342.0, 56.0).strokes()[0].0.clone();
    assert_eq!(before, again);

    s.push(100.0);
    let after = rig.paint(342.0, 56.0).strokes()[0].0.clone();
    assert_eq!(after.len(), before.len() + 1);
    let newest = after.last().unwrap();
    assert_eq!((newest.x, newest.y), (342.0, 0.0));

    // A resize rebuilds the cached geometry for the new plot.
    let wide = rig.paint(534.0, 130.0).strokes()[0].0.clone();
    assert_eq!(wide.last().unwrap().x, 534.0);
}

#[test]
fn rebuild_flags_follow_what_changed() {
    let _g = hold();
    let s = Series::new(8);
    let base = || {
        time_series()
            .series(s.clone(), SeriesStyle::color(RED))
            .label(Corner::TopLeft, "100")
            .height(56.0)
    };
    let mut rig = rig(base);
    rig.paint(342.0, 56.0);
    assert_eq!(rig.rebuild(base), ChangeFlags::NONE);

    let paint_only: [&dyn Fn() -> View; 7] = [
        &|| base().series(s.clone(), SeriesStyle::color(BLUE)),
        &|| {
            time_series()
                .series(Series::new(8), SeriesStyle::color(RED))
                .label(Corner::TopLeft, "100")
                .height(56.0)
        },
        &|| {
            time_series()
                .series(s.clone(), SeriesStyle::color(BLUE))
                .label(Corner::TopLeft, "100")
                .height(56.0)
        },
        &|| base().style(PlotStyle::Line),
        &|| base().grid([0.5]),
        &|| base().grid_color(BLUE),
        &|| base().domain(YDomain::auto_from_zero()),
    ];
    for next in paint_only {
        let flags = rig.rebuild(next);
        assert_eq!(flags, ChangeFlags::PAINT);
        rig.rebuild(base);
    }

    let layout: [&dyn Fn() -> View; 3] = [
        &|| base().height(80.0),
        &|| base().label(Corner::TopLeft, "50"),
        &|| base().label_style(12.0, BLUE),
    ];
    for next in layout {
        let flags = rig.rebuild(next);
        assert_eq!(flags, ChangeFlags::LAYOUT | ChangeFlags::PAINT);
        rig.rebuild(base);
    }
}

#[test]
fn input_is_ignored() {
    let _g = hold();
    let s = series_of(4, &[Some(10.0), Some(20.0)]);
    let mut rig = rig(|| time_series().series(s.clone(), SeriesStyle::color(RED)));
    rig.paint(342.0, 56.0);
    for phase in [PointerPhase::Down, PointerPhase::Up] {
        let event = InputEvent::Pointer(PointerEvent {
            phase,
            position: Point::new(100.0, 20.0),
            button: PointerButton::Primary,
        });
        let outcome: EventOutcome = rig.root.event(&mut (), &event);
        assert!(!outcome.handled);
    }
}

#[test]
fn semantics_is_one_image_node_with_label_and_value() {
    let _g = hold();
    let s = Series::new(8);
    let images = |rig: &Rig| {
        rig.root
            .semantics()
            .nodes
            .into_iter()
            .filter(|(_, n)| n.role() == Role::Image)
            .map(|(_, n)| {
                (
                    n.label().map(str::to_owned),
                    n.description().map(str::to_owned),
                )
            })
            .collect::<Vec<_>>()
    };
    let mut plain = rig(|| time_series().series(s.clone(), SeriesStyle::color(RED)));
    plain.paint(342.0, 56.0);
    assert!(images(&plain).is_empty());

    let mut labelled = rig(|| {
        time_series()
            .series(s.clone(), SeriesStyle::color(RED))
            .semantics("CPU load")
            .semantic_value("42 %")
    });
    labelled.paint(342.0, 56.0);
    assert_eq!(
        images(&labelled),
        [(Some("CPU load".to_owned()), Some("42 %".to_owned()))]
    );
}

#[test]
fn colours_come_from_the_caller_then_the_theme_then_the_fallback() {
    let _g = hold();
    let s = series_of(4, &[Some(10.0), Some(20.0)]);
    let stroke_color = |rig: &mut Rig| solid(&rig.paint(342.0, 56.0).strokes()[0].2);

    let mut unthemed = rig(|| time_series().series(s.clone(), SeriesStyle::new()));
    assert_eq!(stroke_color(&mut unthemed), Tone::FALLBACK_CALM);

    let theme = Theme::neutral();
    let primary = theme.scheme().primary;
    let outline = theme.scheme().outline_variant;
    let mut themed = Rig::new();
    themed.root.set_theme(Box::new(theme));
    themed.rebuild(|| {
        time_series()
            .grid([0.5])
            .series(s.clone(), SeriesStyle::new())
    });
    let rec = themed.paint(342.0, 56.0);
    assert_eq!(solid(&rec.strokes()[0].2), primary);
    assert_eq!(rec.rects()[0].1, outline);

    let mut explicit = Rig::new();
    explicit.root.set_theme(Box::new(Theme::neutral()));
    explicit.rebuild(|| time_series().series(s.clone(), SeriesStyle::color(RED)));
    assert_eq!(stroke_color(&mut explicit), RED);
}

#[test]
fn labels_are_shaped_in_layout_and_drawn_inside_the_plot() {
    let _g = hold();
    let s = series_of(4, &[Some(10.0), Some(20.0)]);
    let mut rig = rig(|| {
        time_series()
            .series(s.clone(), SeriesStyle::color(RED))
            .label(Corner::TopLeft, "100 %")
            .label(Corner::BottomRight, "60 s")
            .label_style(11.0, BLUE)
    });
    let rec = rig.paint(342.0, 56.0);
    assert!(rec.glyph_runs() >= 2, "{}", rec.glyph_runs());
    assert_contained(&rec, Size::new(342.0, 56.0));
    // Labels draw last, over the series.
    let (last, _) = rec.ops.last().unwrap();
    assert!(matches!(last, Op::Glyphs(_)));
    // A repaint after a push reuses the shaped labels.
    s.push(30.0);
    assert_eq!(rig.paint(342.0, 56.0).glyph_runs(), rec.glyph_runs());
}

#[test]
fn an_empty_or_all_gap_series_draws_only_grid_and_labels() {
    let _g = hold();
    for s in [Series::new(8), series_of(8, &[None, None, None])] {
        for style in [
            PlotStyle::Area,
            PlotStyle::Line,
            PlotStyle::Blocks {
                column_width: 4.0,
                gap: 1.0,
            },
        ] {
            let mut rig = rig(|| {
                time_series()
                    .style(style)
                    .domain(YDomain::auto_from_zero())
                    .grid([0.5])
                    .label(Corner::TopRight, "0")
                    .series(s.clone(), SeriesStyle::color(RED))
            });
            let rec = rig.paint(342.0, 56.0);
            assert!(rec.fills().is_empty() && rec.strokes().is_empty());
            assert_eq!(rec.rects().len(), 1);
            assert!(rec.glyph_runs() >= 1);
        }
    }
}

#[test]
fn reference_sizes_paint_within_bounds() {
    let _g = hold();
    let a = series_of(
        60,
        &(0..60)
            .map(|i| (i % 7 != 3).then_some((i as f32 * 13.0) % 100.0))
            .collect::<Vec<_>>(),
    );
    let b = series_of(30, &[Some(5.0), Some(95.0), None, Some(40.0)]);
    for (w, h) in [(342.0, 56.0), (534.0, 130.0), (362.0, 44.0)] {
        for style in [
            PlotStyle::Area,
            PlotStyle::Line,
            PlotStyle::Blocks {
                column_width: 3.0,
                gap: 1.0,
            },
        ] {
            let views: [&dyn Fn() -> View; 2] = [
                &|| {
                    time_series()
                        .style(style)
                        .grid([0.25, 0.5, 0.75])
                        .label(Corner::TopLeft, "100")
                        .label(Corner::BottomLeft, "0")
                        .series(a.clone(), SeriesStyle::color(RED))
                        .series(b.clone(), SeriesStyle::color(BLUE))
                },
                &|| {
                    time_series()
                        .style(style)
                        .domain(YDomain::auto_from_zero())
                        .grid([0.5])
                        .mirrored(
                            (a.clone(), SeriesStyle::color(RED)),
                            (b.clone(), SeriesStyle::color(BLUE)),
                        )
                },
            ];
            for view in views {
                let mut rig = rig(view);
                let rec = rig.paint(w, h);
                assert!(!rec.ops.is_empty());
                assert_contained(&rec, Size::new(w, h));
            }
        }
    }
}

#[test]
fn layout_fills_the_width_and_defaults_the_height() {
    let _g = hold();
    let s = Series::new(4);
    let mut fixed = rig(|| {
        time_series()
            .height(40.0)
            .series(s.clone(), SeriesStyle::color(RED))
            .grid([0.0])
    });
    let rule = fixed.paint(342.0, 300.0).rects()[0].0;
    assert_eq!((rule.x1, rule.y1), (342.0, 40.0));

    let mut fill = rig(|| {
        time_series()
            .series(s.clone(), SeriesStyle::color(RED))
            .grid([0.0])
    });
    let rule = fill.paint(342.0, 130.0).rects()[0].0;
    assert_eq!(rule.y1, 130.0);
}
