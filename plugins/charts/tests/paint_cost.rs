//! Paint cost of live charts under a data tick: what one tick costs when 64 sparklines, or a
//! realistic monitor page, repaint through the repaint-only path.
//!
//! Each tick writes one sample to every `Series` and then runs `RenderRoot::paint` alone: no
//! rebuild and no layout. That is the path a data write takes, so the figures are the cost of
//! the repaint a chart pays per tick.
//!
//! Two kinds of test live here:
//!
//! - [`every_sparkline_repaints_after_a_push_without_a_rebuild`] is always on. It checks that a
//!   push reaches the recorded output of every sparkline on the page without a rebuild. No
//!   timing is asserted.
//! - The `#[ignore]`d measurements print mean, p95 and p99 of paint time, the drawing commands
//!   per frame, and an idle control of the same page with no writes. Timings are host dependent
//!   and are not asserted.
//!
//! The geometry-cache rebuild counter is crate-private (`cfg(test)` inside `frust-charts`), so
//! an integration target cannot read it. The measurements therefore report the commands per
//! frame, and say so in the output rather than printing a rebuild figure.
//!
//! Run: `cargo test -p frust-charts --test paint_cost --release -- --ignored --nocapture`
//! (one measurement: append its name, for example `sparkline_tick_cost`).

use std::any::Any;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Instant;

use frust::authoring::text::TextContext;
use frust::authoring::{BezPath, Brush, PaintScene, Point, Size, View};
use frust::{Color, FrameTime};
use frust_charts::{
    Samples, Series, SeriesStyle, SparkStyle, heat_cells, meter, meter_strip, sparkline,
    time_series,
};
use frust_core::RenderRoot;
use frust_reactive::ReactiveRuntime;
use frust_widgets::Column;

/// Writing a `Series` or `Samples` requests a process-wide repaint, so every test holds this.
static STATICS: Mutex<()> = Mutex::new(());

fn hold() -> MutexGuard<'static, ()> {
    STATICS.lock().unwrap_or_else(PoisonError::into_inner)
}

const SPARKS: usize = 64;
const SLOTS: usize = 60;
const SPARK_W: f32 = 160.0;
const SPARK_H: f32 = 24.0;
const TICKS: usize = 300;
const WARMUP: usize = 3;
const METERS: usize = 4;
const STRIP_BARS: usize = 32;
const HEAT_CELLS: usize = 64;
const CHART_W: f32 = 342.0;
const CHART_H: f32 = 56.0;

/// A distinct colour per sparkline, so its commands can be picked out of a page's output.
fn spark_color(index: usize) -> Color {
    Color::from_rgb8((index * 4) as u8, 0x60, 0xc0)
}

/// The value a series receives on `tick`, for series `index`.
fn tick_value(tick: usize, index: usize) -> f32 {
    ((tick * 13 + index * 7) % 97) as f32
}

/// A scene that counts drawing commands and, when asked, keeps a fingerprint of each one with
/// the colour it was drawn in.
#[derive(Default)]
struct Scene {
    commands: usize,
    log: Option<Vec<(Option<Color>, String)>>,
}

impl Scene {
    fn logging() -> Self {
        Self {
            commands: 0,
            log: Some(Vec::new()),
        }
    }

    fn emit(&mut self, color: Option<Color>, describe: impl FnOnce() -> String) {
        self.commands += 1;
        if let Some(log) = &mut self.log {
            log.push((color, describe()));
        }
    }
}

fn solid(brush: &Brush) -> Option<Color> {
    match brush {
        Brush::Solid(color) => Some(*color),
        _ => None,
    }
}

impl PaintScene for Scene {
    fn fill_rect(&mut self, origin: Point, size: Size, color: Color) {
        self.emit(Some(color), || format!("rect {origin:?} {size:?}"));
    }
    fn fill_rounded_rect(&mut self, origin: Point, size: Size, radius: f64, color: Color) {
        self.emit(Some(color), || {
            format!("round {origin:?} {size:?} {radius}")
        });
    }
    fn fill_rounded_rect_brush(&mut self, origin: Point, size: Size, radius: f64, brush: &Brush) {
        self.emit(solid(brush), || {
            format!("round {origin:?} {size:?} {radius}")
        });
    }
    fn stroke_line(&mut self, p0: Point, p1: Point, width: f64, color: Color) {
        self.emit(Some(color), || format!("line {p0:?} {p1:?} {width}"));
    }
    fn stroke_path(&mut self, origin: Point, path: &BezPath, width: f64, brush: &Brush) {
        self.emit(solid(brush), || {
            format!("stroke {origin:?} {width} {:?}", path.elements())
        });
    }
    fn fill_path(&mut self, origin: Point, path: &BezPath, brush: &Brush) {
        self.emit(solid(brush), || {
            format!("fill {origin:?} {:?}", path.elements())
        });
    }
    fn draw_text(&mut self, _origin: Point, _text: &str) {}
}

