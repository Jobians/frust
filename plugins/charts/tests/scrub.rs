//! `time_series` scrubbing headless: pointer events through a real `RenderRoot`, painting into
//! a recording scene.
//!
//! Writing a `Series` requests a process-wide repaint, so every test holds `STATICS`.

use std::any::Any;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use frust::authoring::scene::GlyphRun;
use frust::authoring::text::TextContext;
use frust::authoring::{
    BezPath, Brush, EventOutcome, InputEvent, PaintScene, Point, PointerButton, PointerEvent,
    PointerPhase, Rect, Shape, Size, View,
};
use frust::{Color, FrameTime};
use frust_charts::{
    ScrubEvent, ScrubSample, Series, SeriesStyle, TimeSeriesView, TimedSeries, YAxis, time_series,
};
use frust_core::RenderRoot;
use frust_reactive::ReactiveRuntime;
use frust_widgets::{Column, ScrollView, SizedBox, scroll_view};

static STATICS: Mutex<()> = Mutex::new(());

fn hold() -> MutexGuard<'static, ()> {
    STATICS.lock().unwrap_or_else(PoisonError::into_inner)
}

const RED: Color = Color::from_rgb8(0xd0, 0x10, 0x10);
const BLUE: Color = Color::from_rgb8(0x10, 0x20, 0xd0);
const GREEN: Color = Color::from_rgb8(0x10, 0xa0, 0x30);
const WIDTH: f64 = 300.0;
const HEIGHT: f64 = 60.0;

/// What the app saw: every `on_scrub` call in order.
#[derive(Default)]
struct Log {
    events: Vec<Option<ScrubEvent>>,
}

/// The rects and solid fills a paint issued.
#[derive(Default)]
struct Rec {
    rects: Vec<(Rect, Color)>,
    fills: Vec<(Rect, Color)>,
}

impl Rec {
    /// The crosshair: a rect in the crosshair colour.
    fn crosshairs(&self) -> Vec<Rect> {
        self.rects
            .iter()
            .filter(|(_, c)| *c == GREEN)
            .map(|(r, _)| *r)
            .collect()
    }

    /// Marker dots: solid fills wider than the 3 px isolated-sample dots.
    fn markers(&self) -> Vec<(Rect, Color)> {
        self.fills
            .iter()
            .filter(|(r, _)| r.width() > 5.0)
            .copied()
            .collect()
    }
}

impl PaintScene for Rec {
    fn fill_rect(&mut self, origin: Point, size: Size, color: Color) {
        self.rects
            .push((Rect::from_origin_size(origin, size), color));
    }
    fn fill_rounded_rect(&mut self, _origin: Point, _size: Size, _radius: f64, _color: Color) {}
    fn fill_path(&mut self, origin: Point, path: &BezPath, brush: &Brush) {
        if let Brush::Solid(color) = brush {
            self.fills
                .push((path.bounding_box() + origin.to_vec2(), *color));
        }
    }
    fn stroke_path(&mut self, _origin: Point, _path: &BezPath, _width: f64, _brush: &Brush) {}
    fn push_clip(&mut self, _origin: Point, _size: Size) {}
    fn pop_clip(&mut self) {}
    fn draw_glyph_run(&mut self, _run: GlyphRun) {}
    fn draw_text(&mut self, _origin: Point, _text: &str) {}
}

struct Rig<V: View<Log>> {
    rt: &'static ReactiveRuntime,
    root: RenderRoot<Log, V>,
    text: TextContext,
    log: Log,
    view: Box<dyn Fn() -> V>,
}

impl<V: View<Log>> Rig<V> {
    fn new(view: impl Fn() -> V + 'static) -> Self {
        let mut rig = Self {
            rt: ReactiveRuntime::init(Arc::new(|| {})),
            root: RenderRoot::new(),
            text: TextContext::new(),
            log: Log::default(),
            view: Box::new(view),
        };
        rig.rebuild();
        rig.paint();
        rig
    }

    fn rebuild(&mut self) {
        let view = &self.view;
        let mut logic = |_: &mut Log| view();
        let (root, log) = (&mut self.root, &mut self.log);
        self.rt.with_owner(|| root.rebuild(&mut logic, log));
    }

