//! The `time_series` widget: one or more [`Series`] or [`TimedSeries`](crate::TimedSeries)
//! drawn as an area, a line or block columns, sized to the box it is given.
//!
//! The widget reads its series while painting, so a push shows with no rebuild: the write
//! requests a repaint and the next paint draws the new samples. Built paths are cached per
//! series and rebuilt only when the series' revision, the plot size, the resolved domain, the
//! style or (for a timed series) the window, gap threshold or time span changed. All geometry
//! comes from the crate's geometry functions. A [`Series`] is placed by slot and a
//! [`TimedSeries`](crate::TimedSeries) by time, both against the same plot rect.
//!
//! Draw order: grid rules, then for each series (in the order given) its fill, its stroke and
//! the dots marking isolated samples, then the scrub crosshair and its markers while a scrub is
//! live, then the axis labels, then the corner labels. Everything is clipped to the widget's
//! bounds. A mirrored pair splits the plot at a middle axis: the upper series grows up from it
//! and the lower one hangs down from it.
//!
//! An optional [`YAxis`] draws its rules at [`ticks`] of each half's resolved domain, in place
//! of the grid fractions, and a label per tick. Text is shaped only in layout, but an automatic
//! domain is resolved at paint, on frames that skip layout. So a paint compares the ticks it
//! resolves with the ones the labels were laid out for; when they differ it calls
//! `PaintCtx::request_layout` once for that tick set and draws this frame with the labels it
//! has, each at its own value (one whose value left the domain is skipped). The next layout
//! shapes the new set. A gutter, when asked for, narrows the plot by the widest label.
//!
//! Colours: the caller's, then the installed theme's (series `primary`, grid
//! `outline_variant`, labels `on_surface_variant`), then fallback constants. Labels are shaped
//! in layout with the theme's `body_small` family, so a theme change needs a layout pass. The
//! chart never animates; the only frame it asks for is the layout above.
//!
//! Input: none by default, so the chart is tap-transparent. With
//! [`on_scrub`](TimeSeriesView::on_scrub) set, a primary press inside the plot starts a scrub:
//! the widget captures the pointer, reports a [`ScrubEvent`] on the press and on every move,
//! and reports `None` when the press ends. Each paint during a scrub draws a vertical crosshair
//! at the sample nearest the pointer in the series' current data, with a marker dot on each
//! series' sample there (see [`crate::ScrubEvent`] for how the sample is found).

use std::marker::PhantomData;
use std::rc::Rc;

use frust::authoring::text::{LocaleStamp, TextContext, TextLayout, TextStyle};
use frust::authoring::{
    BezPath, BoxConstraints, Brush, BuildCtx, ChangeFlags, ErasedArgCallback, EventCtx,
    EventResult, InputEvent, LayoutCtx, PaintCtx, PaintScene, Point, PointerButton, PointerEvent,
    PointerPhase, Rect, Role, SemanticsCtx, Shape, Size, View, Widget, erase_callback_arg,
    mark_pending_result_flush,
};
use frust::kurbo::Circle;
use frust::peniko::{Gradient, GradientKind};
use frust::{Color, Theme};

use crate::axis::{AxisSide, YAxis, ticks};
use crate::geometry::{
    BlockColumn, Orientation, area_paths, block_columns, grid_rule_ys, line_segments,
    smooth_area_paths, smooth_timed_area_paths, snap, snap_rule_y, timed_area_paths,
    timed_line_segments, tone_gradient,
};
use crate::scrub::{Placement, ScrubEvent, scrub_at};
use crate::timed::SourceShape;
use crate::{Series, SeriesSource, Tone, YDomain};

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
const DEFAULT_CROSSHAIR_WIDTH: f32 = 1.0;
/// The radius of the dot marking each series' sample on the crosshair.
const MARKER_RADIUS: f64 = 3.5;

/// How the samples of every series are drawn.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum PlotStyle {
    /// A filled area under a line (the default).
    #[default]
    Area,
    /// A line alone; a series' fill is ignored.
    Line,
    /// One column per present sample, right-aligned, `column_width` wide with `gap` between
    /// columns (both snapped to whole pixels). A [`TimedSeries`](crate::TimedSeries) has no
    /// columns: it draws as [`Line`](Self::Line).
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
    tone_along: Option<Tone>,
}

