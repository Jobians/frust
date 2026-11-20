//! The value axis: [`YDomain`] picks the value range a chart maps onto its height (or, for a
//! horizontal chart, its width), and [`nice_max`] rounds an automatic top up to a readable
//! number.

/// The value range a chart's y axis spans.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum YDomain {
    /// Always `min..max`, whatever the data.
    Fixed {
        /// The value at the bottom edge.
        min: f32,
        /// The value at the top edge.
        max: f32,
    },
    /// From `floor` up to the data's largest value, at least `min_span` tall, with the
    /// span rounded up by [`nice_max`] when `nice` is set.
    Auto {
        /// The value at the bottom edge.
        floor: f32,
        /// Round the span above `floor` up with [`nice_max`].
        nice: bool,
        /// The smallest span above `floor`, so a flat or empty series still has a scale.
        min_span: f32,
    },
    /// From the data's smallest value to its largest, at least `min_span` tall, with both
    /// ends rounded outward to round numbers when `nice` is set. See
    /// [`resolve_range`](Self::resolve_range).
    AutoRange {
        /// Round both ends outward to whole multiples of a 1, 2, 2.5 or 5 times a power of
        /// ten step, the step family [`ticks`](crate::ticks) draws from.
        nice: bool,
        /// The smallest span, so a flat or empty data set still has a scale.
        min_span: f32,
    },
}

impl YDomain {
    /// A fixed `min..max` range.
    pub const fn fixed(min: f32, max: f32) -> Self {
        Self::Fixed { min, max }
    }

    /// The fixed `0..100` range of a percentage.
    pub const fn percent() -> Self {
        Self::fixed(0.0, 100.0)
    }

    /// An automatic range from 0, rounded up with [`nice_max`], at least 1 tall.
    pub const fn auto_from_zero() -> Self {
        Self::Auto {
            floor: 0.0,
            nice: true,
            min_span: 1.0,
        }
    }

    /// An automatic range spanning the data from its minimum to its maximum, both ends
    /// rounded outward to round numbers, at least 1 tall.
    pub const fn auto() -> Self {
        Self::AutoRange {
            nice: true,
            min_span: 1.0,
        }
    }

    /// The `(min, max)` to draw against, given the data's largest value (`None` when there
    /// is none, see [`Series::max`](crate::Series::max)).
    ///
    /// Only the data's maximum is known here, so an [`AutoRange`](Self::AutoRange) domain
    /// treats the data as running from 0 to `data_max`: it resolves as
    /// [`resolve_range`](Self::resolve_range) does for `Some((0.0, data_max))` (or for `None`
    /// when `data_max` is `None` or non-finite). Use `resolve_range` when the data's minimum
    /// is known.
    ///
    /// Always finite with `max > min`: a reversed fixed range is swapped, and a degenerate or
    /// non-finite one is widened, so a caller can divide by `max - min` unguarded.
    pub fn resolve(&self, data_max: Option<f32>) -> (f32, f32) {
        match *self {
            Self::AutoRange { .. } => {
                self.resolve_range(data_max.filter(|v| v.is_finite()).map(|max| (0.0, max)))
            }
            Self::Fixed { min, max } => {
                let (lo, hi) = if min <= max { (min, max) } else { (max, min) };
                non_degenerate(lo, hi)
            }
            Self::Auto {
                floor,
                nice,
                min_span,
            } => {
                let floor = if floor.is_finite() { floor } else { 0.0 };
                let min_span = if min_span.is_finite() {
                    min_span.max(0.0)
                } else {
                    0.0
                };
                let top = data_max.filter(|v| v.is_finite()).unwrap_or(floor);
                let span = (top - floor).max(min_span);
                let span = if nice { nice_max(span) } else { span };
                non_degenerate(floor, floor + span)
            }
        }
    }
}

impl YDomain {
    /// The `(min, max)` to draw against, given the data's smallest and largest values (`None`
    /// when there are none).
    ///
    /// - [`Fixed`](Self::Fixed) ignores the data and resolves as [`resolve`](Self::resolve)
    ///   does.
    /// - [`Auto`](Self::Auto) uses its floor and the data's maximum, exactly as `resolve`
    ///   does for `Some(max)`.
    /// - [`AutoRange`](Self::AutoRange) spans the data. A span shorter than `min_span` is
    ///   widened evenly about its middle; no data spans `0..min_span`. With `nice` set, both
    ///   ends then move outward to whole multiples of the smallest 1, 2, 2.5 or 5 times a power
    ///   of ten step that divides the span into at most four parts, so `(3.2, 87.0)` resolves
    ///   to `(0.0, 100.0)` and `(-40.0, 12.0)` to `(-40.0, 20.0)`, and a tick step from that
    ///   family lands on both ends.
    ///
    /// A non-finite end of `data` is ignored (the other end stands for both), a reversed pair
    /// is swapped, and the result is always finite with `max > min`, as for `resolve`.
    pub fn resolve_range(&self, data: Option<(f32, f32)>) -> (f32, f32) {
        let data = data.and_then(finite_range);
        match *self {
            Self::Fixed { .. } => self.resolve(None),
            Self::Auto { .. } => self.resolve(data.map(|(_, max)| max)),
            Self::AutoRange { nice, min_span } => auto_range(data, nice, min_span),
        }
    }
}

/// `(min, max)` of the finite ends of `(a, b)`; one finite end stands for both.
fn finite_range((a, b): (f32, f32)) -> Option<(f32, f32)> {
    match (a.is_finite(), b.is_finite()) {
        (true, true) => Some((a.min(b), a.max(b))),
        (true, false) => Some((a, a)),
        (false, true) => Some((b, b)),
        (false, false) => None,
    }
}

