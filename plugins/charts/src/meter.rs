//! The meter widgets: a linear meter coloured by a [`Tone`](crate::Tone), its segmented and
//! vertical variants, and a strip that draws many [`Samples`](crate::Samples) values in one
//! widget.
//!
//! A meter value is fixed (an `f32`, changed through a rebuild) or live (a [`SampleSlot`],
//! read at every paint, so a write repaints it with no rebuild); a gap draws the empty track.
//! A fill is clipped to the track's rounded shape and a positive value under one pixel still
//! draws one. Colours: the caller's, then the installed theme's (fill `primary`, track
//! `surface_container_highest`), then [`Tone::FALLBACK_CALM`] and
//! [`MeterView::FALLBACK_TRACK`]. Meters never animate, request no frames and take no input.

use frust::authoring::{
    BoxConstraints, BuildCtx, ChangeFlags, CornerRadii, LayoutCtx, PaintCtx, PaintScene, Point,
    Role, SemanticsCtx, Size, View, Widget,
};
use frust::{Color, Theme};

use crate::{SampleSlot, Samples, Tone, YDomain};

const DEFAULT_RANGE: (f32, f32) = (0.0, 100.0);
const DEFAULT_THICKNESS: f32 = 6.0;
const DEFAULT_STRIP_GAP: f32 = 2.0;
const DEFAULT_BAR_RADIUS: f32 = 2.0;
const DEFAULT_STRIP_HEIGHT: f32 = 40.0;

/// Where a meter's value comes from; build one with `From`.
#[derive(Clone, Debug)]
pub enum MeterValue {
    /// A value carried by the view, changed through a rebuild; `None` is a gap.
    Fixed(Option<f32>),
    /// A [`Samples`] slot read at every paint, so a write shows with no rebuild.
    Live(SampleSlot),
}

impl MeterValue {
    /// The value to draw now: `None` for a gap (a non-finite fixed value is one too).
    pub fn get(&self) -> Option<f32> {
        match self {
            Self::Fixed(value) => value.filter(|v| v.is_finite()),
            Self::Live(slot) => slot.get(),
        }
    }

    /// The same fixed value, or the same slot of the same set.
    fn same_source(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Fixed(a), Self::Fixed(b)) => a.map(f32::to_bits) == b.map(f32::to_bits),
            (Self::Live(a), Self::Live(b)) => {
                a.samples().ptr_eq(b.samples()) && a.index() == b.index()
            }
            _ => false,
        }
    }
}

impl From<f32> for MeterValue {
    fn from(value: f32) -> Self {
        Self::Fixed(Some(value).filter(|v| v.is_finite()))
    }
}

impl From<Option<f32>> for MeterValue {
    fn from(value: Option<f32>) -> Self {
        Self::Fixed(value.filter(|v| v.is_finite()))
    }
}

impl From<SampleSlot> for MeterValue {
    fn from(slot: SampleSlot) -> Self {
        Self::Live(slot)
    }
}

/// One part of a [`meter_segments`] meter: an amount and the colour it fills with.
#[derive(Clone, Debug)]
pub struct Segment {
    value: MeterValue,
    color: Color,
}

impl Segment {
    /// A segment of `value` (an amount above the range's minimum; a gap takes no room).
    pub fn new(value: impl Into<MeterValue>, color: Color) -> Self {
        Self {
            value: value.into(),
            color,
        }
    }

    fn same(&self, other: &Self) -> bool {
        self.color == other.color && self.value.same_source(&other.value)
    }
}

#[derive(Clone, Debug)]
enum Fill {
    Single(MeterValue),
    Segments(Vec<Segment>),
}

impl Fill {
    fn same(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Single(a), Self::Single(b)) => a.same_source(b),
            (Self::Segments(a), Self::Segments(b)) => {
                a.len() == b.len() && a.iter().zip(b).all(|(a, b)| a.same(b))
            }
            _ => false,
        }
    }
}

#[derive(Clone, Debug)]
struct MeterSpec {
    fill: Fill,
    range: (f32, f32),
    thickness: f32,
    radius: Option<f32>,
    color: Option<Color>,
    tone: Option<Tone>,
    track: Option<Color>,
    vertical: bool,
    length: Option<f32>,
    label: Option<String>,
}

impl MeterSpec {
    fn new(fill: Fill) -> Self {
        Self {
            fill,
            range: DEFAULT_RANGE,
            thickness: DEFAULT_THICKNESS,
            radius: None,
            color: None,
            tone: None,
            track: None,
            vertical: false,
            length: None,
            label: None,
        }
    }