impl Default for SeriesStyle {
    fn default() -> Self {
        Self {
            color: None,
            stroke: None,
            stroke_width: DEFAULT_STROKE_WIDTH,
            fill: SeriesFill::Gradient,
            tone: None,
            tone_along: None,
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

impl SeriesStyle {
    /// Colour a line or area by value through `tone`: the stroke is a vertical gradient over
    /// the plot's y extent with a hard colour change at each threshold, and the derived area
    /// fill takes the same gradient at reduced alpha. An explicit [`stroke`](Self::stroke)
    /// brush wins for the stroke, and an explicit [`fill`](Self::fill) or
    /// [`no_fill`](Self::no_fill) for the area. Block columns ignore it (use
    /// [`tone`](Self::tone)).
    pub fn tone_along_value(mut self, tone: Tone) -> Self {
        self.tone_along = Some(tone);
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

/// [`auto_domain`] over sources of either kind.
fn source_domain<'a>(
    sources: impl Iterator<Item = &'a SeriesSource>,
    domain: YDomain,
) -> (f32, f32) {
    let max = sources.filter_map(SeriesSource::max).reduce(f32::max);
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
    series: SeriesSource,
    style: SeriesStyle,
    half: Half,
}

impl Layer {
    fn same(&self, other: &Self) -> bool {
        self.series.ptr_eq(&other.series) && self.style == other.style && self.half == other.half
    }

    /// How this layer draws under the plot's `style`: a timed series has no block columns,
    /// so it falls back to a line.
    fn style_under(&self, style: PlotStyle) -> PlotStyle {
        match style {
            PlotStyle::Blocks { .. } if self.series.is_timed() => PlotStyle::Line,
            style => style,
        }
    }

    /// Where this layer's slots sit under the plot's `style`, for the scrub search.
    fn placement(&self, style: PlotStyle) -> Placement {
        match self.style_under(style) {
            PlotStyle::Blocks { column_width, gap } => Placement::Columns {
                width: f64::from(column_width),
                gap: f64::from(gap),
            },
            PlotStyle::Area | PlotStyle::Line => Placement::Slots,
        }
    }
}

#[derive(Clone, Debug)]
struct Spec {
    layers: Vec<Layer>,
    mirrored: bool,
    shared_domain: bool,
    domain: YDomain,
    style: PlotStyle,
    smooth: bool,
    corner: f32,
    grid: Vec<f32>,
    grid_color: Option<Color>,
    labels: [Option<String>; 4],
    label_size: f32,
    label_color: Option<Color>,
    y_axis: Option<YAxis>,
    crosshair: (Option<Color>, f32),
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
            || self.y_axis != old.y_axis
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
            || self.smooth != old.smooth
            || self.corner != old.corner
            || self.grid != old.grid
            || self.grid_color != old.grid_color
            || self.crosshair != old.crosshair
            || self.semantics != old.semantics
            || self.semantic_value != old.semantic_value
        {
            return ChangeFlags::PAINT;
        }
        ChangeFlags::NONE
    }
}

/// A view-held scrub callback, erased on build.
type OnScrub<S> = Rc<dyn Fn(&mut S, Option<ScrubEvent>)>;

/// A time-series plot, display-only unless [`on_scrub`](Self::on_scrub) is set. Build one with
/// [`time_series`].
pub struct TimeSeriesView<S: 'static> {
    spec: Spec,
    on_scrub: Option<OnScrub<S>>,
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
            smooth: false,
            corner: 0.0,
            grid: Vec::new(),
            grid_color: None,
            labels: Default::default(),
            label_size: DEFAULT_LABEL_SIZE,
            label_color: None,
            y_axis: None,
            crosshair: (None, DEFAULT_CROSSHAIR_WIDTH),
            height: None,
            semantics: None,
            semantic_value: None,
        },
        on_scrub: None,
        _state: PhantomData,
    }
}

