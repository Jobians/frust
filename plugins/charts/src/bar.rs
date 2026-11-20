//! The `bar_chart` widget: categorical bars over a [`Categories`](crate::Categories) list,
//! grouped or stacked, vertical or horizontal, with category labels and a value axis.
//!
//! Each row of the list is one category and each of its values one bar; a gap draws nothing.
//! Grouped bars stand side by side in their category's slot, stacked ones pile up from the
//! zero line, positive values one way and negative values the other. Every bar edge sits on a
//! whole logical pixel, and a non-zero value draws at least one pixel. Built bars are cached and
//! rebuilt only when the list's revision, the plot rect or the resolved domain changes, so a
//! write to the list repaints the chart with no rebuild.
//!
//! Text is shaped only in layout. Category labels follow the list's
//! [`label_revision`](crate::Categories::label_revision), and value-axis labels the ticks of
//! the domain, which an automatic domain resolves from the data at every paint. A paint that
//! finds either changed since the last layout calls `PaintCtx::request_layout` once for that
//! change and draws this frame with the labels it has.
//!
//! Draw order: grid or axis rules, the bars (row by row, column by column), the value-axis
//! labels, then the category labels, all clipped to the widget's bounds. Colours: the caller's,
//! then the installed theme's (columns `primary`, `secondary`, `tertiary`, rules
//! `outline_variant`, labels `on_surface_variant`), then the fallback constants on
//! [`Tone`](crate::Tone). The chart never animates and takes no input.

use frust::authoring::text::TextStyle;
use frust::authoring::{
    BoxConstraints, BuildCtx, ChangeFlags, CornerRadii, LayoutCtx, PaintCtx, PaintScene, Point,
    Rect, Role, SemanticsCtx, Size, View, Widget,
};
use frust::{Color, Theme};

use crate::axis::{AxisSide, YAxis, ticks};
use crate::geometry::{grid_rule_ys, snap, snap_rule_y};
use crate::label::Label;
use crate::{Categories, CategoryRow, Tone, YDomain};

const DEFAULT_HEIGHT: f64 = 160.0;
const UNBOUNDED_WIDTH: f64 = 320.0;
const DEFAULT_GROUP_GAP: f32 = 8.0;
const DEFAULT_BAR_GAP: f32 = 2.0;
const DEFAULT_LABEL_SIZE: f32 = 9.5;
/// Label distance from the plot's edges, along x and along y.
const LABEL_INSET: (f64, f64) = (4.0, 2.0);
/// The least space between two drawn category labels.
const MIN_LABEL_GAP: f64 = 4.0;
const FALLBACK_GRID: Color = Color::from_rgb8(0xd0, 0xd4, 0xda);
const FALLBACK_LABEL: Color = Color::from_rgb8(0x5f, 0x65, 0x6e);
const FALLBACK_COLUMNS: [Color; 3] = [
    Tone::FALLBACK_CALM,
    Tone::FALLBACK_BUSY,
    Tone::FALLBACK_CRITICAL,
];

/// How a category's values are arranged.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BarMode {
    /// One bar per value, side by side in the category's slot (the default).
    #[default]
    Grouped,
    /// One bar per category: positive values stack away from the zero line in column order,
    /// negative values stack the other way.
    Stacked,
}

#[derive(Clone, Debug)]
struct Spec {
    categories: Categories,
    mode: BarMode,
    horizontal: bool,
    colors: Vec<Color>,
    tone: Option<Tone>,
    bar_width: Option<f32>,
    group_gap: f32,
    bar_gap: f32,
    radius: f32,
    domain: YDomain,
    y_axis: Option<YAxis>,
    grid: Vec<f32>,
    grid_color: Option<Color>,
    category_labels: bool,
    label_size: f32,
    label_color: Option<Color>,
    height: Option<f32>,
    semantics: Option<String>,
}

impl Spec {
    /// What a widget holding `old` must redo to show `self`.
    fn changes(&self, old: &Self) -> ChangeFlags {
        if !self.categories.ptr_eq(&old.categories)
            || self.horizontal != old.horizontal
            || self.category_labels != old.category_labels
            || self.label_size != old.label_size
            || self.label_color != old.label_color
            || self.y_axis != old.y_axis
            || self.height != old.height
        {
            return ChangeFlags::LAYOUT | ChangeFlags::PAINT;
        }
        if self.mode != old.mode
            || self.colors != old.colors
            || self.tone != old.tone
            || self.bar_width != old.bar_width
            || self.group_gap != old.group_gap
            || self.bar_gap != old.bar_gap
            || self.radius != old.radius
            || self.domain != old.domain
            || self.grid != old.grid
            || self.grid_color != old.grid_color
            || self.semantics != old.semantics
        {
            return ChangeFlags::PAINT;
        }
        ChangeFlags::NONE
    }