    /// What a widget holding `old` must redo to show `self`.
    fn changes(&self, old: &Self) -> ChangeFlags {
        if self.thickness != old.thickness
            || self.vertical != old.vertical
            || self.length != old.length
        {
            return ChangeFlags::LAYOUT | ChangeFlags::PAINT;
        }
        if !self.fill.same(&old.fill)
            || self.range != old.range
            || self.radius != old.radius
            || self.color != old.color
            || self.tone != old.tone
            || self.track != old.track
            || self.label != old.label
        {
            return ChangeFlags::PAINT;
        }
        ChangeFlags::NONE
    }

    /// The corner radius for a track of `size`: the explicit one, else fully rounded.
    fn corner(&self, size: Size) -> f64 {
        let cross = if self.vertical {
            size.width
        } else {
            size.height
        };
        let radius = self.radius.map_or(cross / 2.0, f64::from);
        radius.max(0.0).min(size.width.min(size.height) / 2.0)
    }

    /// The part of the track from `start` to `start + len` along the main axis (bottom-up
    /// when vertical).
    fn span(&self, origin: Point, size: Size, start: f64, len: f64) -> (Point, Size) {
        if self.vertical {
            let y = origin.y + size.height - start - len;
            (Point::new(origin.x, y), Size::new(size.width, len))
        } else {
            (
                Point::new(origin.x + start, origin.y),
                Size::new(len, size.height),
            )
        }
    }

    /// The value assistive technology reads: the meter's value, or the segments' sum.
    fn reported_value(&self, domain: (f32, f32)) -> Option<f32> {
        let value = match &self.fill {
            Fill::Single(value) => value.get()?,
            Fill::Segments(segments) => {
                let mut present = segments.iter().filter_map(|s| s.value.get()).peekable();
                present.peek()?;
                domain.0 + present.map(|v| v.max(0.0)).sum::<f32>()
            }
        };
        Some(value.clamp(domain.0, domain.1))
    }
}

/// `(min, max)` made finite and non-degenerate, as [`YDomain::resolve`] does.
fn resolve(range: (f32, f32)) -> (f32, f32) {
    YDomain::fixed(range.0, range.1).resolve(None)
}

/// Where `value` lands along `extent` for a resolved `domain`, clamped to `0..=extent`
/// (multiplied before dividing, so a whole-number share of a whole extent stays whole).
fn offset(value: f64, domain: (f32, f32), extent: f64) -> f64 {
    let (min, max) = (f64::from(domain.0), f64::from(domain.1));
    if !value.is_finite() || extent <= 0.0 {
        return 0.0;
    }
    (extent * (value - min) / (max - min)).clamp(0.0, extent)
}

/// The length `value` fills along `extent`: at least one pixel when it is above the minimum.
fn fill_len(value: f32, domain: (f32, f32), extent: f64) -> f64 {
    let len = offset(f64::from(value), domain, extent);
    if len <= 0.0 {
        return 0.0;
    }
    len.max(1.0).min(extent)
}

/// The `(fill, track)` colours when the caller gives none: the theme's, else the fallbacks.
fn default_colors(ctx: &PaintCtx) -> (Color, Color) {
    match Theme::from_paint_ctx(ctx) {
        Some(theme) => {
            let scheme = theme.scheme();
            (scheme.primary, scheme.surface_container_highest)
        }
        None => (Tone::FALLBACK_CALM, MeterView::FALLBACK_TRACK),
    }
}

fn fill_color(tone: Option<&Tone>, color: Option<Color>, default: Color, value: f32) -> Color {
    tone.map(|tone| tone.color_at(value))
        .or(color)
        .unwrap_or(default)
}

/// The main-axis extent: `fixed` when given, else the available space.
fn main_extent(fixed: Option<f32>, available: f64, min: f64) -> f64 {
    match fixed.map(f64::from) {
        Some(len) if len.is_finite() => len.max(0.0),
        _ if available.is_finite() => available,
        _ => min,
    }
}

/// A determinate meter: [`meter`] for one value, [`meter_segments`] for stacked ones.
#[derive(Clone, Debug)]
pub struct MeterView {
    spec: MeterSpec,
}

/// A meter showing `value` (fixed `f32`, `Option<f32>`, or a live [`SampleSlot`]) on a 0..100
/// range, 6 px thick, expanding along its length.
pub fn meter(value: impl Into<MeterValue>) -> MeterView {
    MeterView {
        spec: MeterSpec::new(Fill::Single(value.into())),
    }
}

