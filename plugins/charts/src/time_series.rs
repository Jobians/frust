//! The `time_series` widget: one or more [`Series`] drawn as an area, a line or block columns,
//! sized to the box it is given.
//!
//! The widget reads its series while painting, so a push shows with no rebuild: the write
//! requests a repaint and the next paint draws the new samples. Built paths are cached per
//! series and rebuilt only when the series' revision, the plot size, the resolved domain or
//! the style changed. All geometry comes from the crate's geometry functions.
//!
//! Draw order: grid rules, then for each series (in the order given) its fill, its stroke and
//! the dots marking isolated samples, then the corner labels. Everything is clipped to the
//! widget's bounds. A mirrored pair splits the plot at a middle axis: the upper series grows
//! up from it and the lower one hangs down from it.
//!
//! Colours: the caller's, then the installed theme's (series `primary`, grid
//! `outline_variant`, labels `on_surface_variant`), then fallback constants. Labels are shaped
//! in layout with the theme's `body_small` family, so a theme change needs a layout pass. The
//! chart never animates, requests no frames and takes no input.

use std::marker::PhantomData;

use frust::authoring::text::{LocaleStamp, TextContext, TextLayout, TextStyle};
use frust::authoring::{
    BezPath, BoxConstraints, Brush, BuildCtx, ChangeFlags, LayoutCtx, PaintCtx, PaintScene, Point,
    Rect, Role, SemanticsCtx, Shape, Size, View, Widget,
};
use frust::kurbo::Circle;
use frust::peniko::{Gradient, GradientKind};
use frust::{Color, Theme};

use crate::geometry::{
    BlockColumn, Orientation, area_paths, block_columns, grid_rule_ys, line_segments, snap,
};
use crate::{Series, Tone, YDomain};

const DEFAULT_STROKE_WIDTH: f32 = 1.5;
const DEFAULT_HEIGHT: f64 = 56.0;
const UNBOUNDED_WIDTH: f64 = 160.0;
const DEFAULT_LABEL_SIZE: f32 = 9.5;
/// Label distance from the plot's left/right and top/bottom edges.
const LABEL_INSET: (f64, f64) = (4.0, 2.0);
/// The derived fill's alpha at the top of a series' extent and at its baseline.
const FILL_ALPHA: (f32, f32) = (0.40, 0.03);
const MIN_DOT_DIAMETER: f64 = 3.0;
const FALLBACK_GRID: Color = Color::from_rgb8(0xd0, 0xd4, 0xda);
const FALLBACK_LABEL: Color = Color::from_rgb8(0x5f, 0x65, 0x6e);

/// How the samples of every series are drawn.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum PlotStyle {
    /// A filled area under a line (the default).
    #[default]
    Area,
    /// A line alone; a series' fill is ignored.
    Line,
    /// One column per present sample, right-aligned, `column_width` wide with `gap` between
    /// columns (both snapped to whole pixels).
    Blocks {
        /// Column width in logical pixels.
        column_width: f32,
        /// Space between neighbouring columns in logical pixels.
        gap: f32,
    },
}

/// A corner of the plot, where a [`label`](TimeSeriesView::label) sits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Corner {
    /// The top-left corner.
    TopLeft,
    /// The top-right corner.
    TopRight,
    /// The bottom-left corner.
    BottomLeft,
    /// The bottom-right corner.
    BottomRight,
}

impl Corner {
    const ALL: [Self; 4] = [
        Self::TopLeft,
        Self::TopRight,
        Self::BottomLeft,
        Self::BottomRight,
    ];

    fn index(self) -> usize {
        match self {
            Self::TopLeft => 0,
            Self::TopRight => 1,
            Self::BottomLeft => 2,
            Self::BottomRight => 3,
        }
    }
}

/// What fills the area under a series.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum SeriesFill {
    /// A vertical gradient of the series colour, alpha 0.40 at the top of the series' extent
    /// down to 0.03 at its baseline (the default).
    #[default]
    Gradient,
    /// This brush; a gradient's coordinates are widget-local.
    Brush(Brush),
    /// No fill.
    None,
}