    fn paint(&mut self) -> Rec {
        self.root
            .layout_with_text(Size::new(WIDTH, HEIGHT), &mut self.text as &mut dyn Any);
        let mut rec = Rec::default();
        self.root
            .paint(&mut rec, FrameTime::from_nanos(1_000_000_000));
        rec
    }

    /// Deliver one pointer event, with no rebuild after it.
    fn event(&mut self, phase: PointerPhase, x: f64, y: f64) -> EventOutcome {
        self.press(phase, x, y, PointerButton::Primary)
    }

    fn press(
        &mut self,
        phase: PointerPhase,
        x: f64,
        y: f64,
        button: PointerButton,
    ) -> EventOutcome {
        let event = InputEvent::Pointer(PointerEvent {
            phase,
            position: Point::new(x, y),
            button,
        });
        self.root.event(&mut self.log, &event)
    }

    /// One frame as a shell runs it: the event, then a rebuild.
    fn frame(&mut self, phase: PointerPhase, x: f64, y: f64) -> EventOutcome {
        let outcome = self.event(phase, x, y);
        self.rebuild();
        outcome
    }

    fn samples(&self) -> Vec<Option<ScrubSample>> {
        self.log
            .events
            .iter()
            .map(|e| e.as_ref().and_then(|e| e.sample))
            .collect()
    }
}

type Chart = TimeSeriesView<Log>;

fn series_of(capacity: usize, slots: &[Option<f32>]) -> Series {
    let series = Series::new(capacity);
    series.extend(slots.iter().copied());
    series
}

fn record(log: &mut Log, event: Option<ScrubEvent>) {
    log.events.push(event);
}

/// A chart over `series` that logs every scrub and draws a green crosshair 2 px wide.
fn scrubbing(series: &Series) -> impl Fn() -> Chart + 'static {
    let series = series.clone();
    move || {
        time_series()
            .series(series.clone(), SeriesStyle::color(RED))
            .crosshair(GREEN, 2.0)
            .on_scrub(record)
    }
}

#[test]
fn without_on_scrub_every_phase_is_ignored() {
    let _g = hold();
    let s = series_of(4, &[Some(10.0), Some(20.0)]);
    let mut rig =
        Rig::new(move || -> Chart { time_series().series(s.clone(), SeriesStyle::color(RED)) });
    for phase in [
        PointerPhase::Down,
        PointerPhase::Move,
        PointerPhase::Up,
        PointerPhase::Down,
        PointerPhase::Cancel,
    ] {
        assert!(!rig.frame(phase, 250.0, 20.0).handled, "{phase:?}");
    }
    assert!(rig.paint().crosshairs().is_empty());
    assert!(rig.log.events.is_empty());
}

#[test]
fn down_move_up_report_some_some_none() {
    let _g = hold();
    // Capacity 4 over 300 px: slots at x 0, 100, 200 and 300; slot 2 is a gap.
    let s = series_of(4, &[Some(10.0), Some(20.0), None, Some(40.0)]);
    let mut rig = Rig::new(scrubbing(&s));
    assert!(rig.frame(PointerPhase::Down, 95.0, 20.0).handled);
    assert!(rig.frame(PointerPhase::Move, 290.0, 20.0).handled);
    // On the gap, both neighbours are 100 px away: no sample.
    assert!(rig.frame(PointerPhase::Move, 200.0, 20.0).handled);
    // Off the plot's height the drag still scrubs by x.
    assert!(rig.frame(PointerPhase::Move, 10.0, 500.0).handled);
    assert!(rig.frame(PointerPhase::Up, 10.0, 500.0).handled);

    assert_eq!(rig.log.events.len(), 5, "one callback per event");
    assert_eq!(
        rig.samples(),
        [
            Some(ScrubSample::Index(1)),
            Some(ScrubSample::Index(3)),
            None,
            Some(ScrubSample::Index(0)),
            None,
        ]
    );
    let events: Vec<&ScrubEvent> = rig.log.events.iter().flatten().collect();
    assert_eq!(events.len(), 4);
    assert_eq!(events[0].values, [Some(20.0)]);
    assert_eq!(
        (events[0].x, events[0].position),
        (100.0, Point::new(95.0, 20.0))
    );
    assert_eq!(events[1].values, [Some(40.0)]);
    assert_eq!(
        (events[2].values.as_slice(), events[2].x),
        ([None].as_slice(), 200.0)
    );
    assert_eq!(events[3].values, [Some(10.0)]);
    assert!(rig.log.events[4].is_none(), "the release ends the scrub");
}