    /// The value axis' side; a horizontal chart's value axis always runs along the bottom.
    fn axis_side(&self) -> AxisSide {
        self.y_axis
            .as_ref()
            .map_or(AxisSide::Left, |axis| axis.side)
    }
}

/// A bar chart over a [`Categories`] list. Build one with [`bar_chart`].
#[derive(Clone, Debug)]
pub struct BarChartView {
    spec: Spec,
}

/// A bar chart of `categories`: one category per row, one grouped vertical bar per value, on a
/// domain resolved from the data that always contains 0, with the row labels beneath.
///
/// ```
/// use frust::Color;
/// use frust_charts::{BarMode, Categories, YAxis, bar_chart};
///
/// let sales = Categories::from_rows([("Q1", [12.0, 8.0]), ("Q2", [15.0, 9.5])]);
/// let _view = bar_chart(sales.clone())
///     .mode(BarMode::Stacked)
///     .colors([Color::from_rgb8(0x2e, 0x9e, 0x5b), Color::from_rgb8(0xe0, 0xa3, 0x1a)])
///     .radius(3.0)
///     .y_axis(YAxis::new())
///     .semantics("Sales by quarter");
/// sales.set_value(1, 0, 16.0); // repaints, no rebuild
/// ```
pub fn bar_chart(categories: Categories) -> BarChartView {
    BarChartView {
        spec: Spec {
            categories,
            mode: BarMode::Grouped,
            horizontal: false,
            colors: Vec::new(),
            tone: None,
            bar_width: None,
            group_gap: DEFAULT_GROUP_GAP,
            bar_gap: DEFAULT_BAR_GAP,
            radius: 0.0,
            domain: YDomain::auto(),
            y_axis: None,
            grid: Vec::new(),
            grid_color: None,
            category_labels: true,
            label_size: DEFAULT_LABEL_SIZE,
            label_color: None,
            height: None,
            semantics: None,
        },
    }
}

impl BarChartView {
    /// Grouped or stacked bars (default [`BarMode::Grouped`]).
    pub fn mode(mut self, mode: BarMode) -> Self {
        self.spec.mode = mode;
        self
    }

    /// Lay the bars down: categories run top to bottom, values grow left to right from the
    /// zero line, and the value axis runs along the bottom.
    pub fn horizontal(mut self) -> Self {
        self.spec.horizontal = true;
        self
    }

    /// One colour per value column, reused in order when there are more columns than colours
    /// (default: the theme's `primary`, `secondary` and `tertiary`).
    pub fn colors(mut self, colors: impl Into<Vec<Color>>) -> Self {
        self.spec.colors = colors.into();
        self
    }

    /// Colour each bar by its own value through `tone`; wins over [`colors`](Self::colors).
    pub fn tone(mut self, tone: Tone) -> Self {
        self.spec.tone = Some(tone);
        self
    }

    /// The bar thickness in logical pixels, rounded down to a whole pixel (default: as thick
    /// as the slot allows). Bars that do not fit shrink to fit, down to 1 px; past that the
    /// groups overlap and the chart clips at its bounds.
    pub fn bar_width(mut self, width: f32) -> Self {
        self.spec.bar_width = Some(width);
        self
    }

    /// The least space between neighbouring categories in logical pixels (default 8).
    pub fn group_gap(mut self, gap: f32) -> Self {
        self.spec.group_gap = gap;
        self
    }

    /// The space between the bars of a grouped category, rounded to whole pixels (default 2);
    /// dropped when the bars would not fit with it.
    pub fn bar_gap(mut self, gap: f32) -> Self {
        self.spec.bar_gap = gap;
        self
    }

    /// Round each bar's outer end, away from the zero line, by `radius` (default 0: square). A
    /// stacked bar rounds only the outer end of each stack.
    pub fn radius(mut self, radius: f32) -> Self {
        self.spec.radius = radius;
        self
    }

    /// The value domain (default [`YDomain::auto`]). It is resolved at every paint over the
    /// data widened to contain 0, or over the stack totals in stacked mode; an automatic range
    /// whose data lies on one side of 0 starts at 0.
    pub fn domain(mut self, domain: YDomain) -> Self {
        self.spec.domain = domain;
        self
    }

    /// A value axis (default none): a rule and a label at each of the [`ticks`] of the
    /// resolved domain, the labels in a gutter as wide as the widest of them. Its rules replace
    /// the [`grid`](Self::grid) fractions and use the [`grid_color`](Self::grid_color). The
    /// axis' side, tick count, format, colour and size apply; its gutter setting does not
    /// (there always is one), and a horizontal chart puts the axis along the bottom.
    pub fn y_axis(mut self, axis: YAxis) -> Self {
        self.spec.y_axis = Some(axis);
        self
    }

