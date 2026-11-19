//! The y axis: [`YDomain`] picks the value range a chart maps onto its height, and
//! [`nice_max`] rounds an automatic top up to a readable number.

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

    /// The `(min, max)` to draw against, given the data's largest value (`None` when there
    /// is none, see [`Series::max`](crate::Series::max)).
    ///
    /// Always finite with `max > min`: a reversed fixed range is swapped, and a degenerate or
    /// non-finite one is widened, so a caller can divide by `max - min` unguarded.
    pub fn resolve(&self, data_max: Option<f32>) -> (f32, f32) {
        match *self {
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