#[test]
fn a_dense_series_skips_a_gap_to_the_nearest_present_sample() {
    let _g = hold();
    // 31 slots over 300 px: one every 10 px, slot 15 (x 150) a gap.
    let slots: Vec<Option<f32>> = (0..31).map(|i| (i != 15).then_some(i as f32)).collect();
    let s = series_of(31, &slots);
    let mut rig = Rig::new(scrubbing(&s));
    rig.frame(PointerPhase::Down, 152.0, 20.0);
    rig.frame(PointerPhase::Move, 148.0, 20.0);
    rig.frame(PointerPhase::Move, 171.0, 20.0);
    rig.frame(PointerPhase::Up, 171.0, 20.0);
    assert_eq!(
        rig.samples(),
        [
            Some(ScrubSample::Index(16)),
            Some(ScrubSample::Index(14)),
            Some(ScrubSample::Index(17)),
            None,
        ]
    );
    let values: Vec<_> = rig
        .log
        .events
        .iter()
        .flatten()
        .map(|e| e.values[0])
        .collect();
    assert_eq!(values, [Some(16.0), Some(14.0), Some(17.0)]);
}

#[test]
fn the_crosshair_is_painted_between_down_and_up_only() {
    let _g = hold();
    let s = series_of(4, &[Some(0.0), Some(50.0), Some(100.0), Some(25.0)]);
    let mut rig = Rig::new(scrubbing(&s));
    assert!(rig.paint().crosshairs().is_empty());
    assert!(rig.paint().markers().is_empty());

    rig.frame(PointerPhase::Down, 110.0, 30.0);
    let rec = rig.paint();
    // At the slot at x 100: 2 px wide, the plot's full height.
    assert_eq!(rec.crosshairs(), [Rect::new(99.0, 0.0, 101.0, HEIGHT)]);
    // One red marker on the sample (50 of 0..100: half way up).
    let markers = rec.markers();
    assert_eq!(markers.len(), 1);
    let (dot, color) = markers[0];
    assert_eq!(color, RED);
    assert!((dot.center().x - 100.0).abs() < 1e-6, "{dot:?}");
    assert!((dot.center().y - HEIGHT / 2.0).abs() < 1e-6, "{dot:?}");

    rig.frame(PointerPhase::Move, 195.0, 30.0);
    assert_eq!(
        rig.paint().crosshairs(),
        [Rect::new(199.0, 0.0, 201.0, HEIGHT)]
    );

    rig.frame(PointerPhase::Up, 195.0, 30.0);
    let rec = rig.paint();
    assert!(rec.crosshairs().is_empty());
    assert!(rec.markers().is_empty());
}

#[test]
fn a_cancel_ends_the_scrub_with_none_on_the_following_rebuild() {
    let _g = hold();
    let s = series_of(4, &[Some(10.0), Some(20.0), Some(30.0), Some(40.0)]);
    let mut rig = Rig::new(scrubbing(&s));
    rig.frame(PointerPhase::Down, 100.0, 20.0);
    rig.frame(PointerPhase::Move, 200.0, 20.0);
    assert!(rig.event(PointerPhase::Cancel, 0.0, 0.0).handled);
    // The cancel touched no state, and the crosshair is gone at once.
    assert_eq!(rig.log.events.len(), 2);
    assert!(rig.paint().crosshairs().is_empty());
    // The rebuild that follows delivers the end, once.
    rig.rebuild();
    rig.rebuild();
    assert_eq!(rig.log.events.len(), 3);
    assert!(rig.log.events[2].is_none());
    // Nothing is armed any more: a move or release reports nothing.
    assert!(!rig.frame(PointerPhase::Move, 150.0, 20.0).handled);
    assert!(!rig.frame(PointerPhase::Up, 150.0, 20.0).handled);
    assert_eq!(rig.log.events.len(), 3);
}