/// A built page: a real `RenderRoot`, laid out once. Repaints run paint only.
struct Page<V: View<()> + 'static> {
    root: RenderRoot<(), V>,
}

impl<V: View<()> + 'static> Page<V> {
    fn new(size: Size, view: impl Fn() -> V) -> Self {
        let rt = ReactiveRuntime::init(Arc::new(|| {}));
        let mut root: RenderRoot<(), V> = RenderRoot::new();
        let mut logic = |_: &mut ()| view();
        rt.with_owner(|| root.rebuild(&mut logic, &mut ()));
        let mut text = TextContext::new();
        root.layout_with_text(size, &mut text as &mut dyn Any);
        Self { root }
    }

    /// The repaint-only path: no rebuild, no layout.
    fn repaint(&mut self, scene: &mut Scene) {
        self.root.paint(scene, FrameTime::from_nanos(1_000_000_000));
    }

    fn paint_log(&mut self) -> Vec<(Option<Color>, String)> {
        let mut scene = Scene::logging();
        self.repaint(&mut scene);
        scene.log.unwrap_or_default()
    }
}

fn spark_series() -> Vec<Series> {
    (0..SPARKS)
        .map(|i| {
            let series = Series::new(SLOTS);
            series.extend((0..SLOTS).map(|k| Some(tick_value(k, i))));
            series
        })
        .collect()
}

fn sparkline_view(series: &[Series], style: SparkStyle) -> impl View<()> + use<> {
    let sparks: Vec<_> = series
        .iter()
        .enumerate()
        .map(|(i, s)| {
            sparkline(s.clone())
                .style(style)
                .color(spark_color(i))
                .size(SPARK_W, SPARK_H)
        })
        .collect();
    Column(sparks)
}

fn blocks() -> SparkStyle {
    SparkStyle::Blocks {
        column_width: 2.0,
        gap: 1.0,
    }
}

fn line() -> SparkStyle {
    SparkStyle::Line { width: 1.0 }
}

/// Everything a monitor page shows, as shared handles: a tick writes these, a page reads them.
struct Monitor {
    area: Series,
    upper: Series,
    lower: Series,
    sparks: Vec<Series>,
    meters: Samples,
    strip: Samples,
    heat: Samples,
}

impl Monitor {
    fn new() -> Self {
        let filled = |n: usize| {
            let series = Series::new(SLOTS);
            series.extend((0..SLOTS).map(|k| Some(tick_value(k, n))));
            series
        };
        Self {
            area: filled(1),
            upper: filled(2),
            lower: filled(3),
            sparks: spark_series().into_iter().take(8).collect(),
            meters: Samples::new(METERS),
            strip: Samples::new(STRIP_BARS),
            heat: Samples::new(HEAT_CELLS),
        }
    }

    /// One data tick: a sample into every series, a value into every sample handle.
    fn tick(&self, tick: usize) {
        let series = [&self.area, &self.upper, &self.lower];
        for (i, s) in series.into_iter().chain(self.sparks.iter()).enumerate() {
            s.push(tick_value(tick, i));
        }
        for i in 0..METERS {
            self.meters.set(i, tick_value(tick, i));
        }
        for i in 0..STRIP_BARS {
            self.strip.set(i, tick_value(tick, i));
        }
        for i in 0..HEAT_CELLS {
            self.heat.set(i, tick_value(tick, i));
        }
    }

    fn view(&self) -> impl View<()> + use<> {
        let sparks: Vec<_> = self
            .sparks
            .iter()
            .enumerate()
            .map(|(i, s)| {
                sparkline(s.clone())
                    .style(blocks())
                    .color(spark_color(i))
                    .size(SPARK_W, SPARK_H)
            })
            .collect();
        let meters: Vec<_> = (0..METERS)
            .map(|i| meter(self.meters.slot(i)).range(0.0, 100.0))
            .collect();
        let area = time_series::<()>()
            .series(self.area.clone(), SeriesStyle::color(spark_color(1)))
            .height(CHART_H);
        let mirrored = time_series::<()>()
            .mirrored(
                (self.upper.clone(), SeriesStyle::color(spark_color(2))),
                (self.lower.clone(), SeriesStyle::color(spark_color(3))),
            )
            .height(CHART_H);
        Column((
            area,
            mirrored,
            sparks,
            meters,
            meter_strip(self.strip.clone()).range(0.0, 100.0),
            heat_cells::<()>(self.heat.clone()).columns(8),
        ))
    }
}

/// Mean, p95 and p99 of per-frame paint times, in microseconds.
struct Stats {
    mean: f64,
    p95: f64,
    p99: f64,
}

fn stats(mut samples: Vec<f64>) -> Stats {
    samples.sort_by(|a, b| a.partial_cmp(b).expect("frame timings are always finite"));
    let mean = samples.iter().sum::<f64>() / samples.len() as f64;
    Stats {
        mean,
        p95: percentile(&samples, 0.95),
        p99: percentile(&samples, 0.99),
    }
}