    /// 1 px rules across the bars at these fractions of the domain, 0.0 its minimum and 1.0
    /// its maximum (default none). Ignored when there is a [`y_axis`](Self::y_axis).
    pub fn grid(mut self, fractions: impl Into<Vec<f32>>) -> Self {
        self.spec.grid = fractions.into();
        self
    }

    /// The rule colour (default: the theme's `outline_variant`).
    pub fn grid_color(mut self, color: Color) -> Self {
        self.spec.grid_color = Some(color);
        self
    }

    /// Draw each row's label beside its category (default `true`): beneath the bars, or to
    /// their left when horizontal. When the labels would overlap, every other one is skipped
    /// (then every third, and so on); a horizontal chart's label column is at most half the
    /// width, and a label too wide for it is skipped.
    pub fn category_labels(mut self, show: bool) -> Self {
        self.spec.category_labels = show;
        self
    }

    /// The labels' font size and colour (default 9.5 and the theme's `on_surface_variant`);
    /// a [`YAxis`] colour or size wins for the axis labels.
    pub fn label_style(mut self, size: f32, color: Color) -> Self {
        self.spec.label_size = size;
        self.spec.label_color = Some(color);
        self
    }

    /// A fixed height in logical pixels (default: the available height when bounded, else
    /// 160). The width always expands to the available width.
    pub fn height(mut self, height: f32) -> Self {
        self.spec.height = Some(height);
        self
    }

    /// Give the chart an image node with this label; without one it has no node.
    pub fn semantics(mut self, label: impl Into<String>) -> Self {
        self.spec.semantics = Some(label.into());
        self
    }
}

impl<State: 'static> View<State> for BarChartView {
    type Element = BarChartWidget;

    fn build(&self, _ctx: &mut BuildCtx<'_>) -> BarChartWidget {
        BarChartWidget {
            spec: self.spec.clone(),
            cache: BarCache::default(),
            axis: AxisLabels::default(),
            names: CategoryLabels::default(),
            requested: None,
        }
    }

    fn rebuild(
        &self,
        _prev: &Self,
        element: &mut BarChartWidget,
        _ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        let flags = self.spec.changes(&element.spec);
        if !flags.is_empty() {
            element.spec = self.spec.clone();
            element.cache = BarCache::default();
        }
        flags
    }
}

/// Which end of a bar is rounded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Edge {
    Top,
    Bottom,
    Left,
    Right,
}

impl Edge {
    fn radii(self, r: f64) -> CornerRadii {
        match self {
            Self::Top => CornerRadii::new(r, r, 0.0, 0.0),
            Self::Bottom => CornerRadii::new(0.0, 0.0, r, r),
            Self::Left => CornerRadii::new(r, 0.0, 0.0, r),
            Self::Right => CornerRadii::new(0.0, r, r, 0.0),
        }
    }
}

/// One built bar, widget-local.
#[derive(Clone, Copy, Debug)]
struct Bar {
    rect: Rect,
    column: usize,
    value: f32,
    outer: Option<Edge>,
}

/// The inputs the cached bars were built from.
#[derive(Clone, Copy, Debug, PartialEq)]
struct BarKey {
    revision: u64,
    plot: Rect,
    domain: (f32, f32),
}

#[derive(Default)]
struct BarCache {
    /// The list revision the domain was resolved at, and the domain.
    domain: Option<(u64, (f32, f32))>,
    key: Option<BarKey>,
    bars: Vec<Bar>,
}

/// The value-axis labels as last laid out.
#[derive(Default)]
struct AxisLabels {
    /// The ticks the labels were shaped for, one per label.
    ticks: Vec<f32>,
    /// The shaped labels; a paint places each along the axis at its tick.
    labels: Vec<Label>,
    /// The gutter width (vertical) or band height (horizontal) reserved for them.
    extent: f64,
}

/// The category labels as last laid out.
#[derive(Default)]
struct CategoryLabels {
    /// The list's label revision they were shaped at; `None` when labels are off.
    revision: Option<u64>,
    labels: Vec<Label>,
    shown: Vec<bool>,
    /// The band height (vertical) or column width (horizontal) reserved for them.
    extent: f64,
}

/// The ticks and label revision a paint asked a layout for.
#[derive(Clone, Debug, PartialEq)]
struct LayoutKey {
    ticks: Vec<f32>,
    label_revision: Option<u64>,
}

/// Where values land along the plot's value axis.
#[derive(Clone, Copy, Debug)]
struct ValueAxis {
    /// The coordinates of the domain's minimum and maximum.
    at_min: f64,
    at_max: f64,
    domain: (f32, f32),
}

