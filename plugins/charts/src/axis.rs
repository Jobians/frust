//! Automatic y ticks and tick labels for `time_series`: tick values on 1, 2, 2.5 and 5 times
//! a power of ten, the formats that turn a tick into its label, and the [`YAxis`] options a
//! [`time_series`](crate::time_series) takes.
//!
//! Tick values are pure maths over a domain: [`ticks`] picks the step whose tick count is
//! nearest the target and generates every value from its integer index, so a long run of
//! ticks never accumulates rounding drift. Nothing here paints or holds state.

use std::fmt;
use std::rc::Rc;

use frust::Color;

use crate::YDomain;

/// The multiples of a power of ten a tick step is drawn from.
const STEP_MANTISSAS: [f64; 4] = [1.0, 2.0, 2.5, 5.0];
/// The largest target tick count [`ticks`] honours; a larger one is clamped to it.
const MAX_TARGET: usize = 1000;
/// The default target tick count of a [`YAxis`].
const DEFAULT_TICK_COUNT: usize = 3;
/// Significant digits of [`TickFormat::plain`] and [`TickFormat::percent`].
const PLAIN_DIGITS: i32 = 6;
/// Significant digits of the mantissa of [`TickFormat::metric`] and [`TickFormat::bytes`].
const COMPACT_DIGITS: i32 = 3;
/// The most decimals any built-in format writes.
const MAX_DECIMALS: i32 = 15;
const METRIC_UNITS: [&str; 5] = ["", "k", "M", "G", "T"];
const BYTE_UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];

/// Tick values for an axis spanning `min..max`, about `target_count` of them, ascending.
///
/// The step is 1, 2, 2.5 or 5 times a power of ten, chosen so the number of ticks inside the
/// range is nearest `target_count`; a tie goes to the step whose ticks cover more of the
/// range, then to the larger step. Every tick is a whole multiple of the step and lies inside
/// the range, and each one is computed from its index (`index * step`), never by repeated
/// addition, so ticks stay exact however many there are.
///
/// The range is normalised the way [`YDomain::resolve`] normalises a fixed one: reversed is
/// swapped, and degenerate or non-finite is widened, so the result is never empty. A
/// `target_count` below 2 counts as 2 and one above 1000 as 1000.
///
/// ```
/// use frust_charts::ticks;
///
/// assert_eq!(ticks(0.0, 100.0, 5), [0.0, 25.0, 50.0, 75.0, 100.0]);
/// assert_eq!(ticks(0.0, 4.2, 3), [0.0, 2.0, 4.0]);
/// ```
pub fn ticks(min: f32, max: f32, target_count: usize) -> Vec<f32> {
    let (lo, hi) = YDomain::fixed(min, max).resolve(None);
    let target = target_count.clamp(2, MAX_TARGET);
    let (lo64, hi64) = (f64::from(lo), f64::from(hi));
    let raw = (hi64 - lo64) / (target - 1) as f64;
    let decade = raw.log10().floor() as i32;
    let mut best: Option<Candidate> = None;
    for exponent in decade - 1..=decade + 1 {
        for mantissa in STEP_MANTISSAS {
            let step = Step { mantissa, exponent };
            let Some(candidate) = Candidate::new(step, lo64, hi64, target) else {
                continue;
            };
            if best.as_ref().is_none_or(|b| candidate.beats(b)) {
                best = Some(candidate);
            }
        }
    }
    let Some(best) = best else {
        return vec![lo, hi];
    };
    (best.first..=best.last)
        .map(|index| best.step.value(index).clamp(lo, hi))
        .collect()
}

/// A tick step: `mantissa * 10^exponent`.
#[derive(Clone, Copy)]
struct Step {
    mantissa: f64,
    exponent: i32,
}

impl Step {
    fn size(self) -> f64 {
        self.mantissa * 10f64.powi(self.exponent)
    }

    /// The tick at `index` steps from zero. The integer multiple of the mantissa is exact, and
    /// the power of ten is applied once, as a multiply or a divide by an exact power, so the
    /// result is the correctly rounded tick rather than an accumulated sum.
    fn value(self, index: i64) -> f32 {
        let scaled = index as f64 * self.mantissa;
        let value = if self.exponent >= 0 {
            scaled * 10f64.powi(self.exponent)
        } else {
            scaled / 10f64.powi(-self.exponent)
        };
        value as f32
    }
}

/// One step's ticks over a range, scored against the target count.
struct Candidate {
    step: Step,
    first: i64,
    last: i64,
    miss: u64,
    cover: f64,
}

