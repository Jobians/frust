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
updates on the next frame with no frame loop. Charts are display-only and
tap-transparent. Colours come from the caller, then the installed theme, then
the fallback constants on `Tone`.

`YDomain` picks the y range: `fixed(min, max)`, `percent()` (0..100) or
`auto_from_zero()`, which rounds the data's top up with `nice_max` (1, 2, 2.5,
5 or 10 times a power of ten). `resolve` always returns `max > min`.
