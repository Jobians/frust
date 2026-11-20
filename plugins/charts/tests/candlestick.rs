//! `candlestick_chart` headless: a real `RenderRoot` painting into a recording scene.
//!
//! Writing a `Candles` requests a process-wide repaint, so every test holds `STATICS`.

use std::any::Any;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use frust::authoring::scene::GlyphRun;
use frust::authoring::text::TextContext;
use frust::authoring::{
    BezPath, Brush, ChangeFlags, EventOutcome, InputEvent, PaintScene, Point, PointerButton,
    PointerEvent, PointerPhase, Rect, Role, Size,
};
use frust::{Color, FrameTime, Theme};
use frust_charts::{Candle, Candles, CandlestickView, Tone, YDomain, candlestick_chart};
use frust_core::{RenderRoot, repaint_generation};
use frust_reactive::ReactiveRuntime;

static STATICS: Mutex<()> = Mutex::new(());

fn hold() -> MutexGuard<'static, ()> {
    STATICS.lock().unwrap_or_else(PoisonError::into_inner)
}

const UP: Color = Color::from_rgb8(0x10, 0xa0, 0x20);
const DOWN: Color = Color::from_rgb8(0xd0, 0x10, 0x10);
const GRID: Color = Color::from_rgb8(0x20, 0x20, 0xd0);

#[derive(Default)]
struct Rec {
    rects: Vec<(Rect, Color)>,
    clips: Vec<Rect>,
    clip_depth: usize,
    other_draws: usize,
}

impl PaintScene for Rec {
    fn fill_rect(&mut self, origin: Point, size: Size, color: Color) {
        self.rects
            .push((Rect::from_origin_size(origin, size), color));
    }
    fn fill_rounded_rect(&mut self, _: Point, _: Size, _: f64, _: Color) {
        self.other_draws += 1;
    }
    fn fill_path(&mut self, _: Point, _: &BezPath, _: &Brush) {
        self.other_draws += 1;
    }
    fn stroke_path(&mut self, _: Point, _: &BezPath, _: f64, _: &Brush) {
        self.other_draws += 1;
    }
    fn push_clip(&mut self, origin: Point, size: Size) {
        self.clips.push(Rect::from_origin_size(origin, size));
        self.clip_depth += 1;
    }
    fn pop_clip(&mut self) {
        self.clip_depth -= 1;
    }
    fn draw_glyph_run(&mut self, _run: GlyphRun) {
        self.other_draws += 1;
    }
    fn draw_text(&mut self, _origin: Point, _text: &str) {
        self.other_draws += 1;
    }
}

type View = CandlestickView;

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

fn candles_of(capacity: usize, slots: &[Option<Candle>]) -> Candles {
    let candles = Candles::new(capacity);
    candles.extend(slots.iter().copied());
    candles
}

fn c(open: f32, high: f32, low: f32, close: f32) -> Option<Candle> {
    Some(Candle::new(open, high, low, close))
}

/// A chart over a fixed `0..100` domain, so a value `v` sits at y = 100 - v on a 100 px plot.
fn chart(candles: &Candles) -> View {
    candlestick_chart(candles.clone())
        .domain(YDomain::fixed(0.0, 100.0))
        .up(UP)
        .down(DOWN)
}

fn repaints(f: impl FnOnce()) -> u64 {
    let before = repaint_generation();
    f();
    repaint_generation() - before
}

#[test]
fn body_and_wick_rects_for_a_known_candle() {
    let _g = hold();
    let s = candles_of(20, &[c(20.0, 90.0, 10.0, 60.0)]);
    let mut rig = rig(|| chart(&s));
    let rec = rig.paint(100.0, 100.0);
    // Wick first, then body; newest right-aligned in a 6 px body, 1 px wick centred by
    // whole pixels.
    assert_eq!(
        rec.rects,
        [
            (Rect::new(96.0, 10.0, 97.0, 90.0), UP),
            (Rect::new(94.0, 40.0, 100.0, 80.0), UP),
        ]
    );
}