impl Candidate {
    /// The ticks of `step` inside `lo..=hi`, or `None` when it has none or too many to index.
    fn new(step: Step, lo: f64, hi: f64, target: usize) -> Option<Self> {
        let size = step.size();
        if !(size.is_finite() && size > 0.0) {
            return None;
        }
        // A tolerance of a few f32 ulps keeps a bound that is a whole multiple of the step
        // (up to the rounding of the f32 it came from) on the inside.
        let tolerance = |x: f64| 1e-7 * x.abs().max(1.0);
        let (first, last) = (lo / size, hi / size);
        let first = (first - tolerance(first)).ceil();
        let last = (last + tolerance(last)).floor();
        let limit = (MAX_TARGET * 1000) as f64;
        if !(first.is_finite() && last.is_finite()) || last < first || last - first > limit {
            return None;
        }
        let count = (last - first) as u64 + 1;
        Some(Self {
            step,
            first: first as i64,
            last: last as i64,
            miss: count.abs_diff(target as u64),
            cover: (last - first) * size,
        })
    }

    /// Whether `self` is the better choice: nearer the target count, then more of the range
    /// covered, then the larger step.
    fn beats(&self, other: &Self) -> bool {
        if self.miss != other.miss {
            return self.miss < other.miss;
        }
        let tolerance = 1e-9 * self.cover.abs().max(other.cover.abs());
        if (self.cover - other.cover).abs() > tolerance {
            return self.cover > other.cover;
        }
        self.step.size() > other.step.size()
    }
}

/// How a tick value becomes its label.
///
/// The built-in formats compare by value. A [`custom`](Self::custom) format compares by the
/// identity of its closure, so a view that builds a fresh closure on every rebuild changes its
/// axis each time and pays a layout pass for it; build the format once and clone it to avoid
/// that.
#[derive(Clone, Default)]
pub struct TickFormat(Repr);

#[derive(Clone, Default)]
enum Repr {
    #[default]
    Plain,
    Percent,
    Metric,
    Bytes {
        per_second: bool,
    },
    Custom(Rc<dyn Fn(f32) -> String>),
}

impl TickFormat {
    /// The number with up to six significant digits and no trailing zeros: `25`, `2.5`,
    /// `0.125` (the default).
    pub fn plain() -> Self {
        Self(Repr::Plain)
    }

    /// The number as [`plain`](Self::plain) followed by ` %`: `25 %`. The value is already a
    /// percentage (`50.0` is `50 %`), not a fraction.
    pub fn percent() -> Self {
        Self(Repr::Percent)
    }

    /// The number scaled by powers of 1000 with a `k`, `M`, `G` or `T` suffix and up to three
    /// significant digits: `950`, `1.5k`, `250M`, `2T`.
    pub fn metric() -> Self {
        Self(Repr::Metric)
    }

    /// A byte count scaled by powers of 1024, with up to three significant digits:
    /// `512 B`, `1.5 KiB`, `2 MiB`, up to `PiB`.
    pub fn bytes() -> Self {
        Self(Repr::Bytes { per_second: false })
    }

    /// A byte rate: [`bytes`](Self::bytes) followed by `/s`, as in `1.5 MiB/s`.
    pub fn bytes_per_second() -> Self {
        Self(Repr::Bytes { per_second: true })
    }

    /// Format with `format`. See the type's docs for how a custom format compares.
    pub fn custom(format: impl Fn(f32) -> String + 'static) -> Self {
        Self(Repr::Custom(Rc::new(format)))
    }

    /// The label of `value`. A non-finite value is written as Rust writes it (`NaN`, `inf`)
    /// by the built-in formats.
    pub fn format(&self, value: f32) -> String {
        let v = f64::from(value);
        if !v.is_finite() && !matches!(self.0, Repr::Custom(_)) {
            return value.to_string();
        }
        match &self.0 {
            Repr::Plain => significant(v, PLAIN_DIGITS),
            Repr::Percent => format!("{} %", significant(v, PLAIN_DIGITS)),
            Repr::Metric => {
                let (text, unit) = scaled(v, 1000.0, &METRIC_UNITS);
                format!("{text}{unit}")
            }
            Repr::Bytes { per_second } => {
                let (text, unit) = scaled(v, 1024.0, &BYTE_UNITS);
                let rate = if *per_second { "/s" } else { "" };
                format!("{text} {unit}{rate}")
            }
            Repr::Custom(format) => format(value),
        }
    }
}

impl PartialEq for TickFormat {
    fn eq(&self, other: &Self) -> bool {
        match (&self.0, &other.0) {
            (Repr::Plain, Repr::Plain)
            | (Repr::Percent, Repr::Percent)
            | (Repr::Metric, Repr::Metric) => true,
            (Repr::Bytes { per_second: a }, Repr::Bytes { per_second: b }) => a == b,
            (Repr::Custom(a), Repr::Custom(b)) => Rc::ptr_eq(a, b),
            _ => false,
        }
    }
}