#[test]
fn only_a_primary_press_inside_the_plot_starts_a_scrub() {
    let _g = hold();
    let s = series_of(4, &[Some(10.0), Some(20.0), Some(30.0), Some(40.0)]);
    // A hover move with no press is not a scrub.
    let mut rig = Rig::new(scrubbing(&s));
    assert!(!rig.frame(PointerPhase::Move, 100.0, 20.0).handled);
    assert!(
        !rig.press(PointerPhase::Down, 100.0, 20.0, PointerButton::Secondary)
            .handled
    );
    assert!(!rig.frame(PointerPhase::Up, 100.0, 20.0).handled);
    assert!(rig.log.events.is_empty());

    // A press in the axis gutter is outside the plot.
    let axis = s.clone();
    let mut rig = Rig::new(move || -> Chart {
        time_series()
            .series(axis.clone(), SeriesStyle::color(RED))
            .y_axis(YAxis::new().gutter(true))
            .on_scrub(record)
    });
    assert!(!rig.frame(PointerPhase::Down, 2.0, 20.0).handled);
    assert!(rig.log.events.is_empty());
    assert!(rig.frame(PointerPhase::Down, WIDTH, 20.0).handled);
    assert_eq!(rig.samples(), [Some(ScrubSample::Index(3))]);
    // The oldest slot sits at the gutter's edge, not at x 0.
    rig.frame(PointerPhase::Move, 0.0, 20.0);
    let event = rig.log.events[1].as_ref().expect("a scrub");
    assert_eq!(event.sample, Some(ScrubSample::Index(0)));
    assert!(event.x > 2.0, "{event:?}");
}

#[test]
fn a_mirrored_plot_reports_both_series_and_marks_each() {
    let _g = hold();
    let rx = series_of(4, &[Some(10.0), Some(20.0), Some(30.0), Some(40.0)]);
    let tx = series_of(4, &[Some(1.0), None, Some(3.0), Some(4.0)]);
    let (a, b) = (rx.clone(), tx.clone());
    let mut rig = Rig::new(move || -> Chart {
        time_series()
            .mirrored(
                (a.clone(), SeriesStyle::color(RED)),
                (b.clone(), SeriesStyle::color(BLUE)),
            )
            .crosshair(GREEN, 1.0)
            .on_scrub(record)
    });
    rig.frame(PointerPhase::Down, 205.0, 10.0);
    rig.frame(PointerPhase::Move, 100.0, 10.0);
    let values: Vec<_> = rig
        .log
        .events
        .iter()
        .flatten()
        .map(|e| e.values.clone())
        .collect();
    assert_eq!(
        values,
        [vec![Some(30.0), Some(3.0)], vec![Some(20.0), None]]
    );
    // Only the upper series has a sample on the crosshair now.
    let markers = rig.paint().markers();
    assert_eq!(markers.iter().map(|m| m.1).collect::<Vec<_>>(), [RED]);
    // The lower series' marker hangs in the lower half.
    rig.frame(PointerPhase::Move, 205.0, 10.0);
    let markers = rig.paint().markers();
    assert_eq!(markers.len(), 2);
    let blue = markers
        .iter()
        .find(|m| m.1 == BLUE)
        .expect("a lower marker");
    assert!(blue.0.center().y > HEIGHT / 2.0, "{blue:?}");
}