impl<S: 'static> TimeSeriesView<S> {
    /// Add a series, a [`Series`] or a [`TimedSeries`](crate::TimedSeries); series draw in the
    /// order added, later ones on top. In a mirrored plot it joins the upper half.
    pub fn series(mut self, series: impl Into<SeriesSource>, style: SeriesStyle) -> Self {
        self.spec.layers.push(Layer {
            series: series.into(),
            style,
            half: Half::Upper,
        });
        self
    }

    /// Split the plot at a middle axis: `upper` grows up from it and `lower` hangs down from
    /// it. Each half resolves its own domain unless [`shared_domain`](Self::shared_domain).
    /// Either half takes a [`Series`] or a [`TimedSeries`](crate::TimedSeries).
    pub fn mirrored(
        mut self,
        upper: (impl Into<SeriesSource>, SeriesStyle),
        lower: (impl Into<SeriesSource>, SeriesStyle),
    ) -> Self {
        self.spec.mirrored = true;
        let (upper, lower) = ((upper.0.into(), upper.1), (lower.0.into(), lower.1));
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

    /// Draw line and area edges as a monotone cubic curve through the samples instead of
    /// straight segments (default `false`). The curve passes through every sample and never
    /// overshoots between two; runs and gaps break as before, and a run of two is a straight
    /// segment.
    pub fn smooth(mut self, smooth: bool) -> Self {
        self.spec.smooth = smooth;
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

    /// An automatic y axis (default none): a rule and a label at each of the
    /// [`ticks`] of the resolved domain, computed per half in a mirrored plot with the shared
    /// middle axis labelled once. Its rules replace the [`grid`](Self::grid) fractions and use
    /// the [`grid_color`](Self::grid_color).
    ///
    /// The labels follow an automatic domain as data arrives. They are shaped in layout, so
    /// when a write moves the ticks, the paint that sees it asks for one layout pass and
    /// draws the labels it already has until that pass shapes the new ones; nice-number
    /// domains keep this rare. A fixed domain never asks.
    pub fn y_axis(mut self, axis: YAxis) -> Self {
        self.spec.y_axis = Some(axis);
        self
    }

    /// Report a scrub to `on_scrub` (default: none, and the chart takes no input).
    ///
    /// With it set, a primary press inside the plot (not in an axis gutter) captures the
    /// pointer and calls `on_scrub` with `Some(event)` on the press and on every move while
    /// it is held, each [`ScrubEvent`] naming the sample nearest the pointer and every
    /// series' value there; the release calls it with `None`. A move with no press is never a
    /// scrub. Between the press and the release, each paint draws a crosshair (see
    /// [`crosshair`](Self::crosshair)). The app renders its own readout from the events.
    ///
    /// When a scroll view or another gesture takes the drag away (a `Cancel`), the chart
    /// stops drawing the crosshair at once and calls `on_scrub(None)` from the
    /// `Housekeeping` broadcast of the rebuild that follows, still before the frame paints:
    /// a `Cancel` handler never touches app state.
    ///
    /// The plot is no longer tap-transparent: a press inside it is handled here and does not
    /// reach what lies beneath. A press does not take keyboard focus. Inside a vertical
    /// scroll view, a sideways drag stays with the chart, while a drag that travels more
    /// than the touch slop (18 px) vertically first is taken by the scroll view, which ends
    /// the scrub. Inside a horizontal scroll view it is the other way round: the scroll view
    /// takes a drag once it travels more than the slop sideways, so a scrub there ends after
    /// 18 px of horizontal travel.
    pub fn on_scrub(mut self, on_scrub: impl Fn(&mut S, Option<ScrubEvent>) + 'static) -> Self {
        self.on_scrub = Some(Rc::new(on_scrub));
        self
    }

    /// The scrub crosshair's colour and width in logical pixels (default: the theme's
    /// `on_surface_variant` and 1); a width of 0 draws only the markers. A negative or
    /// non-finite width is 0. Each series' marker dot takes the series' own colour.
    pub fn crosshair(mut self, color: Color, width: f32) -> Self {
        let width = if width.is_finite() {
            width.max(0.0)
        } else {
            0.0
        };
        self.spec.crosshair = (Some(color), width);
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
            axis: AxisLabels::default(),
            spec: self.spec.clone(),
            on_scrub: self.on_scrub.as_ref().map(erase_callback_arg),
            scrub: None,
            end_owed: false,
        }
    }

    fn rebuild(
        &self,
        _prev: &Self,
        element: &mut TimeSeriesWidget,
        _ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        let mut flags = self.spec.changes(&element.spec);
        if !flags.is_empty() {
            element.adopt(self.spec.clone());
        }
        element.on_scrub = self.on_scrub.as_ref().map(erase_callback_arg);
        if element.on_scrub.is_none() {
            // Without a callback there is no scrub to draw or to end.
            element.end_owed = false;
            if element.scrub.take().is_some() {
                flags |= ChangeFlags::PAINT;
            }
        }
        flags
    }
}

/// The inputs a layer's cached geometry was built from.
#[derive(Clone, Debug, PartialEq)]
struct CacheKey {
    revision: u64,
    /// The capacity of a series, or the window, gap threshold and time span of a timed one.
    source: SourceShape,
    plot: Rect,
    orientation: Orientation,
    domain: (f32, f32),
    style: PlotStyle,
    smooth: bool,
    stroke_width: f32,
}

/// The inputs a tone gradient was built from.
#[derive(Clone, Debug, PartialEq)]
struct ToneKey {
    tone: Tone,
    plot: Rect,
    orientation: Orientation,
    domain: (f32, f32),
}

/// A tone's stroke and fill brushes in widget-local coordinates.
#[derive(Clone, Debug)]
struct ToneBrushes {
    key: ToneKey,
    stroke: Brush,
    fill: Brush,
}