impl fmt::Debug for TickFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self.0 {
            Repr::Plain => "Plain",
            Repr::Percent => "Percent",
            Repr::Metric => "Metric",
            Repr::Bytes { per_second: false } => "Bytes",
            Repr::Bytes { per_second: true } => "BytesPerSecond",
            Repr::Custom(_) => "Custom(..)",
        };
        write!(f, "TickFormat::{name}")
    }
}

/// `value` divided by `base` until it is below `base` or the units run out, written with
/// [`COMPACT_DIGITS`] significant digits, and its unit. A mantissa that rounds up to `base`
/// moves to the next unit, so `999 999` reads `1M` rather than `1000k`.
fn scaled(value: f64, base: f64, units: &[&'static str]) -> (String, &'static str) {
    let mut mantissa = value;
    let mut unit = 0;
    while unit + 1 < units.len() && mantissa.abs() >= base {
        mantissa /= base;
        unit += 1;
    }
    let mut text = significant(mantissa, COMPACT_DIGITS);
    let rounded_up = text.parse::<f64>().is_ok_and(|m| m.abs() >= base);
    if rounded_up && unit + 1 < units.len() {
        mantissa /= base;
        unit += 1;
        text = significant(mantissa, COMPACT_DIGITS);
    }
    (text, units[unit])
}

/// `value` with `digits` significant digits (never dropping an integer digit) and no trailing
/// zeros or point; `-0` is written `0`.
fn significant(value: f64, digits: i32) -> String {
    if value == 0.0 {
        return "0".to_owned();
    }
    let magnitude = value.abs().log10().floor() as i32;
    let decimals = (digits - 1 - magnitude).clamp(0, MAX_DECIMALS) as usize;
    let mut text = format!("{value:.decimals$}");
    if text.contains('.') {
        let trimmed = text.trim_end_matches('0').trim_end_matches('.').len();
        text.truncate(trimmed);
    }
    if text == "-0" {
        text.remove(0);
    }
    text
}

/// Which edge of a chart its [`YAxis`] labels sit at.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AxisSide {
    /// The left edge (the default).
    #[default]
    Left,
    /// The right edge.
    Right,
}

/// An automatic y axis for a [`time_series`](crate::time_series): horizontal rules and
/// labels at [`ticks`] of the resolved domain, following an automatic domain as data arrives.
///
/// Labels sit inside the plot at the chosen side, or in a gutter beside it as wide as the
/// widest label with [`gutter`](Self::gutter). Their size and colour default to the chart's
/// corner labels.
///
/// ```
/// use frust_charts::{AxisSide, TickFormat, YAxis};
///
/// let _axis = YAxis::new()
///     .side(AxisSide::Right)
///     .tick_count(4)
///     .format(TickFormat::bytes_per_second())
///     .gutter(true);
/// ```
#[derive(Clone, Debug, PartialEq)]
pub struct YAxis {
    pub(crate) side: AxisSide,
    pub(crate) gutter: bool,
    pub(crate) tick_count: usize,
    pub(crate) format: TickFormat,
    pub(crate) color: Option<Color>,
    pub(crate) size: Option<f32>,
}

impl Default for YAxis {
    fn default() -> Self {
        Self {
            side: AxisSide::Left,
            gutter: false,
            tick_count: DEFAULT_TICK_COUNT,
            format: TickFormat::plain(),
            color: None,
            size: None,
        }
    }
}

impl YAxis {
    /// An axis on the left with about three [`plain`](TickFormat::plain) ticks, labels
    /// inside the plot.
    pub fn new() -> Self {
        Self::default()
    }

    /// The edge the labels sit at (default [`AxisSide::Left`]).
    pub fn side(mut self, side: AxisSide) -> Self {
        self.side = side;
        self
    }

    /// Reserve a gutter beside the plot for the labels, as wide as the widest label plus its
    /// inset, instead of drawing them over the plot (default `false`).
    pub fn gutter(mut self, gutter: bool) -> Self {
        self.gutter = gutter;
        self
    }

    /// About how many ticks to draw per half (default 3); see [`ticks`].
    pub fn tick_count(mut self, count: usize) -> Self {
        self.tick_count = count;
        self
    }

    /// How a tick value becomes its label (default [`TickFormat::plain`]).
    pub fn format(mut self, format: TickFormat) -> Self {
        self.format = format;
        self
    }

    /// The label colour (default: the chart's label colour).
    pub fn color(mut self, color: Color) -> Self {
        self.color = Some(color);
        self
    }

    /// The label font size (default: the chart's label size).
    pub fn size(mut self, size: f32) -> Self {
        self.size = Some(size);
        self
    }
}
