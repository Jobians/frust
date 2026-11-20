//! The `radar_chart` widget: a polygon or circular web with one axis per category and one or
//! more filled datasets over it.
//!
//! Each row of the [`Categories`] is an axis, the first at 12 o'clock and the rest clockwise at
//! even angles; each value column is a dataset, a closed polygon through its value on every
//! axis. All axes share one radial domain: its minimum sits at the centre and its maximum on
//! the outer ring. A value outside the domain is clamped to the web, and a gap sits at the
//! centre. With fewer than three axes there is no polygon: only the web is drawn.
//!
//! The widget reads the categories while painting, so a value write shows with no rebuild.
//! Built paths are cached and rebuilt only when the categories' revision or the web's centre
//! or radius changed (a spec change drops them). Axis titles are the rows' labels, shaped in
//! layout and placed outside the web, which shrinks to keep them inside the widget. A paint
//! that finds the labels changed (`Categories::label_revision` moved) asks for one layout and
//! draws this frame with the titles it has; the next layout shapes and places the new ones.
//!
//! Draw order: the web's rings (inner to outer), its spokes, each dataset in column order (its
//! fill, then its stroke), then the titles, all clipped to the widget's bounds. Colours: the
//! caller's, then the installed theme's (datasets `primary`, web `outline_variant`, titles
//! `on_surface_variant` in the `body_small` style), then fallback constants. The chart never
//! animates, takes no input and asks for no frame beyond that layout.

use std::f64::consts::{FRAC_PI_2, TAU};

use frust::authoring::text::TextStyle;
use frust::authoring::{
    BezPath, BoxConstraints, Brush, BuildCtx, ChangeFlags, LayoutCtx, PaintCtx, PaintScene, Point,
    Role, SemanticsCtx, Size, Vec2, View, Widget,
};
use frust::{Color, Theme};

use crate::geometry::snap;
use crate::label::Label;
use crate::{Categories, CategoryRow, Tone, YDomain, arc_path, point_on_circle};

const DEFAULT_RINGS: usize = 4;
const DEFAULT_SIZE: f64 = 160.0;
const DEFAULT_STROKE_WIDTH: f32 = 1.5;
const DEFAULT_FILL_ALPHA: f32 = 0.25;
const DEFAULT_WEB_WIDTH: f32 = 1.0;
const DEFAULT_TITLE_SIZE: f32 = 11.0;
/// The space between the outer ring and the widget's edge when no title is nearer the edge.
const EDGE_INSET: f64 = 2.0;
/// The space between the outer ring and an axis title's anchor.
const TITLE_GAP: f64 = 4.0;
/// How far from zero an axis direction's component must be to count as left, right, up or down.
const ALIGN_EPS: f64 = 1e-9;
/// The first axis' angle: 12 o'clock.
const FIRST_AXIS: f64 = -FRAC_PI_2;
const FALLBACK_WEB: Color = Color::from_rgb8(0xd0, 0xd4, 0xda);
const FALLBACK_TITLE: Color = Color::from_rgb8(0x5f, 0x65, 0x6e);

/// The outline of a radar chart's web rings.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RadarShape {
    /// Straight edges between neighbouring axes (the default). With fewer than three axes
    /// there is no polygon, so the rings are drawn as circles.
    #[default]
    Polygon,
    /// Circles.
    Circle,
}

/// How one dataset of a [`radar_chart`] is drawn: a stroke around its polygon and a
/// translucent fill in the same colour.
///
/// The default takes the theme's primary colour, a 1.5 px stroke and a fill at alpha 0.25.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RadarStyle {
    color: Option<Color>,
    stroke_width: f32,
    fill_alpha: f32,
}

impl Default for RadarStyle {
    fn default() -> Self {
        Self {
            color: None,
            stroke_width: DEFAULT_STROKE_WIDTH,
            fill_alpha: DEFAULT_FILL_ALPHA,
        }
    }
}

impl RadarStyle {
    /// The default style: the theme's primary colour.
    pub fn new() -> Self {
        Self::default()
    }

