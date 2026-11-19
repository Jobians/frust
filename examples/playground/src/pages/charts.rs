//! The "Charts" section: every `frust-charts` widget updating live from one background thread,
//! with a counter that shows the updates never run `build`.
//!
//! The page's [`Component::init`] creates the data handles ([`Series`] and [`Samples`]) and starts
//! one thread that writes new values to all of them every [`TICK`]. A write bumps the handle's
//! revision and asks for a repaint; no signal changes, so nothing rebuilds. [`Component::build`]
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
//! - **pause feed**: the thread keeps ticking but writes nothing, so the page goes idle.
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
    Corner, PlotStyle, Samples, Segment, Series, SeriesStyle, SparkStyle, Tone, YDomain,
    auto_domain, heat_cells, meter, meter_segments, meter_strip, sparkline, time_series,
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
            .tone(load)
            .range(0.0, 100.0)
            .semantics("Heat grid");

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
            .child(heat);

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
        feed.tick(&mut rng);
        assert!(feed.cpu.revision() > before);
        assert_eq!(feed.cpu.len(), HISTORY);
        assert!(feed.cores.get(CORES - 1).is_some());
        assert!(feed.cells.get(CELLS - 1).is_some());
        assert!(feed.meters.get(METER_SLOTS - 1).is_some());
    }

    #[test]
    fn paused_feed_writes_nothing() {
        let _guard = WRITES.lock().unwrap_or_else(|e| e.into_inner());
        let feed = Feed::new();
        let mut rng = Walk::new(7);
        feed.paused.store(true, Ordering::Release);
        let before = (feed.cpu.revision(), feed.cores.revision());
        feed.tick(&mut rng);
        assert_eq!(before, (feed.cpu.revision(), feed.cores.revision()));
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
