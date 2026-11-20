//! The `sparkline` widget: a tiny, axis-free line or block-column strip over a
//! [`Series`](crate::Series) or a [`TimedSeries`](crate::TimedSeries), cheap enough to show
//! dozens at once.
//!
//! There are no axes, grid or text. Slots are right-aligned (the newest at the right edge) and
//! a gap draws nothing; timed points are placed by time, the newest at the right edge, and
//! draw as a line whatever the style. The geometry (a line's paths, or a block strip's
//! rectangles) is cached against the series revision and timing, the size, the style and the
//! domain, so a paint with none of them changed only redraws it. A write to the series
//! repaints with no rebuild; the widget never animates, requests no frames and takes no input.
//!
//! Colours: the caller's, then the installed theme's `primary`, then [`Tone::FALLBACK_CALM`].
//! A [`Tone`] colours each block by its own value, or a whole line by its latest value.

use frust::authoring::{BezPath, Brush};
use frust::authoring::{
    BoxConstraints, BuildCtx, ChangeFlags, LayoutCtx, PaintCtx, PaintScene, Point, Rect, Role,
    SemanticsCtx, Size, View, Widget,
};
use frust::{Color, Theme};

use crate::geometry::{Orientation, block_columns, line_segments, polyline, timed_line_segments};
use crate::timed::SourceShape;
use crate::{SeriesSource, Tone, YDomain};

const DEFAULT_HEIGHT: f64 = 24.0;
const DEFAULT_LINE_WIDTH: f32 = 1.0;
const DEFAULT_COLUMN_WIDTH: f32 = 2.0;
const DEFAULT_COLUMN_GAP: f32 = 1.0;

/// How a [`sparkline`] draws its series.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SparkStyle {
    /// A polyline per run of present samples, with a dot for a lone one.
    Line {
        /// The stroke width in logical px (default 1).
        width: f32,
    },
    /// One column per present sample, as tall as its value. A
    /// [`TimedSeries`](crate::TimedSeries) has no columns: it draws as [`SparkStyle::line`]
    /// instead.
    Blocks {
        /// The column width in logical px, snapped to a whole pixel (default 2).
        column_width: f32,
        /// The space between columns in logical px, snapped to a whole pixel (default 1).
        gap: f32,
    },
}

impl Default for SparkStyle {
    /// Blocks, 2 px wide with a 1 px gap.
    fn default() -> Self {
        Self::Blocks {
            column_width: DEFAULT_COLUMN_WIDTH,
            gap: DEFAULT_COLUMN_GAP,
        }
    }
}

impl SparkStyle {
    /// A line of the default 1 px width.
    pub const fn line() -> Self {
        Self::Line {
            width: DEFAULT_LINE_WIDTH,
        }
    }
}

#[derive(Clone, Debug)]
struct Spec {
    series: SeriesSource,
    style: SparkStyle,
    color: Option<Color>,
    tone: Option<Tone>,
    domain: YDomain,
    size: Option<(f32, f32)>,
    label: Option<String>,
    value: Option<String>,
}

impl Spec {
    /// What a widget holding `old` must redo to show `self`.
    fn changes(&self, old: &Self) -> ChangeFlags {
        if self.size != old.size {
            return ChangeFlags::LAYOUT | ChangeFlags::PAINT;
        }
        if !self.series.ptr_eq(&old.series)
            || self.style != old.style
            || self.color != old.color
            || self.tone != old.tone
            || self.domain != old.domain
            || self.label != old.label
            || self.value != old.value
        {
            return ChangeFlags::PAINT;
        }
        ChangeFlags::NONE
    }
}

/// A sparkline over `series`, a [`Series`](crate::Series) or a
/// [`TimedSeries`](crate::TimedSeries): block columns by default (a line for a timed series),
/// expanding to the available width and 24 px tall, with the domain `0..100`.
pub fn sparkline(series: impl Into<SeriesSource>) -> SparklineView {
    SparklineView {
        spec: Spec {
            series: series.into(),
            style: SparkStyle::default(),
            color: None,
            tone: None,
            domain: YDomain::percent(),
            size: None,
            label: None,
            value: None,
        },
    }
}

/// A tiny axis-free chart; build one with [`sparkline`].
#[derive(Clone, Debug)]
pub struct SparklineView {
    spec: Spec,
}

impl SparklineView {
    /// Blocks or a line (default blocks). A [`TimedSeries`](crate::TimedSeries) always draws a
    /// line: blocks fall back to [`SparkStyle::line`].
    pub fn style(mut self, style: SparkStyle) -> Self {
        self.spec.style = style;
        self
    }

    /// The drawing colour (default: the theme's primary).
    pub fn color(mut self, color: Color) -> Self {
        self.spec.color = Some(color);
        self
    }

    /// Colour each block by its own value, or a line by the latest value; wins over
    /// [`color`](Self::color).
    pub fn tone(mut self, tone: Tone) -> Self {
        self.spec.tone = Some(tone);
        self
    }