#[test]
fn widths_and_gaps_are_whole_pixels_and_candles_step_by_their_pitch() {
    let _g = hold();
    let s = candles_of(20, &[c(10.0, 30.0, 5.0, 20.0), c(20.0, 40.0, 15.0, 30.0)]);
    let mut rig = rig(|| chart(&s).body_width(4.4).gap(2.6).wick_width(2.0));
    let rec = rig.paint(100.0, 100.0);
    // Body 4, gap 3, pitch 7; wick 2 wide, offset 1.
    let bodies: Vec<Rect> = rec.rects.iter().skip(1).step_by(2).map(|r| r.0).collect();
    assert_eq!(bodies[1].x0, 96.0);
    assert_eq!(bodies[1].x1, 100.0);
    assert_eq!(bodies[0].x0, 89.0);
    assert_eq!(bodies[0].x1, 93.0);
    let wicks: Vec<Rect> = rec.rects.iter().step_by(2).map(|r| r.0).collect();
    assert_eq!((wicks[1].x0, wicks[1].x1), (97.0, 99.0));
    for r in rec.rects.iter().map(|r| r.0) {
        for v in [r.x0, r.y0, r.x1, r.y1] {
            assert_eq!(v, v.round(), "{r:?} is not on whole pixels");
        }
    }
}

#[test]
fn up_and_down_colours_follow_close_against_open() {
    let _g = hold();
    let s = candles_of(
        20,
        &[
            c(30.0, 80.0, 20.0, 50.0),
            c(50.0, 80.0, 20.0, 30.0),
            c(40.0, 80.0, 20.0, 40.0),
        ],
    );
    let mut rig = rig(|| chart(&s));
    let rec = rig.paint(100.0, 100.0);
    let colours: Vec<Color> = rec.rects.iter().step_by(2).map(|r| r.1).collect();
    // Oldest first: up, down, then open == close counts as up.
    assert_eq!(colours, [UP, DOWN, UP]);
}

#[test]
fn a_doji_body_is_one_pixel_tall() {
    let _g = hold();
    let s = candles_of(20, &[c(50.0, 70.0, 30.0, 50.0)]);
    let mut rig = rig(|| chart(&s));
    let rec = rig.paint(100.0, 100.0);
    let body = rec.rects[1].0;
    assert_eq!(body, Rect::new(94.0, 50.0, 100.0, 51.0));
    let wick = rec.rects[0].0;
    assert_eq!((wick.y0, wick.y1), (30.0, 70.0));
}

#[test]
fn gaps_leave_their_place_empty() {
    let _g = hold();
    let s = candles_of(
        20,
        &[c(10.0, 30.0, 5.0, 20.0), None, c(20.0, 40.0, 15.0, 30.0)],
    );
    let mut rig = rig(|| chart(&s));
    let rec = rig.paint(100.0, 100.0);
    assert_eq!(rec.rects.len(), 4);
    assert_eq!(rec.rects[1].0.x0, 78.0);
    assert_eq!(rec.rects[3].0.x0, 94.0);
}

#[test]
fn an_invalid_candle_is_stored_as_a_gap() {
    let _g = hold();
    let s = Candles::new(8);
    s.push(Candle::new(1.0, f32::NAN, 0.0, 1.0));
    s.push(Candle::new(1.0, 0.5, 2.0, 1.0));
    s.push(Candle::new(f32::INFINITY, 2.0, 0.0, 1.0));
    s.push(Candle::new(1.0, 2.0, 0.0, 1.5));
    s.with_slots(|slots| {
        let all: Vec<_> = slots.iter().collect();
        assert_eq!(all, [None, None, None, c(1.0, 2.0, 0.0, 1.5)]);
    });
    assert_eq!((s.min_low(), s.max_high()), (Some(0.0), Some(2.0)));
}

#[test]
fn the_oldest_candles_are_dropped_not_clipped() {
    let _g = hold();
    let s = Candles::new(5);
    for i in 0..5 {
        let base = 10.0 + 10.0 * i as f32;
        s.push(Candle::new(base, base + 5.0, base - 5.0, base + 2.0));
    }
    let mut rig = rig(|| chart(&s));
    // Pitch 8: bodies start at x 14 and 6; the next would start at -2.
    let rec = rig.paint(20.0, 100.0);
    assert_eq!(rec.rects.len(), 4);
    assert_eq!(rec.rects[1].0.x0, 6.0);
    assert_eq!(rec.rects[3].0.x0, 14.0);
    // The newest two are shown: the last has base 50, close 52.
    assert_eq!(rec.rects[3].0.y0, 48.0);
}