impl ValueAxis {
    fn new(plot: Rect, horizontal: bool, domain: (f32, f32)) -> Self {
        let (at_min, at_max) = if horizontal {
            (plot.x0, plot.x1)
        } else {
            (plot.y1, plot.y0)
        };
        Self {
            at_min,
            at_max,
            domain,
        }
    }

    /// The coordinate of `value`, clamped to the plot.
    fn pos(&self, value: f64) -> f64 {
        let (min, max) = (f64::from(self.domain.0), f64::from(self.domain.1));
        let t = ((value - min) / (max - min)).clamp(0.0, 1.0);
        let t = if t.is_nan() { 0.0 } else { t };
        self.at_min + t * (self.at_max - self.at_min)
    }

    /// The snapped edge a run from value `from` to `to` ends at, given the snapped edge
    /// `start` it begins at: at least one pixel on from `start` (inside the plot) when the
    /// run has any length, and `start` itself when it has none.
    fn advance(&self, start: f64, from: f64, to: f64) -> f64 {
        let (a, b) = (self.pos(from), self.pos(to));
        if a == b {
            return start;
        }
        let step = (b - a).signum();
        let end = snap(b);
        if (end - start) * step >= 1.0 {
            return end;
        }
        let (lo, hi) = (
            snap(self.at_min.min(self.at_max)),
            snap(self.at_min.max(self.at_max)),
        );
        (start + step).clamp(lo, hi)
    }
}

/// Whole-pixel bar positions along the category axis.
#[derive(Clone, Copy, Debug)]
struct Slots {
    start: f64,
    slot: f64,
    bar: f64,
    gap: f64,
    group: f64,
}

/// `value` as a whole, non-negative pixel count; non-finite is 0.
fn whole_px(value: f32) -> f64 {
    if value.is_finite() {
        f64::from(value).max(0.0).round()
    } else {
        0.0
    }
}

impl Slots {
    /// `count` slots across `extent` from `start`, each holding `per_group` bars.
    fn new(start: f64, extent: f64, count: usize, per_group: usize, spec: &Spec) -> Self {
        let slot = extent / count as f64;
        let m = per_group.max(1) as f64;
        let group_gap = whole_px(spec.group_gap);
        let mut gap = whole_px(spec.bar_gap);
        let mut room = (slot - group_gap - (m - 1.0) * gap) / m;
        if room < 1.0 {
            gap = 0.0;
            room = (slot - group_gap) / m;
        }
        if room < 1.0 {
            room = slot / m;
        }
        let requested = spec
            .bar_width
            .filter(|w| w.is_finite() && *w > 0.0)
            .map_or(f64::INFINITY, f64::from);
        let bar = requested.min(room).floor().max(1.0);
        Self {
            start,
            slot,
            bar,
            gap,
            group: m * bar + (m - 1.0) * gap,
        }
    }

    /// The leading edge of bar `j` of category `i`.
    fn bar_start(&self, i: usize, j: usize) -> f64 {
        let group = snap(self.start + i as f64 * self.slot + (self.slot - self.group) / 2.0);
        group + j as f64 * (self.bar + self.gap)
    }
}

/// The smallest and largest value the domain must hold: every value when grouped, each row's
/// positive and negative totals when stacked; `None` without data.
fn data_range(rows: &[CategoryRow], mode: BarMode) -> Option<(f32, f32)> {
    let mut range: Option<(f32, f32)> = None;
    let mut include = |v: f32| {
        range = Some(range.map_or((v, v), |(lo, hi)| (lo.min(v), hi.max(v))));
    };
    for row in rows {
        let mut present = row.values.iter().flatten().copied().peekable();
        match mode {
            BarMode::Grouped => present.for_each(&mut include),
            BarMode::Stacked => {
                if present.peek().is_none() {
                    continue;
                }
                let (up, down) = present.fold((0.0f32, 0.0f32), |(up, down), v| {
                    if v >= 0.0 {
                        (up + v, down)
                    } else {
                        (up, down + v)
                    }
                });
                include(up);
                include(down);
            }
        }
    }
    range
}

/// `domain` resolved over `data` widened to contain 0. An automatic range over data on one
/// side of 0 is moved, span kept, to start at 0 when widening about its middle crossed it.
fn resolve_domain(domain: YDomain, data: Option<(f32, f32)>) -> (f32, f32) {
    let widened = data.map(|(lo, hi)| (lo.min(0.0), hi.max(0.0)));
    let (lo, hi) = domain.resolve_range(widened);
    let YDomain::AutoRange { .. } = domain else {
        return (lo, hi);
    };
    match widened {
        Some((data_lo, _)) if data_lo >= 0.0 && lo < 0.0 => (0.0, hi - lo),
        Some((_, data_hi)) if data_hi <= 0.0 && hi > 0.0 => (lo - hi, 0.0),
        _ => (lo, hi),
    }
}