    /// Stroke in `color` and fill in it at the fill alpha.
    pub fn color(color: Color) -> Self {
        Self {
            color: Some(color),
            ..Self::default()
        }
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

    /// The fill's alpha, clamped to `0..=1` (default 0.25); 0 draws no fill, and a
    /// non-finite alpha is 0.
    pub fn fill_alpha(mut self, alpha: f32) -> Self {
        self.fill_alpha = if alpha.is_finite() {
            alpha.clamp(0.0, 1.0)
        } else {
            0.0
        };
        self
    }
}

#[derive(Clone, Debug)]
struct Spec {
    categories: Categories,
    shape: RadarShape,
    rings: usize,
    domain: YDomain,
    styles: Vec<RadarStyle>,
    web: (Option<Color>, f32),
    titles: bool,
    size: Option<f32>,
    semantics: Option<String>,
}

impl Spec {
    /// What a widget holding `old` must redo to show `self`.
    fn changes(&self, old: &Self) -> ChangeFlags {
        if !self.categories.ptr_eq(&old.categories)
            || self.titles != old.titles
            || self.size != old.size
        {
            return ChangeFlags::LAYOUT | ChangeFlags::PAINT;
        }
        if self.shape != old.shape
            || self.rings != old.rings
            || self.domain != old.domain
            || self.styles != old.styles
            || self.web != old.web
            || self.semantics != old.semantics
        {
            return ChangeFlags::PAINT;
        }
        ChangeFlags::NONE
    }
}

/// A radar chart over a [`Categories`] list. Build one with [`radar_chart`].
#[derive(Clone, Debug)]
pub struct RadarChartView {
    spec: Spec,
}

/// A radar chart with one axis per row of `categories` (titled by the row's label) and one
/// dataset per value column: a polygon web of 4 rings over an automatic domain from zero,
/// titled, as large a square as the box allows.
///
/// ```
/// use frust::Color;
/// use frust_charts::{Categories, RadarShape, RadarStyle, radar_chart};
///
/// let skills = Categories::from_rows([
///     ("Speed", [80.0, 55.0]),
///     ("Power", [60.0, 90.0]),
///     ("Range", [70.0, 40.0]),
///     ("Armour", [45.0, 75.0]),
///     ("Stealth", [90.0, 30.0]),
/// ]);
/// let _view = radar_chart(skills.clone())
///     .shape(RadarShape::Circle)
///     .dataset_styles([
///         RadarStyle::color(Color::from_rgb8(0x2e, 0x9e, 0x5b)),
///         RadarStyle::color(Color::from_rgb8(0x1f, 0x6f, 0xd6)),
///     ])
///     .semantics("Skill comparison");
/// skills.set_value(0, 1, 65.0); // repaints, no rebuild
/// ```
pub fn radar_chart(categories: Categories) -> RadarChartView {
    RadarChartView {
        spec: Spec {
            categories,
            shape: RadarShape::Polygon,
            rings: DEFAULT_RINGS,
            domain: YDomain::auto_from_zero(),
            styles: Vec::new(),
            web: (None, DEFAULT_WEB_WIDTH),
            titles: true,
            size: None,
            semantics: None,
        },
    }
}

impl RadarChartView {
    /// The web's outline (default [`RadarShape::Polygon`]).
    pub fn shape(mut self, shape: RadarShape) -> Self {
        self.spec.shape = shape;
        self
    }

    /// How many rings the web draws, evenly spaced out to the outer one (default 4); 0 draws
    /// the spokes alone.
    pub fn rings(mut self, rings: usize) -> Self {
        self.spec.rings = rings;
        self
    }

    /// The radial domain every axis shares (default [`YDomain::auto_from_zero`]), resolved
    /// from the largest value at every paint: its minimum sits at the centre, its maximum on
    /// the outer ring.
    pub fn domain(mut self, domain: YDomain) -> Self {
        self.spec.domain = domain;
        self
    }

    /// One style per dataset, in column order; a dataset past the end takes
    /// [`RadarStyle::default`].
    pub fn dataset_styles(mut self, styles: impl Into<Vec<RadarStyle>>) -> Self {
        self.spec.styles = styles.into();
        self
    }

    /// The web's colour and stroke width in logical pixels (default: the theme's
    /// `outline_variant` and 1); a width of 0 draws no web. A negative or non-finite width
    /// is 0.
    pub fn web(mut self, color: Color, width: f32) -> Self {
        let width = if width.is_finite() {
            width.max(0.0)
        } else {
            0.0
        };
        self.spec.web = (Some(color), width);
        self
    }

    /// Draw each row's label outside the web at its axis (default `true`). A title right of
    /// the centre is left-aligned to its anchor, one left of it right-aligned, and the top and
    /// bottom ones centred; the web shrinks so every title fits inside the widget.
    pub fn titles(mut self, titles: bool) -> Self {
        self.spec.titles = titles;
        self
    }

