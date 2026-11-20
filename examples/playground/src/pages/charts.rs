//! The "Charts" section: every `frust-charts` widget updating live from one background thread,
//! with a counter that shows the updates never run `build`.
//!
//! The page's [`Component::init`] creates the data handles ([`Series`] and [`Samples`]) and starts
//! one thread that writes new values to all of them every [`TICK`]. A write bumps the handle's
//! revision and asks for a repaint; no signal changes, so nothing rebuilds.
//!
//! Below the monitoring set, a "Families" section shows each chart family once over handles
//! created in the same place and written by the same thread: a grouped bar chart and a radar
//! chart over [`Categories`], a donut over [`Categories`], a gauge over one [`Samples`] slot, a
//! scatter chart over [`Points`] and a candlestick chart over [`Candles`]. Their values drift
//! slowly; the newest candle keeps forming for [`CANDLE_TICKS`] ticks before the next one
//! starts. [`Component::build`]
//! counts itself (`builds: N`) and reads the cpu series' revision (`samples: N`). Both labels are
//! only fresh at the moment of a build: while the charts keep moving the labels stay still, and
//! pressing any control rebuilds the page and catches them up.
//!
//! # Feed lifetime
//!
//! The thread holds only a [`Weak`] reference to the page's [`Feed`] and upgrades it each tick,
//! so it ends on its own once the page state is dropped. An [`on_cleanup`] beside `init` also
//! raises the feed's stop flag when the page's owner is disposed.
//!
//! # Controls
//!
//! - **pause feed**: the thread keeps ticking but writes nothing, so the page goes idle (the
//!   families stop with the rest).
//! - **inject gap**: pushes gaps to the time-series handles for [`GAP_TICKS`] ticks (two seconds).
//! - **dim**: wraps the chart column in `animated_opacity` at 0.4 to show stale-state dimming.
//! - **open in page**: pushes a navigator page with one live chart over the same handle.
//! - A chart wrapped in a gesture detector counts taps: charts take no input, so a tap passes
//!   through to it.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use frust::motion::animated_opacity;
use frust::{
    ButtonStyle, Color, Component, EdgeInsets, GestureDetector, NavigatorController, Padding,
    SizedBox, Theme, View, button, checkbox, column, flexible, on_cleanup, row, text, use_context,
};
use frust_charts::{
    Candle, Candles, Categories, Corner, NeedleStyle, PlotStyle, Points, RadarStyle, Samples,
    ScatterPoint, Segment, Series, SeriesStyle, SparkStyle, Tone, YDomain, auto_domain, bar_chart,
    candlestick_chart, gauge, heat_cells, meter, meter_segments, meter_strip, pie_chart,
    radar_chart, scatter_chart, sparkline, time_series,
};

use crate::PlaygroundState;

/// How often the feed thread writes.
const TICK: Duration = Duration::from_millis(500);
/// How many ticks "inject gap" lasts: two seconds.
const GAP_TICKS: u32 = 4;
/// Slots kept per time series (one minute of history at one sample per tick).
const HISTORY: usize = 120;
/// Slots kept per sparkline row.
const SPARK_HISTORY: usize = 48;
/// Sparkline rows, bars in the strip and cells in the heat grid.
const ROWS: usize = 8;
const CORES: usize = 32;
const CELLS: usize = 64;
/// Meter slots: three single meters, then two segments of the segmented one.
const METER_SLOTS: usize = 5;
/// The bar chart's categories, each with [`BAR_SERIES`] values.
const BAR_LABELS: [&str; 4] = ["Q1", "Q2", "Q3", "Q4"];
const BAR_SERIES: usize = 3;
/// The donut's sections.
const SLICES: usize = 4;
/// The radar chart's axes, each with [`RADAR_SETS`] values.
const RADAR_LABELS: [&str; 6] = ["cpu", "mem", "disk", "net", "gpu", "io"];
const RADAR_SETS: usize = 2;
/// Points on the scatter chart.
const SCATTER_POINTS: usize = 30;
/// Candles kept, and how many ticks each one forms for (two seconds).
const CANDLES: usize = 30;
const CANDLE_TICKS: u32 = 4;