/// How one series is drawn: its colour, stroke and fill.
///
/// The default takes the theme's primary colour, a 1.5 px stroke and the derived gradient fill.
#[derive(Clone, Debug, PartialEq)]
pub struct SeriesStyle {
    color: Option<Color>,
    stroke: Option<Brush>,
    stroke_width: f32,
    fill: SeriesFill,
    tone: Option<Tone>,
}

impl Default for SeriesStyle {
    fn default() -> Self {
        Self {
            color: None,
            stroke: None,
            stroke_width: DEFAULT_STROKE_WIDTH,
            fill: SeriesFill::Gradient,
            tone: None,
        }
    }
}

impl SeriesStyle {
    /// The default style: the theme's primary colour.
    pub fn new() -> Self {
        Self::default()
    }

    /// Stroke in `color`, filled with a gradient derived from it.
    pub fn color(color: Color) -> Self {
        Self {
            color: Some(color),
            ..Self::default()
        }
    }

    /// Stroke with this brush instead of the series colour (a gradient's coordinates are
    /// widget-local); the derived fill still uses the colour.
    pub fn stroke(mut self, brush: impl Into<Brush>) -> Self {
        self.stroke = Some(brush.into());
        self
    }

    /// The stroke width in logical pixels (default 1.5); 0 draws no stroke. A negative or
    /// non-finite width is 0.
    pub fn stroke_width(mut self, width: f32) -> Self {
        self.stroke_width = if width.is_finite() {
            width.max(0.0)
        } else {
            0.0
        };
        self
    }

    /// Fill the area with this brush instead of the derived gradient.
    pub fn fill(mut self, brush: impl Into<Brush>) -> Self {
        self.fill = SeriesFill::Brush(brush.into());
        self
    }

    /// Draw no fill.
    pub fn no_fill(mut self) -> Self {
        self.fill = SeriesFill::None;
        self
    }

    /// Colour each column by its own value through `tone` ([`PlotStyle::Blocks`] only).
    pub fn tone(mut self, tone: Tone) -> Self {
        self.tone = Some(tone);
        self
    }
}

/// The `(min, max)` a chart draws `series` against under `domain`: the largest sample across
/// all of them feeds [`YDomain::resolve`]. Use it to format an axis label.
pub fn auto_domain(series: &[&Series], domain: YDomain) -> (f32, f32) {
    let max = series
        .iter()
        .filter_map(|series| series.max())
        .reduce(f32::max);
    domain.resolve(max)
}

/// Which half of a mirrored plot a series is drawn in; the whole plot when not mirrored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Half {
    Upper,
    Lower,
}

#[derive(Clone, Debug)]
struct Layer {
    series: Series,
    style: SeriesStyle,
    half: Half,
}

impl Layer {
    fn same(&self, other: &Self) -> bool {
        self.series.ptr_eq(&other.series) && self.style == other.style && self.half == other.half
    }
}

#[derive(Clone, Debug)]
struct Spec {
    layers: Vec<Layer>,
    mirrored: bool,
    shared_domain: bool,
    domain: YDomain,
    style: PlotStyle,
    corner: f32,
    grid: Vec<f32>,
    grid_color: Option<Color>,
    labels: [Option<String>; 4],
    label_size: f32,
    label_color: Option<Color>,
    height: Option<f32>,
    semantics: Option<String>,
    semantic_value: Option<String>,
}

impl Spec {
    /// What a widget holding `old` must redo to show `self`.
    fn changes(&self, old: &Self) -> ChangeFlags {
        if self.height != old.height
            || self.labels != old.labels
            || self.label_size != old.label_size
            || self.label_color != old.label_color
        {
            return ChangeFlags::LAYOUT | ChangeFlags::PAINT;
        }
        let same_layers = self.layers.len() == old.layers.len()
            && self.layers.iter().zip(&old.layers).all(|(a, b)| a.same(b));
        if !same_layers
            || self.mirrored != old.mirrored
            || self.shared_domain != old.shared_domain
            || self.domain != old.domain
            || self.style != old.style
            || self.corner != old.corner
            || self.grid != old.grid
            || self.grid_color != old.grid_color
            || self.semantics != old.semantics
            || self.semantic_value != old.semantic_value
        {
            return ChangeFlags::PAINT;
        }
        ChangeFlags::NONE
    }
}