    /// A fixed side in logical pixels: the chart is that square (default: the largest square
    /// the box allows, 160 when it is unbounded both ways).
    pub fn size(mut self, side: f32) -> Self {
        self.spec.size = Some(side);
        self
    }

    /// Give the chart an image node with this label; without one it has no node.
    pub fn semantics(mut self, label: impl Into<String>) -> Self {
        self.spec.semantics = Some(label.into());
        self
    }
}

impl<State: 'static> View<State> for RadarChartView {
    type Element = RadarChartWidget;

    fn build(&self, _ctx: &mut BuildCtx<'_>) -> RadarChartWidget {
        RadarChartWidget {
            spec: self.spec.clone(),
            titles: Vec::new(),
            title_revision: None,
            requested: None,
            radius: 0.0,
            built: None,
        }
    }

    fn rebuild(
        &self,
        _prev: &Self,
        element: &mut RadarChartWidget,
        _ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        let flags = self.spec.changes(&element.spec);
        if !flags.is_empty() {
            element.spec = self.spec.clone();
            element.built = None;
        }
        flags
    }
}

/// The inputs the cached paths were built from (the spec is the rest: a change drops them).
#[derive(Clone, Copy, Debug, PartialEq)]
struct BuildKey {
    revision: u64,
    center: Point,
    radius: f64,
}

/// The web and dataset paths, widget-local.
struct Built {
    key: BuildKey,
    axes: usize,
    rings: Vec<BezPath>,
    spokes: BezPath,
    datasets: Vec<BezPath>,
}

/// The angle of axis `index` of `count`, clockwise from 12 o'clock.
fn axis_angle(index: usize, count: usize) -> f64 {
    FIRST_AXIS + TAU * index as f64 / count.max(1) as f64
}

/// Where `value` sits between the centre (0) and the outer ring (1) of `domain`; a gap is 0.
fn reach(value: Option<f32>, (min, max): (f32, f32)) -> f64 {
    value.map_or(0.0, |v| {
        let (v, min, max) = (f64::from(v), f64::from(min), f64::from(max));
        ((v - min) / (max - min)).clamp(0.0, 1.0)
    })
}

/// A closed polygon through `points`, or an empty path when there are none.
fn polygon(points: impl IntoIterator<Item = Point>) -> BezPath {
    let mut path = BezPath::new();
    for (index, point) in points.into_iter().enumerate() {
        if index == 0 {
            path.move_to(point);
        } else {
            path.line_to(point);
        }
    }
    if !path.elements().is_empty() {
        path.close_path();
    }
    path
}

impl Built {
    fn build(spec: &Spec, rows: &[CategoryRow], key: BuildKey) -> Self {
        let BuildKey { center, radius, .. } = key;
        let axes = rows.len();
        let mut built = Self {
            key,
            axes,
            rings: Vec::new(),
            spokes: BezPath::new(),
            datasets: Vec::new(),
        };
        if !(radius.is_finite() && radius > 0.0) {
            return built;
        }
        let at = |index: usize, r: f64| point_on_circle(center, r, axis_angle(index, axes));
        let polygonal = spec.shape == RadarShape::Polygon && axes >= 3;
        for ring in 1..=spec.rings {
            let r = radius * ring as f64 / spec.rings as f64;
            let path = if polygonal {
                polygon((0..axes).map(|i| at(i, r)))
            } else {
                let mut circle = arc_path(center, r, FIRST_AXIS, TAU);
                circle.close_path();
                circle
            };
            built.rings.push(path);
        }
        for index in 0..axes {
            built.spokes.move_to(center);
            built.spokes.line_to(at(index, radius));
        }
        if axes < 3 {
            return built;
        }
        let largest = rows
            .iter()
            .flat_map(|row| row.values.iter().flatten().copied())
            .reduce(f32::max);
        let domain = spec.domain.resolve(largest);
        let columns = rows.iter().map(|row| row.values.len()).max().unwrap_or(0);
        built.datasets = (0..columns)
            .map(|column| {
                polygon(rows.iter().enumerate().map(|(index, row)| {
                    let value = row.values.get(column).copied().flatten();
                    at(index, radius * reach(value, domain))
                }))
            })
            .collect();
        built
    }
}

/// The offset from a title's anchor to its top-left, for an axis at `angle`: a title right of
/// the centre starts at the anchor, one left of it ends there, and one straight above or below
/// is centred on it, sitting above or hanging below. A side title is centred vertically.
fn title_offset(angle: f64, size: Size) -> Vec2 {
    let (cos, sin) = (angle.cos(), angle.sin());
    let x = if cos > ALIGN_EPS {
        0.0
    } else if cos < -ALIGN_EPS {
        -size.width
    } else {
        -size.width / 2.0
    };
    let y = if cos.abs() > ALIGN_EPS {
        -size.height / 2.0
    } else if sin < 0.0 {
        -size.height
    } else {
        0.0
    };
    Vec2::new(x, y)
}