/// A meter of several values laid end to end from the track's start, each as long as its
/// share of the range; the total is clamped to the track and only the outer ends are rounded.
/// Each [`Segment`] carries its colour, so [`color`](MeterView::color) and
/// [`tone`](MeterView::tone) do not apply.
pub fn meter_segments(segments: impl IntoIterator<Item = Segment>) -> MeterView {
    MeterView {
        spec: MeterSpec::new(Fill::Segments(segments.into_iter().collect())),
    }
}

impl MeterView {
    /// The track colour when neither the caller nor the theme gives one.
    pub const FALLBACK_TRACK: Color = Color::from_rgb8(0xdf, 0xe3, 0xe8);

    /// The value range the track spans (default `0..100`; a reversed or empty one is fixed up
    /// as [`YDomain::resolve`] does).
    pub fn range(mut self, min: f32, max: f32) -> Self {
        self.spec.range = (min, max);
        self
    }

    /// The track's cross-axis size in logical px (default 6).
    pub fn thickness(mut self, thickness: f32) -> Self {
        self.spec.thickness = thickness;
        self
    }

    /// The corner radius (default half the thickness: fully rounded).
    pub fn radius(mut self, radius: f32) -> Self {
        self.spec.radius = Some(radius);
        self
    }

    /// The fill colour (default: the theme's primary).
    pub fn color(mut self, color: Color) -> Self {
        self.spec.color = Some(color);
        self
    }

    /// Colour the fill by `tone` at the current value; wins over [`color`](Self::color).
    pub fn tone(mut self, tone: Tone) -> Self {
        self.spec.tone = Some(tone);
        self
    }

    /// The track colour (default: the theme's `surface_container_highest`).
    pub fn track(mut self, color: Color) -> Self {
        self.spec.track = Some(color);
        self
    }

    /// Stand the meter up: it fills bottom-up, its thickness is its width and it expands in
    /// height.
    pub fn vertical(mut self) -> Self {
        self.spec.vertical = true;
        self
    }

    /// A fixed main-axis length in logical px (default: the available space).
    pub fn length(mut self, length: f32) -> Self {
        self.spec.length = Some(length);
        self
    }

    /// The accessible label of the meter's progress-indicator node.
    pub fn semantics(mut self, label: impl Into<String>) -> Self {
        self.spec.label = Some(label.into());
        self
    }
}

impl<State: 'static> View<State> for MeterView {
    type Element = MeterWidget;

    fn build(&self, _ctx: &mut BuildCtx<'_>) -> MeterWidget {
        MeterWidget {
            spec: self.spec.clone(),
        }
    }

    fn rebuild(
        &self,
        _prev: &Self,
        element: &mut MeterWidget,
        _ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        let flags = self.spec.changes(&element.spec);
        if !flags.is_empty() {
            element.spec = self.spec.clone();
        }
        flags
    }
}

/// The retained widget behind a [`MeterView`].
pub struct MeterWidget {
    spec: MeterSpec,
}

impl MeterWidget {
    fn paint_segments(
        &self,
        segments: &[Segment],
        origin: Point,
        size: Size,
        radius: f64,
        scene: &mut dyn PaintScene,
    ) {
        let spec = &self.spec;
        let domain = resolve(spec.range);
        let main = if spec.vertical {
            size.height
        } else {
            size.width
        };
        let base = f64::from(domain.0);
        let mut total = 0.0f64;
        let mut parts = Vec::new();
        for segment in segments {
            let Some(value) = segment.value.get().map(|v| v.max(0.0)) else {
                continue;
            };
            let start = offset(base + total, domain, main);
            total += f64::from(value);
            let end = offset(base + total, domain, main);
            if value <= 0.0 || start >= main {
                continue;
            }
            let end = end.max(start + 1.0).min(main);
            parts.push((spec.span(origin, size, start, end - start), segment.color));
        }
        if parts.is_empty() {
            return;
        }
        scene.push_clip_rounded(origin, size, radius);
        for ((at, extent), color) in parts {
            scene.fill_rect(at, extent, color);
        }
        scene.pop_clip();
    }
}

impl Widget for MeterWidget {
    fn layout(&mut self, _ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        let spec = &self.spec;
        let thickness = f64::from(spec.thickness).max(0.0);
        let (max, min) = (bc.max(), bc.min());
        let size = if spec.vertical {
            Size::new(thickness, main_extent(spec.length, max.height, min.height))
        } else {
            Size::new(main_extent(spec.length, max.width, min.width), thickness)
        };
        bc.constrain(size)
    }