/// How many parts at most a nice [`YDomain::AutoRange`] step divides the data's span into.
const RANGE_PARTS: f64 = 4.0;

/// The [`YDomain::AutoRange`] resolution of finite, ordered `data`.
fn auto_range(data: Option<(f32, f32)>, nice: bool, min_span: f32) -> (f32, f32) {
    let min_span = if min_span.is_finite() {
        f64::from(min_span.max(0.0))
    } else {
        0.0
    };
    let (lo, hi) = data.map_or((0.0, min_span), |(lo, hi)| (f64::from(lo), f64::from(hi)));
    let (lo, hi) = if hi - lo < min_span {
        let middle = lo + (hi - lo) / 2.0;
        (middle - min_span / 2.0, middle + min_span / 2.0)
    } else {
        (lo, hi)
    };
    let rounded = if nice { nice_ends(lo, hi) } else { None };
    let (lo, hi) = rounded.unwrap_or((lo as f32, hi as f32));
    non_degenerate(lo, hi)
}

/// `lo..hi` widened to whole multiples of the smallest nice step at least `(hi - lo) / 4`,
/// or `None` when the span is empty or a rounded end does not fit in `f32`. The result
/// always covers `lo..hi` as `f32` values.
fn nice_ends(lo: f64, hi: f64) -> Option<(f32, f32)> {
    let span = hi - lo;
    if !(span.is_finite() && span > 0.0) {
        return None;
    }
    let (mantissa, exponent) = nice_step(span / RANGE_PARTS)?;
    let size = mantissa * 10f64.powi(exponent);
    // The tolerance keeps an end that is a whole multiple of the step (up to the rounding of
    // the f32 it came from) on that multiple instead of a step further out. It can also pull
    // an end just outside a multiple onto it, so each end is checked against the data below.
    let tolerance = |x: f64| 1e-6 * x.abs().max(1.0);
    let (first, last) = (lo / size, hi / size);
    let first = (first + tolerance(first)).floor();
    let last = (last - tolerance(last)).ceil();
    let value = |index: f64| {
        // The integer multiple of the mantissa is exact and the power of ten is applied once,
        // so an end is the correctly rounded multiple rather than an accumulated sum.
        let scaled = index * mantissa;
        let value = if exponent >= 0 {
            scaled * 10f64.powi(exponent)
        } else {
            scaled / 10f64.powi(-exponent)
        };
        value as f32
    };
    // An end the tolerance left inside the data moves one step out, so the range covers it.
    let (data_lo, data_hi) = (lo as f32, hi as f32);
    let first = if value(first) > data_lo {
        first - 1.0
    } else {
        first
    };
    let last = if value(last) < data_hi {
        last + 1.0
    } else {
        last
    };
    let (lo, hi) = (value(first), value(last));
    (lo.is_finite() && hi.is_finite() && hi > lo).then_some((lo, hi))
}

/// The smallest `mantissa * 10^exponent` with a mantissa from [`NICE_STEPS`] that is `>= raw`.
fn nice_step(raw: f64) -> Option<(f64, i32)> {
    if !(raw.is_finite() && raw > 0.0) {
        return None;
    }
    // Start a decade low so an exponent `log10` rounds up by one still finds the answer.
    let decade = raw.log10().floor() as i32 - 1;
    for exponent in decade..=decade + 2 {
        for mantissa in NICE_STEPS {
            if mantissa * 10f64.powi(exponent) >= raw {
                return Some((mantissa, exponent));
            }
        }
    }
    None
}

impl Default for YDomain {
    /// [`YDomain::auto_from_zero`].
    fn default() -> Self {
        Self::auto_from_zero()
    }
}

/// `(lo, hi)` made finite with `hi > lo`, widening by `max(1, |lo|)` when it is not.
fn non_degenerate(lo: f32, hi: f32) -> (f32, f32) {
    let lo = if lo.is_finite() { lo } else { 0.0 };
    if hi.is_finite() && hi > lo {
        return (lo, hi);
    }
    let step = lo.abs().max(1.0);
    let up = lo + step;
    if up.is_finite() && up > lo {
        (lo, up)
    } else {
        (lo - step, lo)
    }
}

/// The multiples of a power of ten an automatic top rounds up to.
const NICE_STEPS: [f64; 4] = [1.0, 2.0, 2.5, 5.0];

/// The smallest of 1, 2, 2.5, 5 or 10 times a power of ten that is `>= value`.
///
/// `0.3 -> 0.5`, `4.2 -> 5`, `7 -> 10`, `12 -> 20`, `23 -> 25`, `100 -> 100`. Zero, a negative
/// or a non-finite `value` gives `1.0`; a value so large that no such number fits in `f32`
/// is returned unchanged.
pub fn nice_max(value: f32) -> f32 {
    if !value.is_finite() || value <= 0.0 {
        return 1.0;
    }
    // Start a decade low so an exponent `log10` rounds up by one still finds the answer.
    let exponent = f64::from(value).log10().floor() as i32 - 1;
    for decade in exponent..=exponent + 1 {
        let base = 10f64.powi(decade);
        for step in NICE_STEPS {
            let candidate = (step * base) as f32;
            if !candidate.is_finite() {
                return value;
            }
            if candidate >= value {
                return candidate;
            }
        }
    }
    let top = 10f64.powi(exponent + 2) as f32;
    if top.is_finite() { top } else { value }
}