/// A display-only time-series plot. Build one with [`time_series`].
pub struct TimeSeriesView<S: 'static> {
    spec: Spec,
    _state: PhantomData<fn(&mut S)>,
}

/// A time-series plot with no series yet: add them with [`series`](TimeSeriesView::series)
/// or [`mirrored`](TimeSeriesView::mirrored).
///
/// ```
/// use frust::Color;
/// use frust_charts::{Corner, Series, SeriesStyle, time_series};
///
/// let load = Series::new(120);
/// let _view = time_series::<()>()
///     .series(load.clone(), SeriesStyle::color(Color::from_rgb8(0x2e, 0x9e, 0x5b)))
///     .grid([0.5])
///     .label(Corner::TopLeft, "100 %")
///     .height(56.0)
///     .semantics("CPU load");
/// ```
pub fn time_series<S: 'static>() -> TimeSeriesView<S> {
    TimeSeriesView {
        spec: Spec {
            layers: Vec::new(),
            mirrored: false,
            shared_domain: false,
            domain: YDomain::percent(),
            style: PlotStyle::Area,
            corner: 0.0,
            grid: Vec::new(),
            grid_color: None,
            labels: Default::default(),
            label_size: DEFAULT_LABEL_SIZE,
            label_color: None,
            height: None,
            semantics: None,
            semantic_value: None,
        },
        _state: PhantomData,
    }
}

impl<S: 'static> TimeSeriesView<S> {
    /// Add a series; series draw in the order added, later ones on top. In a mirrored plot it
    /// joins the upper half.
    pub fn series(mut self, series: Series, style: SeriesStyle) -> Self {
        self.spec.layers.push(Layer {
            series,
            style,
            half: Half::Upper,
        });
        self
    }

    /// Split the plot at a middle axis: `upper` grows up from it and `lower` hangs down from
    /// it. Each half resolves its own domain unless [`shared_domain`](Self::shared_domain).
    pub fn mirrored(mut self, upper: (Series, SeriesStyle), lower: (Series, SeriesStyle)) -> Self {
        self.spec.mirrored = true;
        for ((series, style), half) in [(upper, Half::Upper), (lower, Half::Lower)] {
            self.spec.layers.push(Layer {
                series,
                style,
                half,
            });
        }
        self
    }

    /// Resolve one domain over both halves of a mirrored plot (default `false`).
    pub fn shared_domain(mut self, shared: bool) -> Self {
        self.spec.shared_domain = shared;
        self
    }

    /// The y domain (default [`YDomain::percent`]); an automatic one is resolved from the
    /// series' current data at every paint.
    pub fn domain(mut self, domain: YDomain) -> Self {
        self.spec.domain = domain;
        self
    }

    /// How samples are drawn (default [`PlotStyle::Area`]).
    pub fn style(mut self, style: PlotStyle) -> Self {
        self.spec.style = style;
        self
    }

    /// The corner radius of block columns (default 0: square).
    pub fn corner(mut self, radius: f32) -> Self {
        self.spec.corner = radius;
        self
    }

    /// Horizontal 1 px rules at these fractions of the domain, 0.0 the baseline and 1.0 the
    /// top (default none); a mirrored plot draws them in each half.
    pub fn grid(mut self, fractions: impl Into<Vec<f32>>) -> Self {
        self.spec.grid = fractions.into();
        self
    }

    /// The grid rule colour (default: the theme's `outline_variant`).
    pub fn grid_color(mut self, color: Color) -> Self {
        self.spec.grid_color = Some(color);
        self
    }

    /// A small caller-supplied label inside the plot at `corner`; one per corner, a later call
    /// replaces it.
    pub fn label(mut self, corner: Corner, text: impl Into<String>) -> Self {
        self.spec.labels[corner.index()] = Some(text.into());
        self
    }

    /// The labels' font size and colour (default 9.5 and the theme's `on_surface_variant`).
    pub fn label_style(mut self, size: f32, color: Color) -> Self {
        self.spec.label_size = size;
        self.spec.label_color = Some(color);
        self
    }

    /// A fixed height in logical pixels (default: the available height when bounded, else
    /// 56). The width always expands to the available width.
    pub fn height(mut self, height: f32) -> Self {
        self.spec.height = Some(height);
        self
    }

    /// Give the plot an image node with this label; without one it has no node.
    pub fn semantics(mut self, label: impl Into<String>) -> Self {
        self.spec.semantics = Some(label.into());
        self
    }

    /// The value the image node describes, for example the latest reading.
    pub fn semantic_value(mut self, value: impl Into<String>) -> Self {
        self.spec.semantic_value = Some(value.into());
        self
    }
}