#[test]
fn update_last_repaints_changing_only_the_last_candle() {
    let _g = hold();
    let s = candles_of(
        20,
        &[
            c(10.0, 30.0, 5.0, 20.0),
            c(20.0, 40.0, 15.0, 30.0),
            c(30.0, 50.0, 25.0, 40.0),
        ],
    );
    let mut rig = rig(|| chart(&s));
    let before = rig.paint(100.0, 100.0);
    assert_eq!(
        repaints(|| s.update_last(Candle::new(30.0, 60.0, 25.0, 55.0))),
        1
    );
    // No rebuild: the same root paints the handle's new content.
    let after = rig.paint(100.0, 100.0);
    assert_eq!(before.rects.len(), after.rects.len());
    assert_eq!(before.rects[..4], after.rects[..4]);
    assert_ne!(before.rects[4..], after.rects[4..]);
    assert_eq!(after.rects[5].0, Rect::new(94.0, 45.0, 100.0, 70.0));
}

#[test]
fn update_last_on_an_empty_ring_pushes_and_a_no_op_does_nothing() {
    let _g = hold();
    let s = Candles::new(4);
    let candle = Candle::new(1.0, 3.0, 0.0, 2.0);
    assert_eq!(repaints(|| s.update_last(candle)), 1);
    assert_eq!(s.len(), 1);
    let revision = s.revision();
    assert_eq!(repaints(|| s.update_last(candle)), 0);
    assert_eq!(s.revision(), revision);
    assert_eq!(repaints(|| s.replace([Some(candle)])), 0);
    assert_eq!(repaints(|| s.extend([])), 0);
    assert_eq!(repaints(|| s.clear()), 1);
    assert_eq!(repaints(|| s.clear()), 0);
    assert!(s.latest().is_none());
}

#[test]
fn the_ring_wraps_and_reports_extremes() {
    let _g = hold();
    let s = Candles::new(3);
    for i in 0..5 {
        let v = i as f32;
        s.push(Candle::new(v, v + 2.0, v - 1.0, v + 1.0));
    }
    assert_eq!(s.len(), 3);
    assert_eq!((s.min_low(), s.max_high()), (Some(1.0), Some(6.0)));
    s.with_slots(|slots| {
        assert_eq!(slots.len(), 3);
        assert_eq!(slots.get(0), Some(c(2.0, 4.0, 1.0, 3.0)));
        assert_eq!(slots.get(3), None);
    });
    assert!(s.ptr_eq(&s.clone()));
    assert!(!s.ptr_eq(&Candles::new(3)));
}

#[test]
fn the_automatic_domain_is_not_anchored_at_zero() {
    let _g = hold();
    let s = candles_of(
        20,
        &[c(102.0, 108.0, 100.0, 106.0), c(106.0, 110.0, 101.0, 103.0)],
    );
    let mut rig = rig(|| candlestick_chart(s.clone()).up(UP).down(DOWN));
    let rec = rig.paint(100.0, 100.0);
    let wicks: Vec<Rect> = rec.rects.iter().step_by(2).map(|r| r.0).collect();
    // The data fills the height: a zero-anchored domain would squeeze it into the top 10 %.
    let top = wicks.iter().map(|r| r.y0).fold(f64::MAX, f64::min);
    let bottom = wicks.iter().map(|r| r.y1).fold(f64::MIN, f64::max);
    assert!(top <= 5.0 && bottom >= 90.0, "{top} {bottom}");
}

#[test]
fn every_draw_lies_within_the_plot() {
    let _g = hold();
    let s = candles_of(
        20,
        &[
            c(0.0, 0.0, 0.0, 0.0),
            c(100.0, 100.0, 100.0, 100.0),
            c(-50.0, 500.0, -80.0, 300.0),
            c(20.0, 90.0, 10.0, 60.0),
        ],
    );
    let mut rig = rig(|| chart(&s).grid([0.0, 0.5, 1.0]));
    let rec = rig.paint(100.0, 100.0);
    let bounds = Rect::new(0.0, 0.0, 100.0, 100.0);
    assert_eq!(rec.clips, [bounds]);
    assert_eq!(rec.other_draws, 0, "the chart draws rectangles only");
    for (r, _) in &rec.rects {
        assert!(
            r.x0 >= 0.0 && r.y0 >= 0.0 && r.x1 <= 100.0 && r.y1 <= 100.0,
            "{r:?}"
        );
        assert!(r.width() >= 1.0 && r.height() >= 1.0, "{r:?}");
    }
}