impl ToneBrushes {
    fn build(key: ToneKey) -> Self {
        let make = |alpha: f32| {
            let g = tone_gradient(&key.tone, key.domain, key.plot, key.orientation, alpha);
            Brush::Gradient(
                Gradient::new_linear((0.0, g.start_y), (0.0, g.end_y))
                    .with_stops(g.stops.as_slice()),
            )
        };
        Self {
            stroke: make(1.0),
            fill: make(FILL_ALPHA.0),
            key,
        }
    }
}

#[derive(Clone, Debug, Default)]
struct LayerCache {
    key: Option<CacheKey>,
    tone: Option<ToneBrushes>,
    fills: Vec<BezPath>,
    strokes: Vec<BezPath>,
    dots: Vec<BezPath>,
    blocks: Vec<BlockColumn>,
}

impl LayerCache {
    /// Rebuild the geometry of `source` unless `key` matches the last build. `key.style` is
    /// the layer's own (a timed source never carries block columns).
    fn refresh(&mut self, source: &SeriesSource, key: CacheKey) {
        if self.key.as_ref() == Some(&key) {
            return;
        }
        let CacheKey {
            plot,
            orientation,
            domain,
            style,
            smooth,
            stroke_width,
            ..
        } = key;
        self.fills.clear();
        self.strokes.clear();
        self.dots.clear();
        self.blocks.clear();
        let area = style == PlotStyle::Area;
        let line = match source {
            SeriesSource::Series(series) => {
                let capacity = series.capacity();
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
                        None
                    }
                    PlotStyle::Area | PlotStyle::Line => {
                        if area {
                            let build = if smooth {
                                smooth_area_paths
                            } else {
                                area_paths
                            };
                            self.fills = build(slots.iter(), capacity, domain, plot, orientation);
                        }
                        Some(line_segments(
                            slots.iter(),
                            capacity,
                            domain,
                            plot,
                            orientation,
                        ))
                    }
                })
            }
            SeriesSource::Timed(series) => {
                let (window, gap_after) = (series.window_secs(), series.gap_after_secs());
                series.with_points(|points| {
                    if area {
                        let build = if smooth {
                            smooth_timed_area_paths
                        } else {
                            timed_area_paths
                        };
                        self.fills = build(points, window, gap_after, domain, plot, orientation);
                    }
                    Some(timed_line_segments(
                        points,
                        window,
                        gap_after,
                        domain,
                        plot,
                        orientation,
                    ))
                })
            }
        };
        if let Some(line) = line {
            self.strokes = if smooth {
                line.smooth_paths()
            } else {
                line.paths()
            };
            let radius = MIN_DOT_DIAMETER.max(2.0 * f64::from(stroke_width)) / 2.0;
            self.dots = line
                .points
                .iter()
                .map(|&p| Circle::new(p, radius).to_path(0.1))
                .collect();
        }
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

    /// The shaped size; zero before the first shape.
    fn size(&self) -> Size {
        self.layout.as_ref().map_or(Size::ZERO, TextLayout::size)
    }

    fn paint(&self, origin: Point, scene: &mut dyn PaintScene) {
        self.draw(origin + self.at.to_vec2(), scene);
    }

    /// Draw the shaped text with its top-left at `at` (scene coordinates).
    fn draw(&self, at: Point, scene: &mut dyn PaintScene) {
        if let Some(layout) = &self.layout {
            for run in layout.to_scene_runs(at) {
                scene.draw_glyph_run(run);
            }
        }
    }
}

/// One axis tick: its value and the half of the plot it belongs to.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Tick {
    half: Half,
    value: f32,
}

/// The axis labels as last laid out, and the tick set a paint last asked a layout for.
#[derive(Default)]
struct AxisLabels {
    /// The ticks the labels were shaped for, one per label.
    ticks: Vec<Tick>,
    /// The shaped labels; only `at.x` is laid out, a paint places each at its tick's y.
    labels: Vec<Label>,
    /// The tick set a paint asked a layout for, until a layout shapes one, so a paint asks at
    /// most once per changed set.
    requested: Option<Vec<Tick>>,
    /// The width reserved beside the plot: 0 without a gutter.
    gutter: f64,
}

/// The `(upper, lower)` resolved domains; both the same unless a mirrored plot resolves each
/// half on its own.
type Domains = ((f32, f32), (f32, f32));

/// A half's rect, orientation and domain: what a tick or a layer in it is drawn against.
type HalfPlot = (Rect, Orientation, (f32, f32));

/// The top of the 1 px rule at `value`, kept on whole pixel rows inside its half.
fn rule_y(value: f32, (rect, orientation, domain): HalfPlot) -> f64 {
    let top = snap(rect.y0);
    let bottom = (snap(rect.y1) - 1.0).max(top);
    snap_rule_y(orientation.y_for(value, domain, rect)).clamp(top, bottom)
}