impl<S: 'static> View<S> for TimeSeriesView<S> {
    type Element = TimeSeriesWidget;

    fn build(&self, _ctx: &mut BuildCtx<'_>) -> TimeSeriesWidget {
        TimeSeriesWidget {
            labels: Corner::ALL.map(|c| self.spec.labels[c.index()].clone().map(Label::new)),
            caches: vec![LayerCache::default(); self.spec.layers.len()],
            spec: self.spec.clone(),
        }
    }

    fn rebuild(
        &self,
        _prev: &Self,
        element: &mut TimeSeriesWidget,
        _ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        let flags = self.spec.changes(&element.spec);
        if !flags.is_empty() {
            element.adopt(self.spec.clone());
        }
        flags
    }
}

/// The inputs a layer's cached geometry was built from.
#[derive(Clone, Debug, PartialEq)]
struct CacheKey {
    revision: u64,
    capacity: usize,
    plot: Rect,
    orientation: Orientation,
    domain: (f32, f32),
    style: PlotStyle,
    stroke_width: f32,
}

#[derive(Clone, Debug, Default)]
struct LayerCache {
    key: Option<CacheKey>,
    fills: Vec<BezPath>,
    strokes: Vec<BezPath>,
    dots: Vec<BezPath>,
    blocks: Vec<BlockColumn>,
}

impl LayerCache {
    /// Rebuild the geometry of `series` unless `key` matches the last build.
    fn refresh(&mut self, series: &Series, key: CacheKey) {
        if self.key.as_ref() == Some(&key) {
            return;
        }
        let CacheKey {
            capacity,
            plot,
            orientation,
            domain,
            style,
            stroke_width,
            ..
        } = key;
        self.fills.clear();
        self.strokes.clear();
        self.dots.clear();
        self.blocks.clear();
        series.with_slots(|slots| match style {
            PlotStyle::Blocks { column_width, gap } => {
                self.blocks = block_columns(
                    slots.iter(),
                    capacity,
                    f64::from(column_width),
                    f64::from(gap),
                    domain,
                    plot,
                    orientation,
                );
            }
            PlotStyle::Area | PlotStyle::Line => {
                if style == PlotStyle::Area {
                    self.fills = area_paths(slots.iter(), capacity, domain, plot, orientation);
                }
                let line = line_segments(slots.iter(), capacity, domain, plot, orientation);
                self.strokes = line.paths();
                let radius = MIN_DOT_DIAMETER.max(2.0 * f64::from(stroke_width)) / 2.0;
                self.dots = line
                    .points
                    .iter()
                    .map(|&p| Circle::new(p, radius).to_path(0.1))
                    .collect();
            }
        });
        self.key = Some(key);
    }
}

/// The cache key behind a label's retained [`TextLayout`]: the style it was shaped at and the
/// [`LocaleStamp`] read before shaping, so a style or default-locale change reshapes it.
#[derive(Default)]
struct LayoutGuard {
    style: Option<TextStyle>,
    stamp: LocaleStamp,
}

impl LayoutGuard {
    fn matches(&self, style: &TextStyle) -> bool {
        self.style.as_ref() == Some(style) && self.stamp.is_current(style.locale.as_deref())
    }
}

/// One corner label: its text, its shaped layout and where it sits.
struct Label {
    text: String,
    layout: Option<TextLayout>,
    guard: LayoutGuard,
    at: Point,
}