/// Which labels to draw, given each one's `(start, length)` along the category axis: every
/// `k`-th from the first, for the smallest `k` at which no two drawn labels come closer than
/// [`MIN_LABEL_GAP`].
fn every_kth(spans: &[(f64, f64)]) -> Vec<bool> {
    let n = spans.len();
    let clear = |k: usize| {
        (0..n)
            .step_by(k)
            .zip((k..n).step_by(k))
            .all(|(a, b)| spans[a].0 + spans[a].1 + MIN_LABEL_GAP <= spans[b].0)
    };
    let k = (1..=n.max(1)).find(|&k| clear(k)).unwrap_or(1);
    (0..n).map(|i| i % k == 0).collect()
}

/// The bars of `rows` in `plot` against `domain`.
fn build_bars(rows: &[CategoryRow], spec: &Spec, plot: Rect, domain: (f32, f32)) -> Vec<Bar> {
    let usable = plot.width() > 0.0 && plot.height() > 0.0;
    if rows.is_empty() || !usable {
        return Vec::new();
    }
    let horizontal = spec.horizontal;
    let per_group = match spec.mode {
        BarMode::Grouped => rows.iter().map(|r| r.values.len()).max().unwrap_or(1),
        BarMode::Stacked => 1,
    };
    let slots = if horizontal {
        Slots::new(plot.y0, plot.height(), rows.len(), per_group, spec)
    } else {
        Slots::new(plot.x0, plot.width(), rows.len(), per_group, spec)
    };
    let axis = ValueAxis::new(plot, horizontal, domain);
    let base = snap(axis.pos(0.0));
    // The end a run from `a` to `b` along the value axis points to.
    let outer = |a: f64, b: f64| match (horizontal, b > a) {
        (false, false) => Edge::Top,
        (false, true) => Edge::Bottom,
        (true, true) => Edge::Right,
        (true, false) => Edge::Left,
    };
    let bar = |lead: f64, (a, b): (f64, f64), column: usize, value: f32| {
        let (lo, hi) = (a.min(b), a.max(b));
        let rect = if horizontal {
            Rect::new(lo, lead, hi, lead + slots.bar)
        } else {
            Rect::new(lead, lo, lead + slots.bar, hi)
        };
        Bar {
            rect,
            column,
            value,
            outer: Some(outer(a, b)),
        }
    };
    let mut bars = Vec::new();
    for (i, row) in rows.iter().enumerate() {
        let present = row.values.iter().enumerate();
        let present = present.filter_map(|(j, v)| v.map(|v| (j, v)));
        if spec.mode == BarMode::Grouped {
            for (j, value) in present {
                let end = axis.advance(base, 0.0, f64::from(value));
                if end != base {
                    bars.push(bar(slots.bar_start(i, j), (base, end), j, value));
                }
            }
            continue;
        }
        let lead = slots.bar_start(i, 0);
        // Per direction: the running total, the edge reached and the last bar drawn.
        let mut stacks = [(0.0f64, base, None::<usize>); 2];
        for (j, value) in present {
            let stack = &mut stacks[usize::from(value < 0.0)];
            let from = stack.0;
            stack.0 += f64::from(value);
            let end = axis.advance(stack.1, from, stack.0);
            if end != stack.1 {
                let mut segment = bar(lead, (stack.1, end), j, value);
                segment.outer = None;
                stack.2 = Some(bars.len());
                bars.push(segment);
            }
            stack.1 = end;
        }
        for (_, end, last) in stacks {
            if let Some(index) = last {
                bars[index].outer = Some(outer(base, end));
            }
        }
    }
    bars
}

/// The colours a paint falls back to.
struct Defaults {
    columns: [Color; 3],
    grid: Color,
}

impl Defaults {
    fn resolve(theme: Option<&Theme>) -> Self {
        match theme {
            Some(theme) => {
                let scheme = theme.scheme();
                Self {
                    columns: [scheme.primary, scheme.secondary, scheme.tertiary],
                    grid: scheme.outline_variant,
                }
            }
            None => Self {
                columns: FALLBACK_COLUMNS,
                grid: FALLBACK_GRID,
            },
        }
    }
}

/// The retained widget behind a [`BarChartView`].
pub struct BarChartWidget {
    spec: Spec,
    cache: BarCache,
    axis: AxisLabels,
    names: CategoryLabels,
    /// The change a paint asked a layout for, until a layout runs, so a paint asks at most once
    /// per change.
    requested: Option<LayoutKey>,
}

impl BarChartWidget {
    /// The domain resolved from the list's current data, reused while its revision holds.
    fn domain(&mut self) -> (f32, f32) {
        let spec = &self.spec;
        let revision = spec.categories.revision();
        if let Some((at, domain)) = self.cache.domain
            && at == revision
        {
            return domain;
        }
        let data = spec
            .categories
            .with_rows(|rows| data_range(rows, spec.mode));
        let domain = resolve_domain(spec.domain, data);
        self.cache.domain = Some((revision, domain));
        domain
    }