const BLUE: Color = Color::from_rgb8(0x3b, 0x82, 0xf6);
const AMBER: Color = Color::from_rgb8(0xf5, 0x9e, 0x0b);
const GREEN: Color = Color::from_rgb8(0x22, 0xc5, 0x5e);
const VIOLET: Color = Color::from_rgb8(0xa8, 0x55, 0xf7);

/// The shared data handles and the controls the feed thread reads.
struct Feed {
    cpu: Series,
    system: Series,
    down: Series,
    up: Series,
    rows: Vec<Series>,
    cores: Samples,
    cells: Samples,
    meters: Samples,
    bars: Categories,
    slices: Categories,
    gauge: Samples,
    scatter: Points,
    radar: Categories,
    candles: Candles,
    paused: AtomicBool,
    stopped: AtomicBool,
    gap_ticks: AtomicU32,
}

/// A small deterministic xorshift generator: plausible values with no dependency.
struct Walk {
    state: u64,
    cpu: f32,
    down: f32,
    up: f32,
    rows: [f32; ROWS],
    cores: [f32; CORES],
    cells: [f32; CELLS],
    meters: [f32; METER_SLOTS],
    bars: [[f32; BAR_SERIES]; BAR_LABELS.len()],
    slices: [f32; SLICES],
    gauge: f32,
    scatter: [(f32, f32); SCATTER_POINTS],
    radar: [[f32; RADAR_SETS]; RADAR_LABELS.len()],
    price: f32,
    /// Ticks written so far, which pace the candles.
    candle_ticks: u32,
}

impl Walk {
    fn new(seed: u64) -> Self {
        Self {
            state: seed.max(1),
            cpu: 35.0,
            down: 20.0,
            up: 4.0,
            rows: [30.0; ROWS],
            cores: [30.0; CORES],
            cells: [30.0; CELLS],
            meters: [30.0; METER_SLOTS],
            bars: [[30.0; BAR_SERIES]; BAR_LABELS.len()],
            slices: [25.0; SLICES],
            gauge: 50.0,
            scatter: std::array::from_fn(|i| {
                let t = i as f32 / SCATTER_POINTS as f32;
                (5.0 + 90.0 * t, 20.0 + 60.0 * t)
            }),
            radar: [[60.0; RADAR_SETS]; RADAR_LABELS.len()],
            price: 50.0,
            candle_ticks: 0,
        }
    }

    /// A value in `0.0..1.0`.
    fn unit(&mut self) -> f32 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.state = x;
        ((x >> 40) as f32) / ((1u64 << 24) as f32)
    }

    /// A step of `value` by up to `step` either way, kept within `lo..=hi`.
    fn walk(&mut self, value: f32, step: f32, lo: f32, hi: f32) -> f32 {
        let delta = (self.unit() - 0.5) * 2.0 * step;
        (value + delta).clamp(lo, hi)
    }
}

impl Feed {
    fn new() -> Self {
        let feed = Self {
            cpu: Series::new(HISTORY),
            system: Series::new(HISTORY),
            down: Series::new(HISTORY),
            up: Series::new(HISTORY),
            rows: (0..ROWS).map(|_| Series::new(SPARK_HISTORY)).collect(),
            cores: Samples::new(CORES),
            cells: Samples::new(CELLS),
            meters: Samples::new(METER_SLOTS),
            bars: Categories::from_rows(BAR_LABELS.map(|label| (label, [0.0; BAR_SERIES]))),
            slices: Categories::from_rows((0..SLICES).map(|i| (format!("part {}", i + 1), [0.0]))),
            gauge: Samples::new(1),
            scatter: Points::new(SCATTER_POINTS),
            radar: Categories::from_rows(RADAR_LABELS.map(|label| (label, [0.0; RADAR_SETS]))),
            candles: Candles::new(CANDLES),
            paused: AtomicBool::new(false),
            stopped: AtomicBool::new(false),
            gap_ticks: AtomicU32::new(0),
        };
        // Start with a full history so the charts are not empty on first paint.
        let mut rng = Walk::new(0x9e37_79b9_7f4a_7c15);
        for _ in 0..HISTORY {
            feed.write(&mut rng);
        }
        feed
    }