impl Label {
    fn new(text: String) -> Self {
        Self {
            text,
            layout: None,
            guard: LayoutGuard::default(),
            at: Point::ZERO,
        }
    }

    /// Shape at `style`, or reuse the last shape when the style and locale are unchanged.
    fn shape(&mut self, ctx: &mut LayoutCtx, style: &TextStyle) -> Size {
        if let Some(layout) = &self.layout
            && self.guard.matches(style)
        {
            return layout.size();
        }
        let stamp = LocaleStamp::for_style(style.locale.as_deref());
        let layout = ctx
            .text_context::<TextContext>()
            .layout(&self.text, style, None);
        let size = layout.size();
        self.layout = Some(layout);
        self.guard = LayoutGuard {
            style: Some(style.clone()),
            stamp,
        };
        size
    }

    fn paint(&self, origin: Point, scene: &mut dyn PaintScene) {
        if let Some(layout) = &self.layout {
            let at = origin + self.at.to_vec2();
            for run in layout.to_scene_runs(at) {
                scene.draw_glyph_run(run);
            }
        }
    }
}

/// The retained widget behind a [`TimeSeriesView`].
pub struct TimeSeriesWidget {
    spec: Spec,
    labels: [Option<Label>; 4],
    caches: Vec<LayerCache>,
}

/// `brush` with a gradient's points moved from widget-local to scene coordinates.
fn at_origin(brush: &Brush, origin: Point) -> Brush {
    let Brush::Gradient(gradient) = brush else {
        return brush.clone();
    };
    let mut gradient = gradient.clone();
    let shift = origin.to_vec2();
    match &mut gradient.kind {
        GradientKind::Linear(p) => {
            p.start += shift;
            p.end += shift;
        }
        GradientKind::Radial(p) => {
            p.start_center += shift;
            p.end_center += shift;
        }
        GradientKind::Sweep(p) => p.center += shift,
    }
    Brush::Gradient(gradient)
}

/// The series and grid colours a paint falls back to.
struct Defaults {
    series: Color,
    grid: Color,
}

impl Defaults {
    fn resolve(theme: Option<&Theme>) -> Self {
        match theme {
            Some(theme) => {
                let scheme = theme.scheme();
                Self {
                    series: scheme.primary,
                    grid: scheme.outline_variant,
                }
            }
            None => Self {
                series: Tone::FALLBACK_CALM,
                grid: FALLBACK_GRID,
            },
        }
    }
}

impl TimeSeriesWidget {
    /// Take a changed spec: relabel and drop every cached path.
    fn adopt(&mut self, spec: Spec) {
        for corner in Corner::ALL {
            let i = corner.index();
            match (&mut self.labels[i], &spec.labels[i]) {
                (Some(label), Some(text)) if label.text == *text => {}
                (slot, text) => *slot = text.clone().map(Label::new),
            }
        }
        self.caches = vec![LayerCache::default(); spec.layers.len()];
        self.spec = spec;
    }

    /// The `(upper, lower)` plot rects: both the whole plot unless mirrored.
    fn halves(&self, size: Size) -> (Rect, Rect) {
        let bounds = Rect::from_origin_size(Point::ZERO, size);
        if !self.spec.mirrored {
            return (bounds, bounds);
        }
        let mid = snap(size.height / 2.0);
        (
            Rect::new(0.0, 0.0, size.width, mid),
            Rect::new(0.0, mid, size.width, size.height),
        )
    }

    /// The `(upper, lower)` domains resolved from the series' current data.
    fn domains(&self) -> ((f32, f32), (f32, f32)) {
        let domain = self.spec.domain;
        let of = |half: Option<Half>| {
            let series: Vec<&Series> = self
                .spec
                .layers
                .iter()
                .filter(|l| half.is_none_or(|h| l.half == h))
                .map(|l| &l.series)
                .collect();
            auto_domain(&series, domain)
        };
        if !self.spec.mirrored || self.spec.shared_domain {
            let all = of(None);
            (all, all)
        } else {
            (of(Some(Half::Upper)), of(Some(Half::Lower)))
        }
    }