/// The largest radius up to `base` at which every title (`(angle, size)`, anchored
/// [`TITLE_GAP`] beyond the outer ring) lies inside `bounds`, or 0 when none does. A title
/// edge that does not move as the radius shrinks sets no limit.
fn fit_radius(bounds: Size, base: f64, titles: &[(f64, Size)]) -> f64 {
    let center = Point::new(bounds.width / 2.0, bounds.height / 2.0);
    let mut radius = base;
    // An edge at `start + reach * slope` must stay at or below `limit`, `reach` being the
    // anchor's distance from the centre.
    let mut cap = |slope: f64, start: f64, limit: f64| {
        if slope > ALIGN_EPS {
            radius = radius.min((limit - start) / slope - TITLE_GAP);
        }
    };
    for &(angle, size) in titles {
        let offset = title_offset(angle, size);
        let (cos, sin) = (angle.cos(), angle.sin());
        cap(cos, center.x + offset.x + size.width, bounds.width);
        cap(-cos, -(center.x + offset.x), 0.0);
        cap(sin, center.y + offset.y + size.height, bounds.height);
        cap(-sin, -(center.y + offset.y), 0.0);
    }
    if radius.is_finite() {
        radius.max(0.0)
    } else {
        0.0
    }
}

/// The style titles are shaped at: the theme's `body_small` in `on_surface_variant`, else the
/// fallback size and colour.
fn title_style(theme: Option<&Theme>) -> TextStyle {
    match theme {
        Some(theme) => {
            let mut style = theme.type_scale.body_small.clone();
            if !(style.size.is_finite() && style.size > 0.0) {
                style.size = DEFAULT_TITLE_SIZE;
            }
            style.color = theme.scheme().on_surface_variant;
            style
        }
        None => TextStyle::new(DEFAULT_TITLE_SIZE, FALLBACK_TITLE),
    }
}

/// The side of the square the chart asks for: `fixed` when given, else the smaller bounded
/// side of `max`, else [`DEFAULT_SIZE`].
fn side(fixed: Option<f32>, max: Size) -> f64 {
    match fixed.map(f64::from) {
        Some(side) if side.is_finite() => side.max(0.0),
        _ => match (max.width.is_finite(), max.height.is_finite()) {
            (true, true) => max.width.min(max.height),
            (true, false) => max.width,
            (false, true) => max.height,
            (false, false) => DEFAULT_SIZE,
        },
    }
}

/// The retained widget behind a [`RadarChartView`].
pub struct RadarChartWidget {
    spec: Spec,
    /// One shaped title per axis, placed by the last layout.
    titles: Vec<Label>,
    /// The label revision the titles were laid out for; `None` without titles.
    title_revision: Option<u64>,
    /// The label revision a paint asked a layout for, so it asks at most once per change.
    requested: Option<u64>,
    /// The outer ring's radius, set by layout.
    radius: f64,
    built: Option<Built>,
}

impl RadarChartWidget {
    /// Shape a title per row (reusing one whose text is unchanged), shrink the web until they
    /// all fit, and place each outside the web at its axis.
    fn layout_titles(&mut self, ctx: &mut LayoutCtx, size: Size, base: f64) {
        let categories = &self.spec.categories;
        let revision = categories.label_revision();
        let texts: Vec<String> =
            categories.with_rows(|rows| rows.iter().map(|row| row.label.clone()).collect());
        let style = title_style(Theme::from_layout_ctx(ctx));
        let axes = texts.len();
        let mut old = std::mem::take(&mut self.titles);
        let mut fits = Vec::with_capacity(axes);
        for (index, text) in texts.into_iter().enumerate() {
            let mut label = match old.iter().position(|label| label.text == text) {
                Some(i) => old.swap_remove(i),
                None => Label::new(text),
            };
            fits.push((axis_angle(index, axes), label.shape(ctx, &style)));
            self.titles.push(label);
        }
        let radius = fit_radius(size, base, &fits);
        let center = Point::new(size.width / 2.0, size.height / 2.0);
        for (label, &(angle, shaped)) in self.titles.iter_mut().zip(&fits) {
            let anchor = point_on_circle(center, radius + TITLE_GAP, angle);
            let top_left = anchor + title_offset(angle, shaped);
            let x = top_left.x.min(size.width - shaped.width).max(0.0);
            let y = top_left.y.min(size.height - shaped.height).max(0.0);
            label.at = Point::new(snap(x), snap(y));
        }
        self.radius = radius;
        self.title_revision = Some(revision);
        self.requested = None;
    }
}