    /// The value range the height spans (default [`YDomain::percent`]).
    pub fn domain(mut self, domain: YDomain) -> Self {
        self.spec.domain = domain;
        self
    }

    /// A fixed size in logical px (default: the available width by 24), clamped into the
    /// constraints.
    pub fn size(mut self, width: f32, height: f32) -> Self {
        self.spec.size = Some((width, height));
        self
    }

    /// Give the chart an image node with this label; without one it has no node.
    pub fn semantics(mut self, label: impl Into<String>) -> Self {
        self.spec.label = Some(label.into());
        self
    }

    /// A description of the current value for assistive technology, reported with the label.
    pub fn semantic_value(mut self, text: impl Into<String>) -> Self {
        self.spec.value = Some(text.into());
        self
    }
}

impl<State: 'static> View<State> for SparklineView {
    type Element = SparklineWidget;

    fn build(&self, _ctx: &mut BuildCtx<'_>) -> SparklineWidget {
        SparklineWidget {
            spec: self.spec.clone(),
            cache: None,
        }
    }

    fn rebuild(
        &self,
        _prev: &Self,
        element: &mut SparklineWidget,
        _ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        let flags = self.spec.changes(&element.spec);
        if !flags.is_empty() {
            element.spec = self.spec.clone();
            // The key does not cover the handle, so a swap must not reuse the old geometry.
            element.cache = None;
        }
        flags
    }
}

/// What the cached geometry was computed for.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Key {
    revision: u64,
    /// The capacity of a series, or the window, gap threshold and time span of a timed one.
    source: SourceShape,
    width: f64,
    height: f64,
    style: SparkStyle,
    domain: YDomain,
}

/// Geometry in widget-local coordinates, so moving the widget does not invalidate it.
enum Shapes {
    Blocks(Vec<(Rect, f32)>),
    Line {
        paths: Vec<BezPath>,
        dots: Vec<Point>,
        latest: Option<f32>,
    },
}

struct Cache {
    key: Key,
    shapes: Shapes,
}