    fn paint_grid(&self, origin: Point, size: Size, color: Color, scene: &mut dyn PaintScene) {
        if self.spec.grid.is_empty() {
            return;
        }
        let (upper, lower) = self.halves(size);
        let mut ys = grid_rule_ys(&self.spec.grid, upper);
        if self.spec.mirrored {
            let flipped: Vec<f32> = self.spec.grid.iter().map(|f| 1.0 - f).collect();
            ys.extend(grid_rule_ys(&flipped, lower));
        }
        ys.sort_by(f64::total_cmp);
        ys.dedup();
        for y in ys {
            scene.fill_rect(
                Point::new(origin.x, origin.y + y),
                Size::new(size.width, 1.0),
                color,
            );
        }
    }

    fn paint_layer(
        &mut self,
        index: usize,
        origin: Point,
        plot: (Rect, Orientation, (f32, f32)),
        default_color: Color,
        scene: &mut dyn PaintScene,
    ) {
        let (rect, orientation, domain) = plot;
        let layer = &self.spec.layers[index];
        let style = self.spec.style;
        let key = CacheKey {
            revision: layer.series.revision(),
            capacity: layer.series.capacity(),
            plot: rect,
            orientation,
            domain,
            style,
            stroke_width: layer.style.stroke_width,
        };
        let cache = &mut self.caches[index];
        cache.refresh(&layer.series, key);
        let series_style = &layer.style;
        let color = series_style.color.unwrap_or(default_color);

        if let PlotStyle::Blocks { .. } = style {
            let radius = f64::from(self.spec.corner);
            for column in &cache.blocks {
                let fill = series_style
                    .tone
                    .as_ref()
                    .map_or(color, |tone| tone.color_at(column.value));
                let at = origin + column.rect.origin().to_vec2();
                if radius.is_finite() && radius > 0.0 {
                    scene.fill_rounded_rect(at, column.rect.size(), radius, fill);
                } else {
                    scene.fill_rect(at, column.rect.size(), fill);
                }
            }
            return;
        }

        if style == PlotStyle::Area {
            let fill = match &series_style.fill {
                SeriesFill::Gradient => Some(derived_fill(color, rect, orientation, origin)),
                SeriesFill::Brush(brush) => Some(at_origin(brush, origin)),
                SeriesFill::None => None,
            };
            if let Some(fill) = fill {
                for path in &cache.fills {
                    scene.fill_path(origin, path, &fill);
                }
            }
        }
        let stroke = series_style
            .stroke
            .as_ref()
            .map_or(Brush::Solid(color), |brush| at_origin(brush, origin));
        let width = f64::from(series_style.stroke_width);
        if width > 0.0 {
            for path in &cache.strokes {
                scene.stroke_path(origin, path, width, &stroke);
            }
        }
        for dot in &cache.dots {
            scene.fill_path(origin, dot, &stroke);
        }
    }
}

/// The default fill: `color` at alpha 0.40 at the top of the half's extent, fading to 0.03 at
/// its baseline.
fn derived_fill(color: Color, plot: Rect, orientation: Orientation, origin: Point) -> Brush {
    let (top, base) = match orientation {
        Orientation::Upright => (plot.y0, plot.y1),
        Orientation::Inverted => (plot.y1, plot.y0),
    };
    let gradient = Gradient::new_linear((origin.x, origin.y + top), (origin.x, origin.y + base))
        .with_stops([
            (0.0, color.with_alpha(FILL_ALPHA.0)),
            (1.0, color.with_alpha(FILL_ALPHA.1)),
        ]);
    Brush::Gradient(gradient)
}