impl Widget for RadarChartWidget {
    fn layout(&mut self, ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        let side = side(self.spec.size, bc.max());
        let size = bc.constrain(Size::new(side, side));
        let base = (size.width.min(size.height) / 2.0 - EDGE_INSET).max(0.0);
        if self.spec.titles {
            self.layout_titles(ctx, size, base);
        } else {
            self.titles.clear();
            self.title_revision = None;
            self.requested = None;
            self.radius = base;
        }
        size
    }

    fn paint(&mut self, ctx: &mut PaintCtx, scene: &mut dyn PaintScene) {
        let (origin, size) = (ctx.origin(), ctx.size());
        if !(size.width > 0.0 && size.height > 0.0) {
            return;
        }
        let spec = &self.spec;
        if spec.titles {
            let revision = spec.categories.label_revision();
            if self.title_revision != Some(revision) && self.requested != Some(revision) {
                // The titles are shaped for other labels: ask once for a layout to reshape.
                ctx.request_layout();
                self.requested = Some(revision);
            }
        }
        let key = BuildKey {
            revision: spec.categories.revision(),
            center: Point::new(size.width / 2.0, size.height / 2.0),
            radius: self.radius,
        };
        if self.built.as_ref().map(|built| built.key) != Some(key) {
            let built = spec
                .categories
                .with_rows(|rows| Built::build(spec, rows, key));
            self.built = Some(built);
        }
        let Some(built) = &self.built else { return };
        let theme = Theme::from_paint_ctx(ctx).map(Theme::scheme);
        let (series, web) = theme.map_or((Tone::FALLBACK_CALM, FALLBACK_WEB), |scheme| {
            (scheme.primary, scheme.outline_variant)
        });

        scene.push_clip(origin, size);
        let (web_color, web_width) = spec.web;
        let web_width = f64::from(web_width);
        if web_width > 0.0 {
            let brush = Brush::Solid(web_color.unwrap_or(web));
            for ring in &built.rings {
                scene.stroke_path(origin, ring, web_width, &brush);
            }
            if !built.spokes.elements().is_empty() {
                scene.stroke_path(origin, &built.spokes, web_width, &brush);
            }
        }
        for (column, path) in built.datasets.iter().enumerate() {
            let style = spec.styles.get(column).copied().unwrap_or_default();
            let color = style.color.unwrap_or(series);
            if style.fill_alpha > 0.0 {
                let fill = Brush::Solid(color.with_alpha(style.fill_alpha));
                scene.fill_path(origin, path, &fill);
            }
            if style.stroke_width > 0.0 {
                let width = f64::from(style.stroke_width);
                scene.stroke_path(origin, path, width, &Brush::Solid(color));
            }
        }
        if spec.titles && self.titles.len() == built.axes {
            for title in &self.titles {
                title.paint(origin, scene);
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
    use super::{Size, TITLE_GAP, fit_radius};
    use std::f64::consts::FRAC_PI_2;

    #[test]
    fn titles_shrink_the_radius_only_on_the_edges_they_move_towards() {
        let bounds = Size::new(200.0, 200.0);
        let wide = Size::new(40.0, 10.0);
        // Right of centre: the right edge sits at 100 + reach + 40, at most 200.
        let right = fit_radius(bounds, 98.0, &[(0.0, wide)]);
        assert!((right - (60.0 - TITLE_GAP)).abs() < 1e-9, "{right}");
        // Straight up: centred, so only the top edge limits it: 100 - reach - 10 >= 0.
        let up = fit_radius(bounds, 98.0, &[(-FRAC_PI_2, wide)]);
        assert!((up - (90.0 - TITLE_GAP)).abs() < 1e-9, "{up}");
        // A title wider than the widget straight up still leaves the radius its vertical room.
        let huge = fit_radius(bounds, 98.0, &[(-FRAC_PI_2, Size::new(400.0, 10.0))]);
        assert!((huge - up).abs() < 1e-9);
        assert_eq!(fit_radius(bounds, 98.0, &[]), 98.0);
        assert_eq!(fit_radius(Size::new(10.0, 10.0), 3.0, &[(0.0, wide)]), 0.0);
    }
}
