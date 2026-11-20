# frust-charts

Live charts for Frust apps, fed from shared data handles (platform-free).

## Add the dependency

```toml
[dependencies]
frust-charts = "0.6"
```

## Data handles

```rust
use frust_charts::{Samples, Series, Tone, YDomain};

// A ring of 120 evenly spaced slots, newest last; `None` is a gap.
let load = Series::new(120);
let writer = load.clone();          // cheap, Send + Sync
std::thread::spawn(move || {
    writer.push(42.0);              // non-finite values become gaps
    writer.push_gap();
});

// A fixed set of current values, e.g. per-core load.
let cores = Samples::new(8);
cores.set(0, 12.5);
let meter_value = cores.slot(3);    // one index, for a single meter

let (min, max) = YDomain::auto_from_zero().resolve(load.max());
let tone = Tone::load(calm, busy, critical); // busy from 60, critical from 85
```

A write never rebuilds or lays out: it bumps the handle's revision and calls
`frust::request_repaint()` once per call, so a chart painting from the handle
updates on the next frame with no frame loop. The one exception is a
`time_series` with a y axis on an automatic domain: when new data changes its
ticks, the chart asks for one layout pass to shape the new labels. Charts are
display-only and tap-transparent unless a `time_series` sets `on_scrub` (see
Scrub). Colours come from the caller, then the installed theme, then the
fallback constants on `Tone`.

`YDomain` picks the y range: `fixed(min, max)`, `percent()` (0..100) or
`auto_from_zero()`, which rounds the data's top up with `nice_max` (1, 2, 2.5,
5 or 10 times a power of ten). `resolve` always returns `max > min`.

## Y axis

```rust
use frust_charts::{AxisSide, SeriesStyle, TickFormat, YAxis, time_series};

time_series::<App>()
    .series(load.clone(), SeriesStyle::color(blue))
    .y_axis(YAxis::new().side(AxisSide::Right).gutter(true).format(TickFormat::percent()));
```

Off by default. `ticks(min, max, target_count)` picks round tick values (`tick_count`
defaults to 3) and they replace the `.grid` fractions. `TickFormat` also has `plain`, `metric`,
`bytes`, `bytes_per_second` and `custom`; a custom closure is compared by identity, so build it
once rather than on every rebuild.

## Timestamped data

```rust
use frust_charts::TimedSeries;

let rtt = TimedSeries::windowed(60.0).gap_after(5.0); // last 60 s; a break over 5 s is a gap
rtt.push(now_secs, 31.5);                              // (seconds, value), kept in time order
time_series::<App>().series(rtt.clone(), SeriesStyle::color(blue));
```

`TimedSeries::new(capacity)` bounds by count instead; `window` adds a time bound to either.
`series`, `mirrored` and `sparkline` take a `Series` or a `TimedSeries`. Block styles draw a
line for timed data.

## Scrub

```rust
use frust_charts::{ScrubEvent, time_series};

time_series::<App>()
    .series(load.clone(), SeriesStyle::color(blue))
    .crosshair(white, 1.0)
    .on_scrub(|app: &mut App, e: Option<ScrubEvent>| app.hover = e.map(|e| e.values));
```

Off by default. With `on_scrub` set the plot handles presses, so it is no longer tap-transparent.
The callback gets a `ScrubEvent` (`sample`, one value per series, `x`, `position`) while the
pointer is down, and `None` on release. Inside a vertical scroll view a sideways drag stays with
the chart, while a horizontal scroll view takes the drag after the touch slop. Whenever a scroll
view takes the drag, the `None` arrives on the rebuild that follows.

## Line refinements

`TimeSeriesView::smooth(true)` draws a monotone curve through the samples that never overshoots
them. `SeriesStyle::tone_along_value(tone)` changes the line (and its area fill) colour where it
crosses a `Tone` threshold; `SeriesStyle::tone` still colours block columns only.