impl Widget for TimeSeriesWidget {
    fn layout(&mut self, ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        let max = bc.max();
        let width = if max.width.is_finite() {
            max.width
        } else {
            UNBOUNDED_WIDTH
        };
        let height = match self.spec.height.map(f64::from) {
            Some(height) if height.is_finite() => height.max(0.0),
            _ if max.height.is_finite() => max.height,
            _ => DEFAULT_HEIGHT,
        };
        let size = bc.constrain(Size::new(width, height));
        if self.labels.iter().all(Option::is_none) {
            return size;
        }
        let theme = Theme::from_layout_ctx(ctx);
        let color = self
            .spec
            .label_color
            .unwrap_or_else(|| theme.map_or(FALLBACK_LABEL, |t| t.scheme().on_surface_variant));
        let family = theme.map(|t| t.type_scale.body_small.family.clone());
        let label_size = if self.spec.label_size.is_finite() && self.spec.label_size > 0.0 {
            self.spec.label_size
        } else {
            DEFAULT_LABEL_SIZE
        };
        let style = TextStyle::new(label_size, color).with_family(family.unwrap_or_default());
        for corner in Corner::ALL {
            let Some(label) = &mut self.labels[corner.index()] else {
                continue;
            };
            let text = label.shape(ctx, &style);
            let (inset_x, inset_y) = LABEL_INSET;
            let x = match corner {
                Corner::TopLeft | Corner::BottomLeft => inset_x,
                Corner::TopRight | Corner::BottomRight => size.width - inset_x - text.width,
            };
            let y = match corner {
                Corner::TopLeft | Corner::TopRight => inset_y,
                Corner::BottomLeft | Corner::BottomRight => size.height - inset_y - text.height,
            };
            label.at = Point::new(snap(x), snap(y));
        }
        size
    }

    fn paint(&mut self, ctx: &mut PaintCtx, scene: &mut dyn PaintScene) {
        let (origin, size) = (ctx.origin(), ctx.size());
        if !(size.width > 0.0 && size.height > 0.0) {
            return;
        }
        let defaults = Defaults::resolve(Theme::from_paint_ctx(ctx));
        scene.push_clip(origin, size);
        let grid = self.spec.grid_color.unwrap_or(defaults.grid);
        self.paint_grid(origin, size, grid, scene);
        let (upper, lower) = self.halves(size);
        let (upper_domain, lower_domain) = self.domains();
        for index in 0..self.spec.layers.len() {
            let plot = match self.spec.layers[index].half {
                Half::Lower if self.spec.mirrored => (lower, Orientation::Inverted, lower_domain),
                _ => (upper, Orientation::Upright, upper_domain),
            };
            self.paint_layer(index, origin, plot, defaults.series, scene);
        }
        for label in self.labels.iter().flatten() {
            label.paint(origin, scene);
        }
        scene.pop_clip();
    }

    fn semantics(&self, ctx: &mut SemanticsCtx) {
        let Some(label) = &self.spec.semantics else {
            return;
        };
        ctx.push_node(Role::Image, |node| {
            node.set_label(label.as_str());
            if let Some(value) = &self.spec.semantic_value {
                node.set_description(value.as_str());
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::{CacheKey, LayerCache, Orientation, PlotStyle};
    use crate::Series;
    use frust::authoring::Rect;

    fn key(series: &Series, plot: Rect) -> CacheKey {
        CacheKey {
            revision: series.revision(),
            capacity: series.capacity(),
            plot,
            orientation: Orientation::Upright,
            domain: (0.0, 100.0),
            style: PlotStyle::Area,
            stroke_width: 1.5,
        }
    }

    #[test]
    fn cached_paths_are_rebuilt_only_when_an_input_changes() {
        let series = Series::new(4);
        series.extend([Some(10.0), Some(20.0)]);
        let plot = Rect::new(0.0, 0.0, 100.0, 50.0);
        let mut cache = LayerCache::default();
        cache.refresh(&series, key(&series, plot));
        assert_eq!((cache.fills.len(), cache.strokes.len()), (1, 1));

        // A marker survives a refresh with the same inputs, so nothing was rebuilt.
        cache.strokes.clear();
        cache.refresh(&series, key(&series, plot));
        assert!(cache.strokes.is_empty());

        // A new plot size rebuilds.
        let wide = Rect::new(0.0, 0.0, 200.0, 50.0);
        cache.refresh(&series, key(&series, wide));
        assert_eq!(cache.strokes.len(), 1);

        // So does a write.
        cache.strokes.clear();
        cache.refresh(&series, key(&series, wide));
        assert!(cache.strokes.is_empty());
        series.push(30.0);
        cache.refresh(&series, key(&series, wide));
        assert_eq!(cache.strokes.len(), 1);
    }
}