    fn ticks(&self, domain: (f32, f32)) -> Vec<f32> {
        self.spec
            .y_axis
            .as_ref()
            .map_or_else(Vec::new, |axis| ticks(domain.0, domain.1, axis.tick_count))
    }

    /// The plot: the widget's bounds less the axis gutter or band and the category labels.
    fn plot(&self, size: Size) -> Rect {
        let (width, height) = (size.width.max(0.0), size.height.max(0.0));
        let (axis, names) = (self.axis.extent, self.names.extent);
        if self.spec.horizontal {
            return Rect::new(names.min(width), 0.0, width, (height - axis).max(0.0));
        }
        let bottom = (height - names).max(0.0);
        match self.spec.axis_side() {
            AxisSide::Right => Rect::new(0.0, 0.0, (width - axis).max(0.0), bottom),
            AxisSide::Left => Rect::new(axis.min(width), 0.0, width, bottom),
        }
    }

    /// Shape a label per tick of the current domain, reusing a label whose text is unchanged,
    /// and measure the space they need.
    fn shape_axis(&mut self, ctx: &mut LayoutCtx, size: Size, style: Option<&TextStyle>) {
        let Some((axis, style)) = self.spec.y_axis.clone().zip(style) else {
            self.axis = AxisLabels::default();
            return;
        };
        let domain = self.domain();
        let ticks = self.ticks(domain);
        let mut old = std::mem::take(&mut self.axis.labels);
        let mut labels = Vec::with_capacity(ticks.len());
        let mut need: f64 = 0.0;
        for tick in &ticks {
            let text = axis.format.format(*tick);
            let mut label = match old.iter().position(|label| label.text == text) {
                Some(i) => old.swap_remove(i),
                None => Label::new(text),
            };
            let shaped = label.shape(ctx, style);
            need = need.max(if self.spec.horizontal {
                shaped.height
            } else {
                shaped.width
            });
            labels.push(label);
        }
        let (inset, room) = if self.spec.horizontal {
            (LABEL_INSET.1, size.height)
        } else {
            (LABEL_INSET.0, size.width)
        };
        let extent = if labels.is_empty() {
            0.0
        } else {
            (need + 2.0 * inset).ceil().min((room / 2.0).max(0.0))
        };
        self.axis = AxisLabels {
            ticks,
            labels,
            extent,
        };
    }

    /// Shape each row's label, reusing a label whose text is unchanged, and measure the space
    /// they need.
    fn shape_names(&mut self, ctx: &mut LayoutCtx, size: Size, style: &TextStyle) {
        let categories = self.spec.categories.clone();
        let revision = categories.label_revision();
        let texts: Vec<String> =
            categories.with_rows(|rows| rows.iter().map(|row| row.label.clone()).collect());
        let mut old = std::mem::take(&mut self.names.labels);
        let mut labels = Vec::with_capacity(texts.len());
        let mut need: f64 = 0.0;
        for text in texts {
            let mut label = match old.iter().position(|label| label.text == text) {
                Some(i) => old.swap_remove(i),
                None => Label::new(text),
            };
            let shaped = label.shape(ctx, style);
            if !label.text.is_empty() {
                need = need.max(if self.spec.horizontal {
                    shaped.width
                } else {
                    shaped.height
                });
            }
            labels.push(label);
        }
        let (inset, room) = if self.spec.horizontal {
            (LABEL_INSET.0, size.width)
        } else {
            (LABEL_INSET.1, size.height)
        };
        let extent = if need > 0.0 {
            (need + 2.0 * inset).ceil().min((room / 2.0).max(0.0))
        } else {
            0.0
        };
        self.names = CategoryLabels {
            revision: Some(revision),
            shown: vec![false; labels.len()],
            labels,
            extent,
        };
    }

    /// Place each category label beside its slot and pick the ones to draw.
    fn place_names(&mut self, size: Size, plot: Rect) {
        let n = self.names.labels.len();
        if n == 0 {
            return;
        }
        let (inset_x, inset_y) = LABEL_INSET;
        let horizontal = self.spec.horizontal;
        let slot = if horizontal {
            plot.height()
        } else {
            plot.width()
        } / n as f64;
        let mut spans = Vec::with_capacity(n);
        let mut fits = Vec::with_capacity(n);
        for (i, label) in self.names.labels.iter_mut().enumerate() {
            let text = label.size();
            let centre = (i as f64 + 0.5) * slot;
            if horizontal {
                let y = (plot.y0 + centre - text.height / 2.0)
                    .clamp(0.0, (plot.y1 - text.height).max(0.0));
                label.at = Point::new(snap(plot.x0 - inset_x - text.width), snap(y));
                spans.push((y, text.height));
                fits.push(text.width + 2.0 * inset_x <= plot.x0 + 1e-6);
            } else {
                let x = (plot.x0 + centre - text.width / 2.0)
                    .clamp(0.0, (size.width - text.width).max(0.0));
                label.at = Point::new(snap(x), snap(plot.y1 + inset_y));
                spans.push((x, text.width));
                fits.push(true);
            }
        }
        self.names.shown = every_kth(&spans)
            .into_iter()
            .zip(fits)
            .map(|(kth, fits)| kth && fits)
            .collect();
    }