#[cfg(test)]
thread_local! {
    static REBUILDS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// The retained widget behind a [`SparklineView`].
pub struct SparklineWidget {
    spec: Spec,
    cache: Option<Cache>,
}

impl SparklineWidget {
    /// The style drawn: the spec's, except that a timed series has no block columns and
    /// falls back to a line.
    fn style(&self) -> SparkStyle {
        match self.spec.style {
            SparkStyle::Blocks { .. } if self.spec.series.is_timed() => SparkStyle::line(),
            style => style,
        }
    }

    fn compute(&self, key: Key) -> Shapes {
        #[cfg(test)]
        REBUILDS.with(|n| n.set(n.get() + 1));
        let series = match &self.spec.series {
            SeriesSource::Series(series) => series,
            SeriesSource::Timed(series) => {
                let (window, gap_after) = (series.window_secs(), series.gap_after_secs());
                return series.with_points(|points| {
                    let max = points.iter().map(|p| p.1).reduce(f32::max);
                    let domain = key.domain.resolve(max);
                    let width = match key.style {
                        SparkStyle::Line { width } => width,
                        SparkStyle::Blocks { .. } => DEFAULT_LINE_WIDTH,
                    };
                    let plot = line_plot(width, key.width, key.height);
                    let geometry = timed_line_segments(
                        points,
                        window,
                        gap_after,
                        domain,
                        plot,
                        Orientation::Upright,
                    );
                    Shapes::Line {
                        paths: geometry.polylines.iter().map(|run| polyline(run)).collect(),
                        dots: geometry.points,
                        latest: points.last().map(|p| p.1),
                    }
                });
            }
        };
        let capacity = series.capacity();
        series.with_slots(|slots| {
            let max = slots.iter().flatten().reduce(f32::max);
            let domain = key.domain.resolve(max);
            match key.style {
                SparkStyle::Blocks { column_width, gap } => {
                    let plot = Rect::new(0.0, 0.0, key.width, key.height);
                    let columns = block_columns(
                        slots.iter(),
                        capacity,
                        f64::from(column_width),
                        f64::from(gap),
                        domain,
                        plot,
                        Orientation::Upright,
                    );
                    Shapes::Blocks(columns.into_iter().map(|c| (c.rect, c.value)).collect())
                }
                SparkStyle::Line { width } => {
                    let plot = line_plot(width, key.width, key.height);
                    let geometry =
                        line_segments(slots.iter(), capacity, domain, plot, Orientation::Upright);
                    Shapes::Line {
                        paths: geometry.polylines.iter().map(|run| polyline(run)).collect(),
                        dots: geometry.points,
                        latest: slots.iter().rev().flatten().find(|v| v.is_finite()),
                    }
                }
            }
        })
    }
}

/// The rect a line is drawn in: the bounds inset by the dot radius on every side.
fn line_plot(width: f32, w: f64, h: f64) -> Rect {
    let dot = dot_radius(width);
    Rect::new(dot, dot, w - dot, h - dot)
}

/// The radius of the dot a lone sample becomes; it also insets a line from the edges so the
/// stroke and dots stay inside the bounds.
fn dot_radius(width: f32) -> f64 {
    if width.is_finite() {
        f64::from(width).max(1.0)
    } else {
        1.0
    }
}

fn stroke_width(width: f32) -> f64 {
    if width.is_finite() {
        f64::from(width).max(0.0)
    } else {
        0.0
    }
}

fn default_color(ctx: &PaintCtx) -> Color {
    match Theme::from_paint_ctx(ctx) {
        Some(theme) => theme.scheme().primary,
        None => Tone::FALLBACK_CALM,
    }
}

impl Widget for SparklineWidget {
    fn layout(&mut self, _ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        let (max, min) = (bc.max(), bc.min());
        let (width, height) = match self.spec.size {
            Some((w, h)) => (f64::from(w), f64::from(h)),
            None => {
                let width = if max.width.is_finite() {
                    max.width
                } else {
                    min.width
                };
                (width, DEFAULT_HEIGHT)
            }
        };
        let clean = |v: f64| if v.is_finite() { v.max(0.0) } else { 0.0 };
        bc.constrain(Size::new(clean(width), clean(height)))
    }

    fn paint(&mut self, ctx: &mut PaintCtx, scene: &mut dyn PaintScene) {
        let (origin, size) = (ctx.origin(), ctx.size());
        if !(size.width > 0.0
            && size.height > 0.0
            && size.width.is_finite()
            && size.height.is_finite())
        {
            return;
        }
        let (revision, source) = self.spec.series.shape();
        let key = Key {
            revision,
            source,
            width: size.width,
            height: size.height,
            style: self.style(),
            domain: self.spec.domain,
        };
        if self.cache.as_ref().is_none_or(|cache| cache.key != key) {
            let shapes = self.compute(key);
            self.cache = Some(Cache { key, shapes });
        }
        let Some(cache) = &self.cache else { return };
        let spec = &self.spec;
        let base = default_color(ctx);
        let base = spec.color.unwrap_or(base);
        match &cache.shapes {
            Shapes::Blocks(columns) => {
                if columns.is_empty() {
                    return;
                }
                scene.push_clip(origin, size);
                for (rect, value) in columns {
                    let color = spec.tone.as_ref().map_or(base, |t| t.color_at(*value));
                    scene.fill_rect(
                        Point::new(origin.x + rect.x0, origin.y + rect.y0),
                        Size::new(rect.width(), rect.height()),
                        color,
                    );
                }
                scene.pop_clip();
            }
            Shapes::Line {
                paths,
                dots,
                latest,
            } => {
                if paths.is_empty() && dots.is_empty() {
                    return;
                }
                let SparkStyle::Line { width } = cache.key.style else {
                    return;
                };
                let color = match (&spec.tone, latest) {
                    (Some(tone), Some(value)) => tone.color_at(*value),
                    _ => base,
                };
                let brush = Brush::Solid(color);
                let radius = dot_radius(width);
                scene.push_clip(origin, size);
                for path in paths {
                    scene.stroke_path(origin, path, stroke_width(width), &brush);
                }
                for dot in dots {
                    scene.fill_rounded_rect(
                        Point::new(origin.x + dot.x - radius, origin.y + dot.y - radius),
                        Size::new(radius * 2.0, radius * 2.0),
                        radius,
                        color,
                    );
                }
                scene.pop_clip();
            }
        }
    }

    fn semantics(&self, ctx: &mut SemanticsCtx) {
        if let Some(label) = &self.spec.label {
            ctx.push_node(Role::Image, |node| {
                node.set_label(label.as_str());
                if let Some(value) = &self.spec.value {
                    node.set_description(value.as_str());
                }
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use frust::FrameTime;
    use frust_core::RenderRoot;

    use super::*;
    use crate::Series;

    struct Null;

    impl PaintScene for Null {
        fn fill_rect(&mut self, _: Point, _: Size, _: Color) {}
        fn draw_text(&mut self, _: Point, _: &str) {}
    }

    #[test]
    fn geometry_is_rebuilt_only_when_its_inputs_change() {
        let series = Series::new(60);
        series.extend((0..60).map(|i| Some(i as f32)));
        let mut root: RenderRoot<(), SparklineView> = RenderRoot::new();
        let view = sparkline(series.clone());
        let mut view = Some(view);
        root.rebuild(&mut |_: &mut ()| view.take().expect("built once"), &mut ());
        root.layout(Size::new(160.0, 24.0));
        let mut paint = || root.paint(&mut Null, FrameTime::from_nanos(1_000_000_000));
        let before = REBUILDS.with(std::cell::Cell::get);
        paint();
        assert_eq!(REBUILDS.with(std::cell::Cell::get), before + 1);
        paint();
        paint();
        assert_eq!(REBUILDS.with(std::cell::Cell::get), before + 1, "reused");
        series.push(5.0);
        paint();
        assert_eq!(
            REBUILDS.with(std::cell::Cell::get),
            before + 2,
            "data moved"
        );
    }
}