/// The retained widget behind a [`TimeSeriesView`].
pub struct TimeSeriesWidget {
    spec: Spec,
    labels: [Option<Label>; 4],
    caches: Vec<LayerCache>,
    axis: AxisLabels,
    on_scrub: Option<ErasedArgCallback<Option<ScrubEvent>>>,
    /// The pointer position (widget-local) of the live scrub: set by a primary press inside
    /// the plot, moved by each captured move, cleared by the release or a cancel.
    scrub: Option<Point>,
    /// A cancel ended the scrub and `on_scrub(None)` is still owed, delivered from the next
    /// `Housekeeping` broadcast (or ahead of the next press).
    end_owed: bool,
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

/// The series, grid and crosshair colours a paint falls back to.
struct Defaults {
    series: Color,
    grid: Color,
    crosshair: Color,
}

impl Defaults {
    fn resolve(theme: Option<&Theme>) -> Self {
        match theme {
            Some(theme) => {
                let scheme = theme.scheme();
                Self {
                    series: scheme.primary,
                    grid: scheme.outline_variant,
                    crosshair: scheme.on_surface_variant,
                }
            }
            None => Self {
                series: Tone::FALLBACK_CALM,
                grid: FALLBACK_GRID,
                crosshair: FALLBACK_LABEL,
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

    /// The plot: the widget's bounds less the axis gutter, if any.
    fn plot(&self, size: Size) -> Rect {
        let gutter = match &self.spec.y_axis {
            Some(axis) if axis.gutter => self.axis.gutter.clamp(0.0, size.width.max(0.0)),
            _ => 0.0,
        };
        match self.spec.y_axis.as_ref().map(|axis| axis.side) {
            Some(AxisSide::Right) => Rect::new(0.0, 0.0, size.width - gutter, size.height),
            _ => Rect::new(gutter, 0.0, size.width, size.height),
        }
    }

    /// The `(upper, lower)` plot rects: both the whole plot unless mirrored.
    fn halves(&self, size: Size) -> (Rect, Rect) {
        let plot = self.plot(size);
        if !self.spec.mirrored {
            return (plot, plot);
        }
        let mid = snap(size.height / 2.0);
        (
            Rect::new(plot.x0, 0.0, plot.x1, mid),
            Rect::new(plot.x0, mid, plot.x1, size.height),
        )
    }

    /// What `half` is drawn against, given the `(upper, lower)` rects and domains.
    fn half_plot(
        &self,
        half: Half,
        (upper, lower): (Rect, Rect),
        (upper_domain, lower_domain): Domains,
    ) -> HalfPlot {
        match half {
            Half::Lower if self.spec.mirrored => (lower, Orientation::Inverted, lower_domain),
            _ => (upper, Orientation::Upright, upper_domain),
        }
    }

    /// The axis ticks for the `(upper, lower)` domains: none without an axis, else each half's
    /// own, with a mirrored plot's shared middle axis ticked once (by the upper half when it
    /// has a tick there).
    fn tick_set(&self, (upper, lower): Domains) -> Vec<Tick> {
        let Some(axis) = &self.spec.y_axis else {
            return Vec::new();
        };
        let mut set: Vec<Tick> = ticks(upper.0, upper.1, axis.tick_count)
            .into_iter()
            .map(|value| Tick {
                half: Half::Upper,
                value,
            })
            .collect();
        if self.spec.mirrored {
            let upper_on_axis = set.first().is_some_and(|tick| tick.value == upper.0);
            set.extend(
                ticks(lower.0, lower.1, axis.tick_count)
                    .into_iter()
                    .filter(|&value| !(upper_on_axis && value == lower.0))
                    .map(|value| Tick {
                        half: Half::Lower,
                        value,
                    }),
            );
        }
        set
    }

    /// Shape a label per tick of the current domains (reusing a label whose text is
    /// unchanged), measure the gutter and set each label's x. Without an axis, or without a
    /// `style`, drop every axis label.
    fn layout_axis(&mut self, ctx: &mut LayoutCtx, size: Size, style: Option<&TextStyle>) {
        let Some((axis, style)) = self.spec.y_axis.as_ref().zip(style) else {
            self.axis = AxisLabels::default();
            return;
        };
        let ticks = self.tick_set(self.domains());
        let mut old = std::mem::take(&mut self.axis.labels);
        let mut labels = Vec::with_capacity(ticks.len());
        let mut widest: f64 = 0.0;
        for tick in &ticks {
            let text = axis.format.format(tick.value);
            let mut label = match old.iter().position(|label| label.text == text) {
                Some(i) => old.swap_remove(i),
                None => Label::new(text),
            };
            widest = widest.max(label.shape(ctx, style).width);
            labels.push(label);
        }
        let (inset_x, _) = LABEL_INSET;
        let gutter = if axis.gutter && !labels.is_empty() {
            (widest + 2.0 * inset_x).ceil().min(size.width.max(0.0))
        } else {
            0.0
        };
        for label in &mut labels {
            let width = label.size().width;
            let x = match (axis.side, axis.gutter) {
                (AxisSide::Left, false) => inset_x,
                (AxisSide::Right, false) => size.width - inset_x - width,
                (AxisSide::Left, true) => gutter - inset_x - width,
                (AxisSide::Right, true) => size.width - gutter + inset_x,
            };
            label.at = Point::new(snap(x), 0.0);
        }
        self.axis = AxisLabels {
            ticks,
            labels,
            requested: None,
            gutter,
        };
    }

    /// Draw each laid-out axis label centred on its tick's rule under the current domains,
    /// kept inside the widget's height; a label whose value left its half's domain is skipped.
    fn paint_axis_labels(
        &self,
        origin: Point,
        size: Size,
        domains: Domains,
        scene: &mut dyn PaintScene,
    ) {
        let halves = self.halves(size);
        let (_, inset_y) = LABEL_INSET;
        for (tick, label) in self.axis.ticks.iter().zip(&self.axis.labels) {
            let plot = self.half_plot(tick.half, halves, domains);
            let (rect, orientation, domain) = plot;
            if !(domain.0..=domain.1).contains(&tick.value) {
                continue;
            }
            let height = label.size().height;
            let centre = orientation.y_for(tick.value, domain, rect);
            let top = (centre - height / 2.0)
                .min(size.height - inset_y - height)
                .max(inset_y);
            let at = Point::new(origin.x + label.at.x, origin.y + snap(top));
            label.draw(at, scene);
        }
    }

    /// The `(upper, lower)` domains resolved from the series' current data.
    fn domains(&self) -> Domains {
        let domain = self.spec.domain;
        let of = |half: Option<Half>| {
            let sources = self
                .spec
                .layers
                .iter()
                .filter(|l| half.is_none_or(|h| l.half == h))
                .map(|l| &l.series);
            source_domain(sources, domain)
        };
        if !self.spec.mirrored || self.spec.shared_domain {
            let all = of(None);
            (all, all)
        } else {
            (of(Some(Half::Upper)), of(Some(Half::Lower)))
        }
    }

    /// The horizontal rules: one per axis tick when there is an axis, else one per grid
    /// fraction (in each half of a mirrored plot), each across the plot's width.
    fn paint_grid(
        &self,
        origin: Point,
        size: Size,
        (ticks, domains): (&[Tick], Domains),
        color: Color,
        scene: &mut dyn PaintScene,
    ) {
        let plot = self.plot(size);
        if plot.width() <= 0.0 || plot.width().is_nan() {
            return;
        }
        let (upper, lower) = self.halves(size);
        let mut ys = if self.spec.y_axis.is_some() {
            ticks
                .iter()
                .map(|tick| {
                    rule_y(
                        tick.value,
                        self.half_plot(tick.half, (upper, lower), domains),
                    )
                })
                .collect()
        } else {
            let mut ys = grid_rule_ys(&self.spec.grid, upper);
            if self.spec.mirrored {
                let flipped: Vec<f32> = self.spec.grid.iter().map(|f| 1.0 - f).collect();
                ys.extend(grid_rule_ys(&flipped, lower));
            }
            ys
        };
        ys.sort_by(f64::total_cmp);
        ys.dedup();
        for y in ys {
            scene.fill_rect(
                Point::new(origin.x + plot.x0, origin.y + y),
                Size::new(plot.width(), 1.0),
                color,
            );
        }
    }

    /// The scrub at the widget-local `position` against the series' current data.
    fn scrub_at(&self, size: Size, position: Point) -> ScrubEvent {
        let style = self.spec.style;
        let layers = self
            .spec
            .layers
            .iter()
            .map(move |layer| (&layer.series, layer.placement(style)));
        scrub_at(layers, self.plot(size), position)
    }

    /// Whether a press at the widget-local `position` lands inside the plot.
    fn in_plot(&self, size: Size, position: Point) -> bool {
        let plot = self.plot(size);
        plot.width() > 0.0
            && (plot.x0..=plot.x1).contains(&position.x)
            && (plot.y0..=plot.y1).contains(&position.y)
    }

    fn report(&mut self, ctx: &mut EventCtx, event: Option<ScrubEvent>) {
        if let Some(on_scrub) = self.on_scrub.as_mut() {
            on_scrub(ctx, event);
        }
    }

    /// A pointer event while `on_scrub` is set.
    fn scrub_pointer(&mut self, ctx: &mut EventCtx, p: &PointerEvent) -> EventResult {
        match p.phase {
            PointerPhase::Down => {
                if p.button != PointerButton::Primary || !self.in_plot(ctx.size(), p.position) {
                    return EventResult::Ignored;
                }
                if std::mem::take(&mut self.end_owed) {
                    self.report(ctx, None);
                }
                ctx.capture_pointer();
                self.scrub = Some(p.position);
                let event = self.scrub_at(ctx.size(), p.position);
                self.report(ctx, Some(event));
            }
            PointerPhase::Move => {
                if self.scrub.is_none() {
                    return EventResult::Ignored;
                }
                self.scrub = Some(p.position);
                let event = self.scrub_at(ctx.size(), p.position);
                self.report(ctx, Some(event));
            }
            PointerPhase::Up => {
                if self.scrub.take().is_none() {
                    return EventResult::Ignored;
                }
                self.report(ctx, None);
            }
            PointerPhase::Cancel => {
                // A cancel may arrive over throwaway state, so it never calls back: the end is
                // owed to the next state-bearing broadcast.
                if self.scrub.take().is_none() {
                    return EventResult::Ignored;
                }
                self.end_owed = true;
                mark_pending_result_flush();
            }
        }
        ctx.request_redraw();
        EventResult::Handled
    }

    /// The crosshair at the live scrub's sample and a marker on each series' sample there.
    fn paint_scrub(
        &self,
        origin: Point,
        size: Size,
        (position, domains): (Point, Domains),
        defaults: &Defaults,
        scene: &mut dyn PaintScene,
    ) {
        let plot = self.plot(size);
        if plot.width() <= 0.0 || plot.width().is_nan() {
            return;
        }
        let event = self.scrub_at(size, position);
        let (color, width) = self.spec.crosshair;
        let width = f64::from(width);
        if width > 0.0 {
            scene.fill_rect(
                Point::new(origin.x + snap(event.x - width / 2.0), origin.y + plot.y0),
                Size::new(width, plot.height()),
                color.unwrap_or(defaults.crosshair),
            );
        }
        let halves = self.halves(size);
        for (layer, value) in self.spec.layers.iter().zip(&event.values) {
            let Some(value) = *value else {
                continue;
            };
            let (rect, orientation, domain) = self.half_plot(layer.half, halves, domains);
            let centre = Point::new(event.x, orientation.y_for(value, domain, rect));
            let dot = Circle::new(origin + centre.to_vec2(), MARKER_RADIUS).to_path(0.1);
            let color = layer.style.color.unwrap_or(defaults.series);
            scene.fill_path(Point::ZERO, &dot, &Brush::Solid(color));
        }
    }

    fn paint_layer(
        &mut self,
        index: usize,
        origin: Point,
        plot: HalfPlot,
        default_color: Color,
        scene: &mut dyn PaintScene,
    ) {
        let (rect, orientation, domain) = plot;
        let layer = &self.spec.layers[index];
        let style = layer.style_under(self.spec.style);
        let (revision, source) = layer.series.shape();
        let key = CacheKey {
            revision,
            source,
            plot: rect,
            orientation,
            domain,
            style,
            smooth: self.spec.smooth,
            stroke_width: layer.style.stroke_width,
        };
        let cache = &mut self.caches[index];
        cache.refresh(&layer.series, key);
        match &layer.style.tone_along {
            Some(tone) => {
                let key = ToneKey {
                    tone: tone.clone(),
                    plot: rect,
                    orientation,
                    domain,
                };
                if cache.tone.as_ref().map(|b| &b.key) != Some(&key) {
                    cache.tone = Some(ToneBrushes::build(key));
                }
            }
            None => cache.tone = None,
        }
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
                SeriesFill::Gradient => Some(match &cache.tone {
                    Some(tone) => at_origin(&tone.fill, origin),
                    None => derived_fill(color, rect, orientation, origin),
                }),
                SeriesFill::Brush(brush) => Some(at_origin(brush, origin)),
                SeriesFill::None => None,
            };
            if let Some(fill) = fill {
                for path in &cache.fills {
                    scene.fill_path(origin, path, &fill);
                }
            }
        }
        let stroke = match (&series_style.stroke, &cache.tone) {
            (Some(brush), _) => at_origin(brush, origin),
            (None, Some(tone)) => at_origin(&tone.stroke, origin),
            (None, None) => Brush::Solid(color),
        };
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
        if self.labels.iter().all(Option::is_none) && self.spec.y_axis.is_none() {
            self.axis = AxisLabels::default();
            return size;
        }
        let theme = Theme::from_layout_ctx(ctx);
        let color = self
            .spec
            .label_color
            .unwrap_or_else(|| theme.map_or(FALLBACK_LABEL, |t| t.scheme().on_surface_variant));
        let family = theme
            .map(|t| t.type_scale.body_small.family.clone())
            .unwrap_or_default();
        let usable = |size: f32| size.is_finite() && size > 0.0;
        let label_size = if usable(self.spec.label_size) {
            self.spec.label_size
        } else {
            DEFAULT_LABEL_SIZE
        };
        let axis_style = self.spec.y_axis.as_ref().map(|axis| {
            let size = axis.size.filter(|&s| usable(s)).unwrap_or(label_size);
            TextStyle::new(size, axis.color.unwrap_or(color)).with_family(family.clone())
        });
        let style = TextStyle::new(label_size, color).with_family(family);
        self.layout_axis(ctx, size, axis_style.as_ref());
        let plot = self.plot(size);
        for corner in Corner::ALL {
            let Some(label) = &mut self.labels[corner.index()] else {
                continue;
            };
            let text = label.shape(ctx, &style);
            let (inset_x, inset_y) = LABEL_INSET;
            let x = match corner {
                Corner::TopLeft | Corner::BottomLeft => plot.x0 + inset_x,
                Corner::TopRight | Corner::BottomRight => plot.x1 - inset_x - text.width,
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
        let domains = self.domains();
        let ticks = self.tick_set(domains);
        if ticks != self.axis.ticks && self.axis.requested.as_ref() != Some(&ticks) {
            // The labels are shaped for other ticks: ask once for a layout to shape these.
            ctx.request_layout();
            self.axis.requested = Some(ticks.clone());
        }
        let defaults = Defaults::resolve(Theme::from_paint_ctx(ctx));
        scene.push_clip(origin, size);
        let grid = self.spec.grid_color.unwrap_or(defaults.grid);
        self.paint_grid(origin, size, (&ticks, domains), grid, scene);
        let halves = self.halves(size);
        for index in 0..self.spec.layers.len() {
            let plot = self.half_plot(self.spec.layers[index].half, halves, domains);
            self.paint_layer(index, origin, plot, defaults.series, scene);
        }
        if let Some(position) = self.scrub {
            self.paint_scrub(origin, size, (position, domains), &defaults, scene);
        }
        self.paint_axis_labels(origin, size, domains, scene);
        for label in self.labels.iter().flatten() {
            label.paint(origin, scene);
        }
        scene.pop_clip();
    }

    fn event(&mut self, ctx: &mut EventCtx, event: &InputEvent) -> EventResult {
        if self.on_scrub.is_none() {
            return EventResult::Ignored;
        }
        match event {
            InputEvent::Pointer(p) => self.scrub_pointer(ctx, p),
            InputEvent::Housekeeping => {
                if std::mem::take(&mut self.end_owed) {
                    self.report(ctx, None);
                }
                EventResult::Ignored
            }
            _ => EventResult::Ignored,
        }
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
    use crate::{Series, SeriesSource, TimedSeries};
    use frust::authoring::Rect;

    fn key(series: &SeriesSource, plot: Rect) -> CacheKey {
        let (revision, source) = series.shape();
        CacheKey {
            revision,
            source,
            plot,
            orientation: Orientation::Upright,
            domain: (0.0, 100.0),
            style: PlotStyle::Area,
            smooth: false,
            stroke_width: 1.5,
        }
    }

    #[test]
    fn cached_paths_are_rebuilt_only_when_an_input_changes() {
        let handle = Series::new(4);
        handle.extend([Some(10.0), Some(20.0)]);
        let series = SeriesSource::from(handle.clone());
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
        handle.push(30.0);
        cache.refresh(&series, key(&series, wide));
        assert_eq!(cache.strokes.len(), 1);
    }

    #[test]
    fn a_timed_cache_is_rebuilt_when_its_timing_changes() {
        let handle = TimedSeries::new(8);
        handle.extend([(0.0, 10.0), (1.0, 20.0), (5.0, 30.0)]);
        let series = SeriesSource::from(handle.clone());
        let plot = Rect::new(0.0, 0.0, 100.0, 50.0);
        let mut cache = LayerCache::default();
        cache.refresh(&series, key(&series, plot));
        assert_eq!(cache.strokes.len(), 1);

        cache.strokes.clear();
        cache.refresh(&series, key(&series, plot));
        assert!(cache.strokes.is_empty(), "same inputs reuse the build");

        // A gap threshold splits the line: the key's timing moves and the geometry follows.
        let before = key(&series, plot);
        let _handle = handle.gap_after(2.0);
        let after = key(&series, plot);
        assert_ne!(before.source, after.source);
        cache.refresh(&series, after);
        assert_eq!((cache.strokes.len(), cache.dots.len()), (1, 1));
    }
}