    fn paint(&mut self, ctx: &mut PaintCtx, scene: &mut dyn PaintScene) {
        let (origin, size) = (ctx.origin(), ctx.size());
        if size.width <= 0.0 || size.height <= 0.0 {
            return;
        }
        let (default_fill, default_track) = default_colors(ctx);
        let spec = &self.spec;
        let radius = spec.corner(size);
        scene.fill_rounded_rect(origin, size, radius, spec.track.unwrap_or(default_track));
        let value = match &spec.fill {
            Fill::Single(value) => value,
            Fill::Segments(segments) => {
                return self.paint_segments(segments, origin, size, radius, scene);
            }
        };
        let Some(value) = value.get() else { return };
        let main = if spec.vertical {
            size.height
        } else {
            size.width
        };
        let len = fill_len(value, resolve(spec.range), main);
        if len <= 0.0 {
            return;
        }
        let color = fill_color(spec.tone.as_ref(), spec.color, default_fill, value);
        let (at, extent) = spec.span(origin, size, 0.0, len);
        scene.push_clip_rounded(origin, size, radius);
        scene.fill_rounded_rect(at, extent, radius.min(len / 2.0), color);
        scene.pop_clip();
    }

    fn semantics(&self, ctx: &mut SemanticsCtx) {
        let domain = resolve(self.spec.range);
        let value = self.spec.reported_value(domain);
        ctx.push_node(Role::ProgressIndicator, |node| {
            if let Some(value) = value {
                node.set_numeric_value(f64::from(value));
            }
            node.set_min_numeric_value(f64::from(domain.0));
            node.set_max_numeric_value(f64::from(domain.1));
            if let Some(label) = &self.spec.label {
                node.set_label(label.as_str());
            }
        });
    }
}

#[derive(Clone, Debug)]
struct StripSpec {
    samples: Samples,
    range: (f32, f32),
    gap: f32,
    bar_radius: f32,
    tone: Option<Tone>,
    color: Option<Color>,
    track: Option<Color>,
    height: f32,
    label: Option<String>,
}

impl StripSpec {
    fn changes(&self, old: &Self) -> ChangeFlags {
        if self.height != old.height {
            return ChangeFlags::LAYOUT | ChangeFlags::PAINT;
        }
        if !self.samples.ptr_eq(&old.samples)
            || self.range != old.range
            || self.gap != old.gap
            || self.bar_radius != old.bar_radius
            || self.tone != old.tone
            || self.color != old.color
            || self.track != old.track
            || self.label != old.label
        {
            return ChangeFlags::PAINT;
        }
        ChangeFlags::NONE
    }
}

/// `(left margin, bar width, gap)` for `count` bars across `width`: whole-pixel bars with the
/// leftover on the left, or fractional bars when whole ones would be under one pixel (with no
/// gap when the gaps alone would not fit).
fn bar_layout(width: f64, count: usize, gap: f64) -> (f64, f64, f64) {
    let n = count as f64;
    let mut gap = gap.max(0.0);
    if width - gap * (n - 1.0) <= 0.0 {
        gap = 0.0;
    }
    let room = width - gap * (n - 1.0);
    let whole = (room / n).floor();
    if whole >= 1.0 {
        let left = (room - whole * n).floor().max(0.0);
        (left, whole, gap)
    } else {
        (0.0, (room / n).max(0.0), gap)
    }
}

/// Many vertical meters drawn from one [`Samples`] set, one per index, side by side.
#[derive(Clone, Debug)]
pub struct MeterStripView {
    spec: StripSpec,
}

/// A strip of one vertical meter per index of `samples` (per-core load), read at every paint
/// so a write repaints it with no rebuild; 40 px tall, expanding to the available width.
pub fn meter_strip(samples: Samples) -> MeterStripView {
    MeterStripView {
        spec: StripSpec {
            samples,
            range: DEFAULT_RANGE,
            gap: DEFAULT_STRIP_GAP,
            bar_radius: DEFAULT_BAR_RADIUS,
            tone: None,
            color: None,
            track: None,
            height: DEFAULT_STRIP_HEIGHT,
            label: None,
        },
    }
}

impl MeterStripView {
    /// The space between bars in logical px (default 2).
    pub fn gap(mut self, gap: f32) -> Self {
        self.spec.gap = gap;
        self
    }

    /// The radius of each bar's top corners; bottoms stay square (default 2).
    pub fn bar_radius(mut self, radius: f32) -> Self {
        self.spec.bar_radius = radius;
        self
    }

    /// The value range every bar spans (default `0..100`).
    pub fn range(mut self, min: f32, max: f32) -> Self {
        self.spec.range = (min, max);
        self
    }