    /// Fix each axis label's position across the axis; a paint sets the one along it.
    fn place_axis(&mut self, size: Size) {
        let (inset_x, inset_y) = LABEL_INSET;
        let plot = self.plot(size);
        let extent = self.axis.extent;
        let side = self.spec.axis_side();
        for label in &mut self.axis.labels {
            let at = if self.spec.horizontal {
                Point::new(0.0, plot.y1 + inset_y)
            } else {
                let width = label.size().width;
                let x = match side {
                    AxisSide::Left => extent - inset_x - width,
                    AxisSide::Right => size.width - extent + inset_x,
                };
                Point::new(x, 0.0)
            };
            label.at = Point::new(snap(at.x), snap(at.y));
        }
    }

    /// The rules: one per axis tick when there is an axis, else one per grid fraction.
    fn paint_rules(
        &self,
        origin: Point,
        (plot, axis): (Rect, ValueAxis),
        ticks: &[f32],
        color: Color,
        scene: &mut dyn PaintScene,
    ) {
        if !(plot.width() > 0.0 && plot.height() > 0.0) {
            return;
        }
        let horizontal = self.spec.horizontal;
        let mut at: Vec<f64> = if self.spec.y_axis.is_some() {
            let (lo, hi) = if horizontal {
                (snap(plot.x0), (snap(plot.x1) - 1.0).max(snap(plot.x0)))
            } else {
                (snap(plot.y0), (snap(plot.y1) - 1.0).max(snap(plot.y0)))
            };
            ticks
                .iter()
                .map(|&tick| snap_rule_y(axis.pos(f64::from(tick))).clamp(lo, hi))
                .collect()
        } else if horizontal {
            let flipped = Rect::new(plot.y0, plot.x0, plot.y1, plot.x1);
            let fractions: Vec<f32> = self.spec.grid.iter().map(|f| 1.0 - f).collect();
            grid_rule_ys(&fractions, flipped)
        } else {
            grid_rule_ys(&self.spec.grid, plot)
        };
        at.sort_by(f64::total_cmp);
        at.dedup();
        for c in at {
            let (point, extent) = if horizontal {
                (Point::new(c, plot.y0), Size::new(1.0, plot.height()))
            } else {
                (Point::new(plot.x0, c), Size::new(plot.width(), 1.0))
            };
            scene.fill_rect(origin + point.to_vec2(), extent, color);
        }
    }

    /// Draw each axis label centred on its tick under `axis`, kept inside the widget; a label
    /// whose value left the domain is skipped.
    fn paint_axis_labels(
        &self,
        origin: Point,
        size: Size,
        axis: ValueAxis,
        scene: &mut dyn PaintScene,
    ) {
        let (inset_x, inset_y) = LABEL_INSET;
        let domain = axis.domain;
        for (tick, label) in self.axis.ticks.iter().zip(&self.axis.labels) {
            if !(domain.0..=domain.1).contains(tick) {
                continue;
            }
            let text = label.size();
            let centre = axis.pos(f64::from(*tick));
            let at = if self.spec.horizontal {
                let x = (centre - text.width / 2.0)
                    .min(size.width - inset_x - text.width)
                    .max(inset_x);
                Point::new(snap(x), label.at.y)
            } else {
                let y = (centre - text.height / 2.0)
                    .min(size.height - inset_y - text.height)
                    .max(inset_y);
                Point::new(label.at.x, snap(y))
            };
            label.draw(origin + at.to_vec2(), scene);
        }
    }