    /// One tick: write nothing while paused, gaps for the time series while a gap is pending.
    fn tick(&self, rng: &mut Walk) {
        if self.paused.load(Ordering::Acquire) {
            return;
        }
        self.write(rng);
    }

    fn write(&self, rng: &mut Walk) {
        // Only this thread lowers the count; a concurrent "inject gap" store just wins.
        let pending = self.gap_ticks.load(Ordering::Acquire);
        let gap = pending > 0;
        if gap {
            let _ = self.gap_ticks.compare_exchange(
                pending,
                pending - 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
        }

        rng.cpu = rng.walk(rng.cpu, 9.0, 5.0, 98.0);
        rng.down = rng.walk(rng.down, 8.0, 1.0, 90.0);
        rng.up = rng.walk(rng.up, 2.0, 0.5, 22.0);
        if gap {
            self.cpu.push_gap();
            self.system.push_gap();
            self.down.push_gap();
            self.up.push_gap();
        } else {
            let system = rng.cpu * (0.25 + 0.2 * rng.unit());
            self.cpu.push(rng.cpu);
            self.system.push(system);
            self.down.push(rng.down);
            self.up.push(rng.up);
        }

        for index in 0..ROWS {
            rng.rows[index] = rng.walk(rng.rows[index], 12.0, 2.0, 99.0);
            self.rows[index].push(rng.rows[index]);
        }
        for index in 0..CORES {
            rng.cores[index] = rng.walk(rng.cores[index], 20.0, 0.0, 100.0);
        }
        self.cores.set_all(rng.cores.iter().map(|v| Some(*v)));
        for index in 0..CELLS {
            rng.cells[index] = rng.walk(rng.cells[index], 15.0, 0.0, 100.0);
        }
        self.cells.set_all(rng.cells.iter().map(|v| Some(*v)));
        for index in 0..METER_SLOTS {
            let (lo, hi) = if index < 3 { (0.0, 100.0) } else { (5.0, 45.0) };
            rng.meters[index] = rng.walk(rng.meters[index], 10.0, lo, hi);
        }
        self.meters.set_all(rng.meters.iter().map(|v| Some(*v)));
        self.write_families(rng);
    }

    /// The families: small steps, so their values drift rather than jump.
    fn write_families(&self, rng: &mut Walk) {
        for row in 0..BAR_LABELS.len() {
            for col in 0..BAR_SERIES {
                rng.bars[row][col] = rng.walk(rng.bars[row][col], 3.0, 5.0, 60.0);
            }
        }
        self.bars.set_rows(BAR_LABELS.into_iter().zip(rng.bars));
        for index in 0..SLICES {
            rng.slices[index] = rng.walk(rng.slices[index], 2.0, 5.0, 40.0);
        }
        self.slices
            .set_rows((0..SLICES).map(|i| (format!("part {}", i + 1), [rng.slices[i]])));
        rng.gauge = rng.walk(rng.gauge, 4.0, 0.0, 100.0);
        self.gauge.set(0, rng.gauge);
        for index in 0..SCATTER_POINTS {
            let (x, y) = rng.scatter[index];
            rng.scatter[index] = (rng.walk(x, 1.5, 0.0, 100.0), rng.walk(y, 3.0, 0.0, 100.0));
        }
        self.scatter
            .replace(rng.scatter.iter().enumerate().map(|(index, &(x, y))| {
                ScatterPoint::new(x, y)
                    .with_radius(2.0 + (index % 3) as f32)
                    .with_color(index % 3)
            }));
        for row in 0..RADAR_LABELS.len() {
            for col in 0..RADAR_SETS {
                rng.radar[row][col] = rng.walk(rng.radar[row][col], 4.0, 20.0, 100.0);
            }
        }
        self.radar.set_rows(RADAR_LABELS.into_iter().zip(rng.radar));

        // The newest candle forms for `CANDLE_TICKS` ticks, then the next opens at its close.
        rng.price = rng.walk(rng.price, 1.5, 20.0, 80.0);
        let price = rng.price;
        let forming = !rng.candle_ticks.is_multiple_of(CANDLE_TICKS);
        rng.candle_ticks = rng.candle_ticks.wrapping_add(1);
        match self.candles.latest() {
            Some(last) if forming => self.candles.update_last(Candle::new(
                last.open,
                last.high.max(price),
                last.low.min(price),
                price,
            )),
            last => {
                let open = last.map_or(price, |last| last.close);
                self.candles
                    .push(Candle::new(open, open.max(price), open.min(price), price));
            }
        }
    }
}

/// Run the feed until the page is gone or stopped.
fn spawn_feed(feed: &Arc<Feed>) {
    let weak: Weak<Feed> = Arc::downgrade(feed);
    let spawned = std::thread::Builder::new()
        .name("charts-feed".to_owned())
        .spawn(move || {
            let mut rng = Walk::new(0x2545_f491_4f6c_dd1d);
            loop {
                std::thread::sleep(TICK);
                let Some(feed) = weak.upgrade() else { break };
                if feed.stopped.load(Ordering::Acquire) {
                    break;
                }
                feed.tick(&mut rng);
            }
        });
    if let Err(error) = spawned {
        log::warn!("charts feed thread did not start: {error}");
    }
}

/// The Charts page's retained state.
pub struct ChartsState {
    feed: Arc<Feed>,
    /// How many times `build` has run.
    builds: u32,
    /// Taps counted by the gesture detector wrapped around one chart.
    taps: u32,
    dim: bool,
    paused: bool,
}

/// The Charts page's own [`Component`] (see the [module docs](self)).
pub struct ChartsPage {
    /// The shared navigator, for the "open in page" button.
    pub nav: NavigatorController<PlaygroundState>,
}

impl Component for ChartsPage {
    type State = ChartsState;