    /// Colour each bar's fill by `tone` at its value; wins over [`color`](Self::color).
    pub fn tone(mut self, tone: Tone) -> Self {
        self.spec.tone = Some(tone);
        self
    }

    /// The fill colour (default: the theme's primary).
    pub fn color(mut self, color: Color) -> Self {
        self.spec.color = Some(color);
        self
    }

    /// The track colour (default: the theme's `surface_container_highest`).
    pub fn track(mut self, color: Color) -> Self {
        self.spec.track = Some(color);
        self
    }

    /// The strip's height in logical px (default 40).
    pub fn height(mut self, height: f32) -> Self {
        self.spec.height = height;
        self
    }

    /// Give the strip an image node with this label; without one it has no node.
    pub fn semantics(mut self, label: impl Into<String>) -> Self {
        self.spec.label = Some(label.into());
        self
    }
}

impl<State: 'static> View<State> for MeterStripView {
    type Element = MeterStripWidget;

    fn build(&self, _ctx: &mut BuildCtx<'_>) -> MeterStripWidget {
        MeterStripWidget {
            spec: self.spec.clone(),
        }
    }

    fn rebuild(
        &self,
        _prev: &Self,
        element: &mut MeterStripWidget,
        _ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        let flags = self.spec.changes(&element.spec);
        if !flags.is_empty() {
            element.spec = self.spec.clone();
        }
        flags
    }
}

/// The retained widget behind a [`MeterStripView`].
pub struct MeterStripWidget {
    spec: StripSpec,
}

impl Widget for MeterStripWidget {
    fn layout(&mut self, _ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        let width = main_extent(None, bc.max().width, bc.min().width);
        let height = f64::from(self.spec.height).max(0.0);
        bc.constrain(Size::new(width, height))
    }

    fn paint(&mut self, ctx: &mut PaintCtx, scene: &mut dyn PaintScene) {
        let (origin, size) = (ctx.origin(), ctx.size());
        let spec = &self.spec;
        let count = spec.samples.len();
        if count == 0 || size.width <= 0.0 || size.height <= 0.0 {
            return;
        }
        let (left, bar, gap) = bar_layout(size.width, count, f64::from(spec.gap));
        if bar <= 0.0 {
            return;
        }
        let (default_fill, default_track) = default_colors(ctx);
        let track = spec.track.unwrap_or(default_track);
        let radius = f64::from(spec.bar_radius)
            .max(0.0)
            .min(bar / 2.0)
            .min(size.height);
        let domain = resolve(spec.range);
        spec.samples.with_values(|values| {
            for (i, value) in values.iter().enumerate() {
                let x = origin.x + left + i as f64 * (bar + gap);
                let top = CornerRadii::new(radius, radius, 0.0, 0.0);
                scene.fill_rounded_rect_radii(
                    Point::new(x, origin.y),
                    Size::new(bar, size.height),
                    top,
                    track,
                );
                let Some(value) = *value else { continue };
                let height = fill_len(value, domain, size.height);
                if height <= 0.0 {
                    continue;
                }
                let r = radius.min(height);
                let color = fill_color(spec.tone.as_ref(), spec.color, default_fill, value);
                scene.fill_rounded_rect_radii(
                    Point::new(x, origin.y + size.height - height),
                    Size::new(bar, height),
                    CornerRadii::new(r, r, 0.0, 0.0),
                    color,
                );
            }
        });
    }

    fn semantics(&self, ctx: &mut SemanticsCtx) {
        if let Some(label) = &self.spec.label {
            ctx.push_node(Role::Image, |node| node.set_label(label.as_str()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{bar_layout, fill_len};

    #[test]
    fn bars_are_whole_pixels_with_the_leftover_on_the_left() {
        // 8 bars, 7 gaps of 2: room 186, bar 23, leftover 2.
        assert_eq!(bar_layout(200.0, 8, 2.0), (2.0, 23.0, 2.0));
        let (left, bar, gap) = bar_layout(20.0, 32, 2.0);
        assert_eq!((left, gap), (0.0, 0.0));
        assert!(bar > 0.0 && bar < 1.0);
    }

    #[test]
    fn a_value_above_the_minimum_fills_at_least_one_pixel() {
        let domain = (0.0, 100.0);
        assert_eq!(fill_len(0.0, domain, 100.0), 0.0);
        assert_eq!(fill_len(0.1, domain, 100.0), 1.0);
        assert_eq!(fill_len(50.0, domain, 0.5), 0.5);
        assert_eq!(fill_len(12.0, domain, 200.0), 24.0);
        assert_eq!(fill_len(f32::NAN, domain, 200.0), 0.0);
    }
}