    fn paint_bars(
        &mut self,
        origin: Point,
        key: BarKey,
        defaults: &Defaults,
        scene: &mut dyn PaintScene,
    ) {
        if self.cache.key != Some(key) {
            let spec = &self.spec;
            self.cache.bars = spec
                .categories
                .with_rows(|rows| build_bars(rows, spec, key.plot, key.domain));
            self.cache.key = Some(key);
        }
        let spec = &self.spec;
        let radius = f64::from(spec.radius);
        for bar in &self.cache.bars {
            let color = match (&spec.tone, spec.colors.len()) {
                (Some(tone), _) => tone.color_at(bar.value),
                (None, 0) => defaults.columns[bar.column % defaults.columns.len()],
                (None, len) => spec.colors[bar.column % len],
            };
            let (at, size) = (origin + bar.rect.origin().to_vec2(), bar.rect.size());
            // At most half the thickness, and no more than the bar's length.
            let (thickness, length) = if spec.horizontal {
                (size.height, size.width)
            } else {
                (size.width, size.height)
            };
            let r = radius.min(thickness / 2.0).min(length);
            match bar.outer {
                Some(edge) if r > 0.0 => {
                    scene.fill_rounded_rect_radii(at, size, edge.radii(r), color);
                }
                _ => scene.fill_rect(at, size, color),
            }
        }
    }
}

impl Widget for BarChartWidget {
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
        self.requested = None;
        if !self.spec.category_labels && self.spec.y_axis.is_none() {
            self.axis = AxisLabels::default();
            self.names = CategoryLabels::default();
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
        self.shape_axis(ctx, size, axis_style.as_ref());
        if self.spec.category_labels {
            let style = TextStyle::new(label_size, color).with_family(family);
            self.shape_names(ctx, size, &style);
        } else {
            self.names = CategoryLabels::default();
        }
        let plot = self.plot(size);
        self.place_names(size, plot);
        self.place_axis(size);
        size
    }

    fn paint(&mut self, ctx: &mut PaintCtx, scene: &mut dyn PaintScene) {
        let (origin, size) = (ctx.origin(), ctx.size());
        if !(size.width > 0.0 && size.height > 0.0) {
            return;
        }
        let domain = self.domain();
        let ticks = self.ticks(domain);
        let label_revision = self
            .spec
            .category_labels
            .then(|| self.spec.categories.label_revision());
        let shaped = self.axis.ticks == ticks && self.names.revision == label_revision;
        let asked = self
            .requested
            .as_ref()
            .is_some_and(|k| k.ticks == ticks && k.label_revision == label_revision);
        if !shaped && !asked {
            // The labels were shaped for other ticks or other names: ask once for a layout.
            ctx.request_layout();
            self.requested = Some(LayoutKey {
                ticks: ticks.clone(),
                label_revision,
            });
        }
        let defaults = Defaults::resolve(Theme::from_paint_ctx(ctx));
        let plot = self.plot(size);
        let axis = ValueAxis::new(plot, self.spec.horizontal, domain);
        scene.push_clip(origin, size);
        let grid = self.spec.grid_color.unwrap_or(defaults.grid);
        self.paint_rules(origin, (plot, axis), &ticks, grid, scene);
        let key = BarKey {
            revision: self.spec.categories.revision(),
            plot,
            domain,
        };
        self.paint_bars(origin, key, &defaults, scene);
        self.paint_axis_labels(origin, size, axis, scene);
        for (label, shown) in self.names.labels.iter().zip(&self.names.shown) {
            if *shown {
                label.paint(origin, scene);
            }
        }
        scene.pop_clip();
    }

    fn semantics(&self, ctx: &mut SemanticsCtx) {
        if let Some(label) = &self.spec.semantics {
            ctx.push_node(Role::Image, |node| node.set_label(label.as_str()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{every_kth, resolve_domain};
    use crate::YDomain;

    #[test]
    fn labels_are_kept_at_the_smallest_clear_stride() {
        let spaced = [(0.0, 10.0), (20.0, 10.0), (40.0, 10.0)];
        assert_eq!(every_kth(&spaced), [true, true, true]);
        let crowded = [
            (0.0, 10.0),
            (8.0, 10.0),
            (16.0, 10.0),
            (24.0, 10.0),
            (32.0, 10.0),
        ];
        assert_eq!(every_kth(&crowded), [true, false, true, false, true]);
        assert_eq!(every_kth(&[(0.0, 100.0), (1.0, 100.0)]), [true, false]);
        assert!(every_kth(&[]).is_empty());
    }

    #[test]
    fn an_automatic_domain_starts_at_zero() {
        let auto = YDomain::auto();
        assert_eq!(resolve_domain(auto, Some((3.2, 87.0))), (0.0, 100.0));
        assert_eq!(resolve_domain(auto, Some((-40.0, -3.0))), (-40.0, 0.0));
        assert_eq!(resolve_domain(auto, Some((-40.0, 12.0))), (-40.0, 20.0));
        assert_eq!(resolve_domain(auto, Some((0.0, 0.0))), (0.0, 1.0));
        assert_eq!(resolve_domain(auto, None), (0.0, 1.0));
        let fixed = YDomain::fixed(10.0, 20.0);
        assert_eq!(resolve_domain(fixed, Some((0.0, 50.0))), (10.0, 20.0));
    }
}