    fn init(&self) -> ChartsState {
        let feed = Arc::new(Feed::new());
        let weak = Arc::downgrade(&feed);
        on_cleanup(move || {
            if let Some(feed) = weak.upgrade() {
                feed.stopped.store(true, Ordering::Release);
            }
        });
        spawn_feed(&feed);
        ChartsState {
            feed,
            builds: 0,
            taps: 0,
            dim: false,
            paused: false,
        }
    }

    fn build(&self, state: &mut ChartsState) -> impl View<ChartsState> {
        state.builds += 1;
        let feed = &state.feed;
        let load = Tone::load(
            Tone::FALLBACK_CALM,
            Tone::FALLBACK_BUSY,
            Tone::FALLBACK_CRITICAL,
        );

        // Axis label text for the auto-domain pair is read now and goes stale as data moves.
        let (_, net_top) = auto_domain(&[&feed.down, &feed.up], YDomain::auto_from_zero());

        let area = time_series::<ChartsState>()
            .series(feed.cpu.clone(), SeriesStyle::color(BLUE))
            .series(feed.system.clone(), SeriesStyle::color(AMBER))
            .grid([0.25, 0.5, 0.75])
            .label(Corner::TopLeft, "100 %")
            .label(Corner::BottomLeft, "0")
            .label(Corner::TopRight, "cpu / system")
            .height(96.0)
            .semantics("CPU load, area chart");

        let blocks = time_series::<ChartsState>()
            .series(
                feed.cpu.clone(),
                SeriesStyle::color(BLUE).tone(load.clone()),
            )
            .style(PlotStyle::Blocks {
                column_width: 3.0,
                gap: 1.0,
            })
            .corner(1.0)
            .grid([0.5])
            .label(Corner::TopLeft, "100 %")
            .height(64.0)
            .semantics("CPU load, block columns");

        let mirrored = time_series::<ChartsState>()
            .mirrored(
                (feed.down.clone(), SeriesStyle::color(GREEN)),
                (feed.up.clone(), SeriesStyle::color(VIOLET)),
            )
            .domain(YDomain::auto_from_zero())
            .grid([0.5])
            .label(Corner::TopLeft, format!("{net_top:.0} MB/s"))
            .label(Corner::TopRight, "down / up")
            .height(96.0)
            .semantics("Network down and up, mirrored");

        let spark_rows = feed.rows.iter().enumerate().fold(
            column::<ChartsState>().gap(6.0),
            |rows, (index, series)| {
                let style = if index % 2 == 0 {
                    SparkStyle::default()
                } else {
                    SparkStyle::line()
                };
                rows.child(
                    row::<ChartsState>()
                        .gap(8.0)
                        .child(
                            SizedBox(Some(64.0), None)
                                .child(text(format!("row {}", index + 1)).size(11.0)),
                        )
                        .push(flexible(
                            1,
                            sparkline(series.clone())
                                .style(style)
                                .tone(load.clone())
                                .semantics(format!("Sparkline row {}", index + 1)),
                        )),
                )
            },
        );

        let meters = column::<ChartsState>()
            .gap(8.0)
            .child(
                meter(feed.meters.slot(0))
                    .thickness(4.0)
                    .tone(load.clone())
                    .semantics("Meter, thin"),
            )
            .child(
                meter(feed.meters.slot(1))
                    .thickness(6.0)
                    .tone(load.clone())
                    .semantics("Meter, medium"),
            )
            .child(
                meter(feed.meters.slot(2))
                    .thickness(8.0)
                    .tone(load.clone())
                    .semantics("Meter, thick"),
            )
            .child(
                meter_segments([
                    Segment::new(feed.meters.slot(3), BLUE),
                    Segment::new(feed.meters.slot(4), AMBER),
                ])
                .thickness(8.0)
                .semantics("Segmented meter"),
            );

        let strip = meter_strip(feed.cores.clone())
            .height(48.0)
            .tone(load.clone())
            .semantics("Per-core load");

        let heat = heat_cells::<ChartsState>(feed.cells.clone())
            .columns(16)
            .cell(12.0, 18.0)
            .tone(load.clone())
            .range(0.0, 100.0)
            .semantics("Heat grid");

        let families = families(feed, load);

        let charts = column::<ChartsState>()
            .gap(12.0)
            .child(caption("Area, two series, grid and corner labels"))
            .child(area)
            .child(caption("Block columns, toned by value (tap counts below)"))
            .child(GestureDetector(blocks).on_tap(|state: &mut ChartsState| state.taps += 1))
            .child(caption("Mirrored pair, auto domain"))
            .child(mirrored)
            .child(caption("Sparkline rows"))
            .child(spark_rows)
            .child(caption(
                "Meters 4 / 6 / 8 px, tone at 60 and 85, then segmented",
            ))
            .child(meters)
            .child(caption("Per-core strip"))
            .child(strip)
            .child(caption("Heat grid"))
            .child(heat)
            .child(text("Families").size(13.0).color(accent()))
            .child(families);

        let target = if state.dim { 0.4 } else { 1.0 };
        let pause_label = if state.paused {
            "resume feed"
        } else {
            "pause feed"
        };
        let nav = self.nav.clone();
        let handles = (feed.cpu.clone(), feed.system.clone());

        Padding(
            EdgeInsets::all(12.0),
            column::<ChartsState>()
                .gap(8.0)
                .child(text("Charts").size(13.0).color(accent()))
                .child(
                    text(
                        "Every chart is fed from one background thread and repaints on its own. \
                         The labels below are rebuilt only by a rebuild: they go stale while the \
                         charts move.",
                    )
                    .size(11.0)
                    .color(muted()),
                )
                .child(text(format!("builds: {}", state.builds)).size(12.0))
                .child(text(format!("samples: {}", feed.cpu.revision())).size(12.0))
                .child(text(format!("chart taps: {}", state.taps)).size(12.0))
                .child(
                    row::<ChartsState>()
                        .gap(8.0)
                        .child(
                            button(pause_label, |state: &mut ChartsState| {
                                state.paused = !state.paused;
                                state.feed.paused.store(state.paused, Ordering::Release);
                            })
                            .style(ButtonStyle::Secondary)
                            .small(),
                        )
                        .child(
                            button("inject gap", |state: &mut ChartsState| {
                                state.feed.gap_ticks.store(GAP_TICKS, Ordering::Release);
                            })
                            .style(ButtonStyle::Secondary)
                            .small(),
                        )
                        .child(checkbox(state.dim, "dim", |state: &mut ChartsState, on| {
                            state.dim = on;
                        })),
                )
                .child(
                    button("open in page", move |_state: &mut ChartsState| {
                        let handles = handles.clone();
                        let host = nav.clone();
                        nav.push(move || detail_page(host.clone(), handles.clone()));
                    })
                    .style(ButtonStyle::Primary)
                    .small(),
                )
                .child(animated_opacity(target, charts)),
        )
    }
}

/// One chart of each family, over the feed's family handles. Building reads no data: every chart
/// reads its handle while painting.
fn families(feed: &Feed, load: Tone) -> impl View<ChartsState> {
    let bars = bar_chart(feed.bars.clone())
        .colors([BLUE, AMBER, GREEN])
        .domain(YDomain::fixed(0.0, 60.0))
        .grid([0.5])
        .radius(2.0)
        .height(120.0)
        .semantics("Bar chart, three series per quarter");

    let donut = pie_chart::<ChartsState>(feed.slices.clone())
        .colors([BLUE, AMBER, GREEN, VIOLET])
        .donut(0.55)
        .gap(2.0)
        .corner_radius(2.0)
        .size(120.0)
        .semantics("Donut, four parts");

    let dial = gauge::<ChartsState>(feed.gauge.slot(0))
        .zones([
            (0.0, 60.0, Tone::FALLBACK_CALM),
            (60.0, 85.0, Tone::FALLBACK_BUSY),
            (85.0, 100.0, Tone::FALLBACK_CRITICAL),
        ])
        .tone(load)
        .needle(NeedleStyle::new())
        .ticks(11, 4.0, muted())
        .size(120.0)
        .semantics("Gauge");

    let scatter = scatter_chart::<ChartsState>(feed.scatter.clone())
        .x_domain(YDomain::fixed(0.0, 100.0))
        .y_domain(YDomain::fixed(0.0, 100.0))
        .colors([BLUE, AMBER, VIOLET])
        .grid(&[0.5], &[0.5])
        .height(120.0)
        .semantics("Scatter chart, three colours");

    let radar = radar_chart(feed.radar.clone())
        .domain(YDomain::fixed(0.0, 100.0))
        .dataset_styles([RadarStyle::color(GREEN), RadarStyle::color(BLUE)])
        .size(200.0)
        .semantics("Radar chart, two datasets");

    let candles = candlestick_chart(feed.candles.clone())
        .up(GREEN)
        .down(Tone::FALLBACK_CRITICAL)
        .grid([0.25, 0.5, 0.75])
        .height(96.0)
        .semantics("Candlestick chart");

    column::<ChartsState>()
        .gap(12.0)
        .child(caption("Bars, grouped, fixed 0..60"))
        .child(bars)
        .child(caption("Donut and gauge (zones, load tone, needle)"))
        .child(row::<ChartsState>().gap(16.0).child(donut).child(dial))
        .child(caption("Scatter, size and colour per point"))
        .child(scatter)
        .child(caption("Radar, two datasets"))
        .child(radar)
        .child(caption("Candlesticks, a new candle every two seconds"))
        .child(candles)
}

/// The pushed page: one live time series over the same handles, which is a retained boundary
/// for certain.
fn detail_page(
    nav: NavigatorController<PlaygroundState>,
    (cpu, system): (Series, Series),
) -> impl View<PlaygroundState> {
    Padding(
        EdgeInsets::all(12.0),
        column::<PlaygroundState>()
            .gap(8.0)
            .child(text("Chart in a pushed page").size(13.0).color(accent()))
            .child(
                time_series::<PlaygroundState>()
                    .series(cpu, SeriesStyle::color(BLUE))
                    .series(system, SeriesStyle::color(AMBER))
                    .grid([0.5])
                    .label(Corner::TopLeft, "100 %")
                    .height(120.0)
                    .semantics("CPU load in a pushed page"),
            )
            .child(
                button("Back", move |_state: &mut PlaygroundState| nav.pop())
                    .style(ButtonStyle::Secondary)
                    .small(),
            ),
    )
}

/// A muted section caption.
fn caption(label: &'static str) -> impl View<ChartsState> {
    text(label).size(11.0).color(muted())
}

fn accent() -> Color {
    use_context::<Theme>()
        .unwrap_or_else(frust_material::baseline)
        .scheme()
        .primary
}

fn muted() -> Color {
    use_context::<Theme>()
        .unwrap_or_else(frust_material::baseline)
        .scheme()
        .on_surface_variant
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    /// The repaint state is process-wide: serialise tests that write the handles.
    static WRITES: Mutex<()> = Mutex::new(());

    #[test]
    fn tick_writes_every_handle() {
        let _guard = WRITES.lock().unwrap_or_else(|e| e.into_inner());
        let feed = Feed::new();
        let mut rng = Walk::new(7);
        let before = feed.cpu.revision();
        let families_before = family_revisions(&feed);
        feed.tick(&mut rng);
        assert!(feed.cpu.revision() > before);
        assert_eq!(feed.cpu.len(), HISTORY);
        assert!(feed.cores.get(CORES - 1).is_some());
        assert!(feed.cells.get(CELLS - 1).is_some());
        assert!(feed.meters.get(METER_SLOTS - 1).is_some());
        let families_after = family_revisions(&feed);
        for (index, (before, after)) in families_before.iter().zip(&families_after).enumerate() {
            assert!(after > before, "family handle {index} was not written");
        }
        assert_eq!(feed.bars.len(), BAR_LABELS.len());
        assert_eq!(feed.scatter.len(), SCATTER_POINTS);
        assert_eq!(
            feed.candles.len(),
            CANDLES,
            "the prefill fills the candle ring"
        );
        assert!(feed.gauge.get(0).is_some());
    }

    /// Every family handle's revision, in one comparable value.
    fn family_revisions(feed: &Feed) -> [u64; 6] {
        [
            feed.bars.revision(),
            feed.slices.revision(),
            feed.gauge.revision(),
            feed.scatter.revision(),
            feed.radar.revision(),
            feed.candles.revision(),
        ]
    }

    #[test]
    fn a_candle_opens_at_the_last_close_then_forms_for_its_ticks() {
        let _guard = WRITES.lock().unwrap_or_else(|e| e.into_inner());
        let feed = Feed::new();
        // A fresh walk starts a candle on its first tick.
        let mut rng = Walk::new(7);
        let prior = feed.candles.latest().expect("a candle after the prefill");
        feed.tick(&mut rng);
        let opened = feed.candles.latest().expect("a candle");
        assert_eq!(
            opened.open, prior.close,
            "a new candle opens at the last close"
        );
        for _ in 1..CANDLE_TICKS {
            feed.tick(&mut rng);
            let forming = feed.candles.latest().expect("a candle");
            assert_eq!(forming.open, opened.open, "a forming candle keeps its open");
            assert!(forming.low <= forming.open.min(forming.close));
            assert!(forming.high >= forming.open.max(forming.close));
        }
    }

    #[test]
    fn paused_feed_writes_nothing() {
        let _guard = WRITES.lock().unwrap_or_else(|e| e.into_inner());
        let feed = Feed::new();
        let mut rng = Walk::new(7);
        feed.paused.store(true, Ordering::Release);
        let before = (
            feed.cpu.revision(),
            feed.cores.revision(),
            family_revisions(&feed),
        );
        feed.tick(&mut rng);
        assert_eq!(
            before,
            (
                feed.cpu.revision(),
                feed.cores.revision(),
                family_revisions(&feed)
            )
        );
    }

    #[test]
    fn injected_gap_lasts_its_ticks_then_data_resumes() {
        let _guard = WRITES.lock().unwrap_or_else(|e| e.into_inner());
        let feed = Feed::new();
        let mut rng = Walk::new(7);
        feed.gap_ticks.store(GAP_TICKS, Ordering::Release);
        for _ in 0..GAP_TICKS {
            feed.tick(&mut rng);
            assert_eq!(feed.cpu.latest(), None);
        }
        feed.tick(&mut rng);
        assert!(feed.cpu.latest().is_some());
    }

    #[test]
    fn generated_values_stay_in_range() {
        let _guard = WRITES.lock().unwrap_or_else(|e| e.into_inner());
        let feed = Feed::new();
        let mut rng = Walk::new(99);
        for _ in 0..200 {
            feed.tick(&mut rng);
        }
        feed.cpu.with_slots(|slots| {
            assert!(slots.iter().flatten().all(|v| (0.0..=100.0).contains(&v)));
        });
        feed.cores.with_values(|values| {
            assert!(values.iter().flatten().all(|v| (0.0..=100.0).contains(v)));
        });
    }
}