/// Nearest-rank percentile over an already-sorted sample set.
fn percentile(sorted: &[f64], p: f64) -> f64 {
    let rank = (((sorted.len() - 1) as f64) * p).round() as usize;
    sorted[rank.min(sorted.len() - 1)]
}

/// The timings of one run.
#[derive(Default)]
struct Costs {
    paint_us: Vec<f64>,
    commands: Vec<usize>,
}

/// Run `frames` paint-only frames after a few settle paints. `before(frame)` runs before each
/// frame; for a tick it writes the data, for the idle control it does nothing.
fn run<V: View<()> + 'static>(
    page: &mut Page<V>,
    frames: usize,
    mut before: impl FnMut(usize),
) -> Costs {
    for _ in 0..WARMUP {
        page.repaint(&mut Scene::default());
    }
    let mut costs = Costs::default();
    for frame in 0..frames {
        before(frame);
        let mut scene = Scene::default();
        let started = Instant::now();
        page.repaint(&mut scene);
        costs
            .paint_us
            .push(started.elapsed().as_secs_f64() * 1_000_000.0);
        costs.commands.push(scene.commands);
    }
    costs
}

fn report(name: &str, costs: Costs) {
    let commands = costs.commands.iter().sum::<usize>() as f64 / costs.commands.len() as f64;
    let s = stats(costs.paint_us);
    println!(
        "{name:<32} {:>10.2} {:>10.2} {:>10.2} {commands:>14.1}",
        s.mean, s.p95, s.p99
    );
}

fn print_header(title: &str) {
    println!("\n{title}");
    println!(
        "{:<32} {:>10} {:>10} {:>10} {:>14}",
        "scenario", "mean_us", "p95_us", "p99_us", "commands/frame"
    );
    println!(
        "geometry-cache rebuilds per frame: not observable from this target (crate-private counter)"
    );
}

#[test]
fn every_sparkline_repaints_after_a_push_without_a_rebuild() {
    let _g = hold();
    for style in [blocks(), line()] {
        let series = spark_series();
        let mut page = Page::new(
            Size::new(SPARK_W as f64, (SPARK_H as usize * SPARKS) as f64),
            || sparkline_view(&series, style),
        );
        let before = page.paint_log();
        for (i, s) in series.iter().enumerate() {
            s.push(tick_value(SLOTS, i));
        }
        let after = page.paint_log();
        for i in 0..SPARKS {
            let color = Some(spark_color(i));
            let own = |log: &[(Option<Color>, String)]| -> Vec<String> {
                log.iter()
                    .filter(|(c, _)| *c == color)
                    .map(|(_, text)| text.clone())
                    .collect()
            };
            let (b, a) = (own(&before), own(&after));
            assert!(!b.is_empty(), "sparkline {i} drew nothing before the push");
            assert_ne!(
                a, b,
                "sparkline {i} did not repaint after the push ({style:?})"
            );
        }
    }
}

#[test]
#[ignore = "perf evidence, host-machine dependent: run via \
            `cargo test -p frust-charts --test paint_cost --release -- --ignored --nocapture sparkline_tick_cost`"]
fn sparkline_tick_cost() {
    let _g = hold();
    print_header("64 sparklines, 160x24, 60 slots, one push per series per tick");
    for (name, style) in [("blocks", blocks()), ("line", line())] {
        let series = spark_series();
        let mut page = Page::new(
            Size::new(SPARK_W as f64, (SPARK_H as usize * SPARKS) as f64),
            || sparkline_view(&series, style),
        );
        let costs = run(&mut page, TICKS, |tick| {
            for (i, s) in series.iter().enumerate() {
                s.push(tick_value(tick + SLOTS, i));
            }
        });
        report(&format!("sparkline/{name}/tick"), costs);

        let mut page = Page::new(
            Size::new(SPARK_W as f64, (SPARK_H as usize * SPARKS) as f64),
            || sparkline_view(&series, style),
        );
        let costs = run(&mut page, TICKS, |_| {});
        report(&format!("sparkline/{name}/idle control"), costs);
    }
}

#[test]
#[ignore = "perf evidence, host-machine dependent: run via \
            `cargo test -p frust-charts --test paint_cost --release -- --ignored --nocapture monitor_page_tick_cost`"]
fn monitor_page_tick_cost() {
    let _g = hold();
    print_header(
        "monitor page: 2 time-series (342x56, one mirrored pair), 8 sparklines, 4 meters, \
         32-bar strip, 64-cell heat grid",
    );
    let monitor = Monitor::new();
    let mut page = Page::new(Size::new(CHART_W as f64, 1100.0), || monitor.view());
    let costs = run(&mut page, TICKS, |tick| monitor.tick(tick + SLOTS));
    report("monitor/tick", costs);

    let mut page = Page::new(Size::new(CHART_W as f64, 1100.0), || monitor.view());
    let costs = run(&mut page, TICKS, |_| {});
    report("monitor/idle control", costs);
}