#[test]
fn a_timed_series_reports_the_time_of_its_nearest_point() {
    let _g = hold();
    let timed = TimedSeries::new(8).gap_after(2.0);
    // Span 0..10 s over 300 px: 30 px a second; an outage between 2 s and 10 s.
    timed.extend([(0.0, 1.0), (1.0, 2.0), (2.0, 3.0), (10.0, 4.0)]);
    let t = timed.clone();
    let mut rig = Rig::new(move || -> Chart {
        time_series()
            .series(t.clone(), SeriesStyle::color(RED))
            .on_scrub(record)
    });
    rig.frame(PointerPhase::Down, 40.0, 20.0);
    rig.frame(PointerPhase::Move, 180.0, 20.0);
    rig.frame(PointerPhase::Move, 285.0, 20.0);
    rig.frame(PointerPhase::Up, 285.0, 20.0);
    assert_eq!(
        rig.samples(),
        [
            Some(ScrubSample::Time(1.0)),
            None,
            Some(ScrubSample::Time(10.0)),
            None,
        ]
    );
    assert_eq!(rig.log.events[2].as_ref().map(|e| e.x), Some(300.0));
}

#[test]
fn dropping_on_scrub_mid_drag_clears_the_crosshair() {
    let _g = hold();
    let s = series_of(4, &[Some(10.0), Some(20.0), Some(30.0), Some(40.0)]);
    let scrub = Arc::new(Mutex::new(true));
    let (series, on) = (s.clone(), Arc::clone(&scrub));
    let mut rig = Rig::new(move || -> Chart {
        let chart = time_series()
            .series(series.clone(), SeriesStyle::color(RED))
            .crosshair(GREEN, 1.0);
        if *on.lock().unwrap() {
            chart.on_scrub(record)
        } else {
            chart
        }
    });
    rig.frame(PointerPhase::Down, 100.0, 20.0);
    assert_eq!(rig.paint().crosshairs().len(), 1);
    *scrub.lock().unwrap() = false;
    rig.rebuild();
    assert!(rig.paint().crosshairs().is_empty());
    assert!(!rig.frame(PointerPhase::Up, 100.0, 20.0).handled);
    assert_eq!(rig.log.events.len(), 1);
}

/// A chart 80 px tall at the top of a vertical scroll view whose content overflows it.
fn in_scroll_view(series: &Series) -> impl Fn() -> ScrollView<Log> + 'static {
    let chart = scrubbing(series);
    move || scroll_view(Column((chart().height(80.0), SizedBox(None, Some(1000.0)))))
}

#[test]
fn inside_a_vertical_scroll_view_a_vertical_drag_ends_the_scrub_and_a_sideways_one_keeps_it() {
    let _g = hold();
    let s = series_of(4, &[Some(10.0), Some(20.0), Some(30.0), Some(40.0)]);

    // Vertical: the first move stays under the 18 px slop, the second passes it.
    let mut rig = Rig::new(in_scroll_view(&s));
    rig.frame(PointerPhase::Down, 100.0, 30.0);
    rig.frame(PointerPhase::Move, 101.0, 40.0);
    assert_eq!(rig.paint().crosshairs().len(), 1);
    rig.frame(PointerPhase::Move, 102.0, 60.0);
    rig.frame(PointerPhase::Move, 102.0, 90.0);
    rig.frame(PointerPhase::Up, 102.0, 90.0);
    assert_eq!(rig.log.events.len(), 3, "{:?}", rig.log.events);
    assert!(rig.log.events[0].is_some() && rig.log.events[1].is_some());
    assert!(
        rig.log.events[2].is_none(),
        "the scroll view ended the scrub"
    );
    assert!(rig.paint().crosshairs().is_empty());

    // Sideways: every move stays with the chart, however far it goes.
    let mut rig = Rig::new(in_scroll_view(&s));
    rig.frame(PointerPhase::Down, 100.0, 30.0);
    for x in [130.0, 180.0, 240.0, 290.0] {
        rig.frame(PointerPhase::Move, x, 34.0);
    }
    assert_eq!(rig.paint().crosshairs().len(), 1);
    rig.frame(PointerPhase::Up, 290.0, 34.0);
    assert_eq!(rig.log.events.len(), 6, "{:?}", rig.log.events);
    assert!(rig.log.events[..5].iter().all(Option::is_some));
    assert!(rig.log.events[5].is_none());
    let last = rig.log.events[4].as_ref().expect("a scrub");
    assert_eq!(last.values.len(), 1);
    assert!(last.values[0].is_some());
}
