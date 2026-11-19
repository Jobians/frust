//! [`Tone`]: an ordered threshold colour scale (calm, busy, critical) and the heat-cell
//! intensity ramp.

use frust::Color;

/// An ordered threshold colour scale: a value takes the colour of the highest stop at or
/// below it, or the base colour below every stop.
///
/// ```
/// use frust::Color;
/// use frust_charts::Tone;
///
/// let tone = Tone::new(Color::from_rgb8(0x2e, 0x7d, 0x32)).stop(60.0, Color::from_rgb8(0xf9, 0xa8, 0x25));
/// assert_eq!(tone.color_at(60.0), Color::from_rgb8(0xf9, 0xa8, 0x25));
/// ```
#[derive(Clone, Debug, PartialEq)]
pub struct Tone {
    base: Color,
    stops: Vec<(f32, Color)>,
}

impl Tone {
    /// Where [`Tone::load`] turns busy, on a 0..100 scale.
    pub const LOAD_BUSY_AT: f32 = 60.0;
    /// Where [`Tone::load`] turns critical, on a 0..100 scale.
    pub const LOAD_CRITICAL_AT: f32 = 85.0;
    /// The calm colour a chart falls back to when neither the caller nor the theme gives one.
    pub const FALLBACK_CALM: Color = Color::from_rgb8(0x2e, 0x9e, 0x5b);
    /// The busy colour a chart falls back to when neither the caller nor the theme gives one.
    pub const FALLBACK_BUSY: Color = Color::from_rgb8(0xe0, 0xa3, 0x1a);
    /// The critical colour a chart falls back to when neither the caller nor the theme gives
    /// one.
    pub const FALLBACK_CRITICAL: Color = Color::from_rgb8(0xd6, 0x3b, 0x3b);

    /// A scale with no stops: every value takes `base`.
    pub fn new(base: Color) -> Self {
        Self {
            base,
            stops: Vec::new(),
        }
    }

    /// Add a stop: values `>= at` take `color` (up to the next stop). Stops stay sorted; a
    /// stop at an `at` already present replaces its colour, and a NaN `at` is ignored.
    pub fn stop(mut self, at: f32, color: Color) -> Self {
        if at.is_nan() {
            return self;
        }
        match self.stops.binary_search_by(|(s, _)| s.total_cmp(&at)) {
            Ok(i) => self.stops[i].1 = color,
            Err(i) => self.stops.insert(i, (at, color)),
        }
        self
    }

    /// The load rule on a 0..100 scale: `calm` below [`LOAD_BUSY_AT`](Self::LOAD_BUSY_AT),
    /// `busy` from it, `critical` from [`LOAD_CRITICAL_AT`](Self::LOAD_CRITICAL_AT).
    pub fn load(calm: Color, busy: Color, critical: Color) -> Self {
        Self::new(calm)
            .stop(Self::LOAD_BUSY_AT, busy)
            .stop(Self::LOAD_CRITICAL_AT, critical)
    }

    /// The colour for `value`; a NaN `value` takes the base colour.
    pub fn color_at(&self, value: f32) -> Color {
        self.stops
            .iter()
            .take_while(|(at, _)| value >= *at)
            .last()
            .map_or(self.base, |(_, color)| *color)
    }

    /// The colour below every stop.
    pub fn base(&self) -> Color {
        self.base
    }

    /// The stops, ascending by threshold.
    pub fn stops(&self) -> &[(f32, Color)] {
        &self.stops
    }

    /// Where `value` sits in `domain` (a `(min, max)` from
    /// [`YDomain::resolve`](crate::YDomain::resolve)), clamped to `0..=1`: a heat cell's fill
    /// strength. A non-finite `value` gives 0; a degenerate `domain` gives 0 at or below
    /// `min` and 1 above it.
    pub fn intensity_at(value: f32, domain: (f32, f32)) -> f32 {
        let (min, max) = domain;
        if !value.is_finite() {
            return 0.0;
        }
        let span = max - min;
        if !(span.is_finite() && span > 0.0) {
            return if value > min { 1.0 } else { 0.0 };
        }
        ((value - min) / span).clamp(0.0, 1.0)
    }
}