#[test]
fn grid_rules_sit_under_the_candles() {
    let _g = hold();
    let s = candles_of(20, &[c(20.0, 90.0, 10.0, 60.0)]);
    let mut rig = rig(|| chart(&s).grid([0.5]).grid_color(GRID));
    let rec = rig.paint(100.0, 100.0);
    assert_eq!(rec.rects.len(), 3);
    assert_eq!(rec.rects[0], (Rect::new(0.0, 50.0, 100.0, 51.0), GRID));
}

#[test]
fn colours_come_from_the_caller_then_the_theme_then_the_fallback() {
    let _g = hold();
    let s = candles_of(20, &[c(30.0, 80.0, 20.0, 50.0), c(50.0, 80.0, 20.0, 30.0)]);
    let colours = |rig: &mut Rig| -> Vec<Color> {
        rig.paint(100.0, 100.0)
            .rects
            .iter()
            .step_by(2)
            .map(|r| r.1)
            .collect()
    };
    let plain = || candlestick_chart(s.clone()).domain(YDomain::fixed(0.0, 100.0));
    let mut unthemed = rig(plain);
    assert_eq!(
        colours(&mut unthemed),
        [Tone::FALLBACK_CALM, Tone::FALLBACK_CRITICAL]
    );

    let theme = Theme::neutral();
    let (tertiary, error) = (theme.scheme().tertiary, theme.scheme().error);
    let mut themed = Rig::new();
    themed.root.set_theme(Box::new(theme));
    themed.rebuild(plain);
    assert_eq!(colours(&mut themed), [tertiary, error]);

    let mut explicit = Rig::new();
    explicit.root.set_theme(Box::new(Theme::neutral()));
    explicit.rebuild(|| plain().up(UP).down(DOWN));
    assert_eq!(colours(&mut explicit), [UP, DOWN]);
}

#[test]
fn semantics_is_an_image_node_only_when_labelled() {
    let _g = hold();
    let s = Candles::new(8);
    let images = |rig: &Rig| {
        rig.root
            .semantics()
            .nodes
            .into_iter()
            .filter(|(_, n)| n.role() == Role::Image)
            .map(|(_, n)| n.label().map(str::to_owned))
            .collect::<Vec<_>>()
    };
    let mut plain = rig(|| chart(&s));
    plain.paint(100.0, 100.0);
    assert!(images(&plain).is_empty());
    let mut labelled = rig(|| chart(&s).semantics("BTC daily"));
    labelled.paint(100.0, 100.0);
    assert_eq!(images(&labelled), [Some("BTC daily".to_owned())]);
}

#[test]
fn a_rebuild_with_new_options_repaints_and_the_same_view_does_not() {
    let _g = hold();
    let s = candles_of(20, &[c(20.0, 90.0, 10.0, 60.0)]);
    let mut rig = rig(|| chart(&s));
    rig.paint(100.0, 100.0);
    assert_eq!(rig.rebuild(|| chart(&s)), ChangeFlags::NONE);
    let changed = rig.rebuild(|| chart(&s).body_width(8.0));
    assert!(changed.contains(ChangeFlags::PAINT));
    let rec = rig.paint(100.0, 100.0);
    assert_eq!(rec.rects[1].0, Rect::new(92.0, 40.0, 100.0, 80.0));
    let taller = rig.rebuild(|| chart(&s).body_width(8.0).height(50.0));
    assert!(taller.contains(ChangeFlags::LAYOUT));
}

#[test]
fn input_is_ignored() {
    let _g = hold();
    let s = candles_of(20, &[c(20.0, 90.0, 10.0, 60.0)]);
    let mut rig = rig(|| chart(&s));
    rig.paint(100.0, 100.0);
    for phase in [PointerPhase::Down, PointerPhase::Up] {
        let event = InputEvent::Pointer(PointerEvent {
            phase,
            position: Point::new(97.0, 50.0),
            button: PointerButton::Primary,
        });
        let outcome: EventOutcome = rig.root.event(&mut (), &event);
        assert!(!outcome.handled);
    }
}
