//! The `pie_chart` widget: a pie or donut with gaps between sections, rounded corners and a
//! centre slot.
//!
//! Each row of a [`Categories`] list is one section, sized by its first value; a gap, zero,
//! negative or non-finite value draws no section. The sections run from the start angle in
//! row order. One positive value draws a whole ring with no seam, and no positive value at all
//! draws an empty ring in the track colour.
//!
//! The widget reads the list while painting, so a write shows with no rebuild: the write
//! requests a repaint and the next paint draws the new sections. Built paths are cached and
//! rebuilt only when the list's revision, the widget's size or the ring options changed.
//!
//! A gap between sections is constant along the radius: each section's straight edges are
//! offset from the radial lines by half the gap, so the gap is as wide at the hole as at the
//! rim. Where the offset edges of a section meet before reaching the hole (or the centre of a
//! pie) the section ends in a point there; a section too thin to clear the gaps at the rim
//! draws nothing. The scene has no blend modes, so gaps and the hole are geometry, never an
//! erasing paint.
//!
//! Colours: the caller's list, cycled by row index (so a row keeps its colour when another
//! row drops out); else the installed theme's `primary`, `tertiary`, `secondary`, `error`,
//! `primary_fixed_dim` and `tertiary_fixed_dim`; else a fixed palette that starts with
//! [`Tone::FALLBACK_CALM`]. The empty ring takes the caller's track colour, else the theme's
//! `surface_container_highest`, else [`MeterView::FALLBACK_TRACK`]. The chart never animates
//! and requests no frames.
//!
//! The optional centre child is laid out with loose constraints up to the square inscribed in
//! the donut hole, centred in it (in a pie, which has no hole, that square is empty). It is
//! painted over the ring and receives events normally; the ring itself takes no input, so a
//! press outside the child is ignored and reaches whatever lies beneath.

use std::f64::consts::{FRAC_PI_2, PI, SQRT_2, TAU};

use frust::authoring::{
    AnyView, BezPath, BoxConstraints, Brush, BuildCtx, ChangeFlags, ChildPod, EventCtx,
    EventResult, InputEvent, LayoutCtx, PaintCtx, PaintScene, Point, Role, SemanticsCtx, Size,
    View, Widget, any, build_child, rebuild_child, route_event_single, teardown_child,
};
use frust::{Color, Theme};

use crate::geometry::snap;
use crate::polar::{angle_of, arc_path, point_on_circle, sector_path};
use crate::{Categories, MeterView, Tone};

/// The diameter when neither the caller nor the constraints bound it.
const DEFAULT_DIAMETER: f64 = 160.0;
/// 12 o'clock in the polar convention (0 at 3 o'clock, positive clockwise).
const DEFAULT_START_ANGLE: f64 = -FRAC_PI_2;
/// The section colours when neither the caller nor the theme gives any.
const FALLBACK_PALETTE: [Color; 6] = [
    Tone::FALLBACK_CALM,
    Color::from_rgb8(0x2f, 0x6f, 0xd6),
    Tone::FALLBACK_BUSY,
    Color::from_rgb8(0x8e, 0x4e, 0xc6),
    Tone::FALLBACK_CRITICAL,
    Color::from_rgb8(0x1f, 0x9e, 0xa8),
];

#[derive(Clone, Debug)]
struct PieSpec {
    categories: Categories,
    colors: Vec<Color>,
    track: Option<Color>,
    inner_fraction: f32,
    gap: f32,
    corner_radius: f32,
    /// Kept in `f64` so the default lands exactly on 12 o'clock.
    start_angle: f64,
    clockwise: bool,
    size: Option<f32>,
    label: Option<String>,
}

fn same_f32(a: f32, b: f32) -> bool {
    a.to_bits() == b.to_bits()
}

impl PieSpec {
    /// What a widget holding `old` must redo to show `self`.
    fn changes(&self, old: &Self) -> ChangeFlags {
        if self.size.map(f32::to_bits) != old.size.map(f32::to_bits)
            || !same_f32(self.inner_fraction, old.inner_fraction)
        {
            return ChangeFlags::LAYOUT | ChangeFlags::PAINT;
        }
        if !self.categories.ptr_eq(&old.categories)
            || self.colors != old.colors
            || self.track != old.track
            || !same_f32(self.gap, old.gap)
            || !same_f32(self.corner_radius, old.corner_radius)
            || self.start_angle.to_bits() != old.start_angle.to_bits()
            || self.clockwise != old.clockwise
            || self.label != old.label
        {
            return ChangeFlags::PAINT;
        }
        ChangeFlags::NONE
    }

    /// The ring options made finite and in range.
    fn style(&self) -> RingStyle {
        let inner_fraction = if self.inner_fraction.is_finite() {
            f64::from(self.inner_fraction).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let positive = |v: f32| {
            if v.is_finite() && v > 0.0 {
                f64::from(v)
            } else {
                0.0
            }
        };
        let start = if self.start_angle.is_finite() {
            self.start_angle
        } else {
            DEFAULT_START_ANGLE
        };
        RingStyle {
            inner_fraction,
            half_gap: positive(self.gap) / 2.0,
            corner: positive(self.corner_radius),
            start,
            direction: if self.clockwise { 1.0 } else { -1.0 },
        }
    }
}

/// The resolved ring options a built path depends on.
#[derive(Clone, Copy, Debug, PartialEq)]
struct RingStyle {
    inner_fraction: f64,
    half_gap: f64,
    corner: f64,
    start: f64,
    direction: f64,
}

/// The ring's circles in widget-local coordinates.
#[derive(Clone, Copy, Debug)]
struct Disc {
    center: Point,
    inner: f64,
    outer: f64,
}

impl Disc {
    /// The largest circle centred in `size`, with a hole of `inner_fraction` of its radius.
    fn of(size: Size, inner_fraction: f64) -> Self {
        let outer = (size.width.min(size.height) / 2.0).max(0.0);
        Self {
            center: Point::new(size.width / 2.0, size.height / 2.0),
            inner: outer * inner_fraction,
            outer,
        }
    }
}

/// A pie or donut chart over a [`Categories`] list; build one with [`pie_chart`].
pub struct PieChartView<S: 'static> {
    spec: PieSpec,
    center: Option<AnyView<S>>,
}

/// A pie chart with one section per row of `categories`, sized by the row's first value,
/// starting at 12 o'clock and running clockwise; its diameter is the smaller side of the space
/// it is given.
pub fn pie_chart<S: 'static>(categories: Categories) -> PieChartView<S> {
    PieChartView {
        spec: PieSpec {
            categories,
            colors: Vec::new(),
            track: None,
            inner_fraction: 0.0,
            gap: 0.0,
            corner_radius: 0.0,
            start_angle: DEFAULT_START_ANGLE,
            clockwise: true,
            size: None,
            label: None,
        },
        center: None,
    }
}

impl<S: 'static> PieChartView<S> {
    /// The section colours, cycled by row index (default: the theme's palette). An empty list
    /// restores the default.
    pub fn colors(mut self, colors: impl IntoIterator<Item = Color>) -> Self {
        self.spec.colors = colors.into_iter().collect();
        self
    }

    /// The colour of the empty ring drawn when no row has a positive value (default: the
    /// theme's `surface_container_highest`).
    pub fn track(mut self, color: Color) -> Self {
        self.spec.track = Some(color);
        self
    }

    /// Make a donut: the hole's radius as a fraction of the outer radius, clamped to `0..=1`
    /// (default 0, a pie).
    pub fn donut(mut self, inner_fraction: f32) -> Self {
        self.spec.inner_fraction = inner_fraction;
        self
    }

    /// The space between neighbouring sections in logical px, the same width from the hole to
    /// the rim (default 0).
    pub fn gap(mut self, gap: f32) -> Self {
        self.spec.gap = gap;
        self
    }

    /// The radius of each section's corners in logical px (default 0), shrunk to fit a thin
    /// or narrow section. A pie section's point at the centre stays sharp.
    pub fn corner_radius(mut self, radius: f32) -> Self {
        self.spec.corner_radius = radius;
        self
    }

    /// Where the first section starts, in radians: 0 at 3 o'clock, growing clockwise (default
    /// `-PI / 2`, 12 o'clock).
    pub fn start_angle(mut self, radians: f32) -> Self {
        self.spec.start_angle = f64::from(radians);
        self
    }

    /// Whether the sections run clockwise from the start angle (default `true`).
    pub fn clockwise(mut self, clockwise: bool) -> Self {
        self.spec.clockwise = clockwise;
        self
    }

    /// A child laid out in the donut hole, centred, within the square inscribed in the hole.
    pub fn center(mut self, child: impl View<S>) -> Self {
        self.center = Some(any(child));
        self
    }

    /// A fixed diameter in logical px (default: the smaller side of the space given).
    pub fn size(mut self, diameter: f32) -> Self {
        self.spec.size = Some(diameter);
        self
    }

    /// Give the chart an image node with this label; without one the chart adds no node of
    /// its own (a centre child's nodes are still reported).
    pub fn semantics(mut self, label: impl Into<String>) -> Self {
        self.spec.label = Some(label.into());
        self
    }
}

impl<S: 'static> View<S> for PieChartView<S> {
    type Element = PieChartWidget;

    fn build(&self, ctx: &mut BuildCtx<'_>) -> PieChartWidget {
        PieChartWidget {
            spec: self.spec.clone(),
            center: self.center.as_ref().map(|child| build_child(child, ctx)),
            cache: None,
        }
    }

    fn rebuild(
        &self,
        prev: &Self,
        element: &mut PieChartWidget,
        ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        let mut flags = self.spec.changes(&element.spec);
        if !flags.is_empty() {
            element.spec = self.spec.clone();
            element.cache = None;
        }
        match (&prev.center, &self.center, &mut element.center) {
            (Some(prev), Some(next), Some(pod)) => flags |= rebuild_child(prev, next, pod, ctx),
            (Some(prev), None, slot @ Some(_)) => {
                if let Some(pod) = slot {
                    teardown_child(prev, pod, ctx);
                }
                *slot = None;
                flags |= ChangeFlags::LAYOUT | ChangeFlags::PAINT;
            }
            (_, Some(next), slot @ None) => {
                *slot = Some(build_child(next, ctx));
                flags |= ChangeFlags::LAYOUT | ChangeFlags::PAINT;
            }
            _ => {}
        }
        flags
    }

    fn teardown(&self, element: &mut PieChartWidget, ctx: &mut BuildCtx<'_>) {
        if let (Some(child), Some(pod)) = (&self.center, &mut element.center) {
            teardown_child(child, pod, ctx);
        }
    }
}

/// The inputs a built ring was made from.
#[derive(Clone, Copy, Debug, PartialEq)]
struct CacheKey {
    revision: u64,
    size: Size,
    style: RingStyle,
}

/// The ring's paths, each with the row it draws.
enum Ring {
    /// No row has a positive value: one whole ring in the track colour.
    Track(BezPath),
    /// One path per drawn section, with its row index.
    Sections(Vec<(usize, BezPath)>),
}

struct Built {
    key: CacheKey,
    ring: Ring,
}

/// The retained widget behind a [`PieChartView`].
pub struct PieChartWidget {
    spec: PieSpec,
    center: Option<ChildPod>,
    cache: Option<Built>,
}

impl PieChartWidget {
    fn section_color(&self, row: usize, theme: Option<&Theme>) -> Color {
        if !self.spec.colors.is_empty() {
            return self.spec.colors[row % self.spec.colors.len()];
        }
        match theme {
            Some(theme) => {
                let s = theme.scheme();
                let palette = [
                    s.primary,
                    s.tertiary,
                    s.secondary,
                    s.error,
                    s.primary_fixed_dim,
                    s.tertiary_fixed_dim,
                ];
                palette[row % palette.len()]
            }
            None => FALLBACK_PALETTE[row % FALLBACK_PALETTE.len()],
        }
    }
}

impl Widget for PieChartWidget {
    fn layout(&mut self, ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        let max = bc.max();
        let finite = |v: f64| v.is_finite().then_some(v);
        let available = match (finite(max.width), finite(max.height)) {
            (Some(w), Some(h)) => w.min(h),
            (Some(side), None) | (None, Some(side)) => side,
            (None, None) => DEFAULT_DIAMETER,
        };
        let diameter = match self.spec.size.map(f64::from) {
            Some(d) if d.is_finite() => d.max(0.0),
            _ => available,
        };
        let size = bc.constrain(Size::new(diameter, diameter));
        if let Some(pod) = &mut self.center {
            let disc = Disc::of(size, self.spec.style().inner_fraction);
            let side = disc.inner * SQRT_2;
            let child = pod.layout_child(ctx, &BoxConstraints::loose(Size::new(side, side)));
            pod.set_origin(Point::new(
                snap(disc.center.x - child.width / 2.0),
                snap(disc.center.y - child.height / 2.0),
            ));
        }
        size
    }

    fn paint(&mut self, ctx: &mut PaintCtx, scene: &mut dyn PaintScene) {
        let (origin, size) = (ctx.origin(), ctx.size());
        if size.width > 0.0 && size.height > 0.0 {
            let key = CacheKey {
                revision: self.spec.categories.revision(),
                size,
                style: self.spec.style(),
            };
            if self.cache.as_ref().is_none_or(|built| built.key != key) {
                let ring = build_ring(&self.spec.categories, size, key.style);
                self.cache = Some(Built { key, ring });
            }
            let theme = Theme::from_paint_ctx(ctx);
            if let Some(built) = &self.cache {
                match &built.ring {
                    Ring::Track(path) => {
                        let track = self.spec.track.unwrap_or_else(|| {
                            theme.map_or(MeterView::FALLBACK_TRACK, |t| {
                                t.scheme().surface_container_highest
                            })
                        });
                        scene.fill_path(origin, path, &Brush::Solid(track));
                    }
                    Ring::Sections(sections) => {
                        for (row, path) in sections {
                            let color = self.section_color(*row, theme);
                            scene.fill_path(origin, path, &Brush::Solid(color));
                        }
                    }
                }
            }
        }
        if let Some(pod) = &mut self.center {
            pod.paint_child(ctx, scene);
        }
    }

    fn event(&mut self, ctx: &mut EventCtx, event: &InputEvent) -> EventResult {
        // The child sits inside the hole, so only the child's own bounds take input.
        match &mut self.center {
            Some(pod) => route_event_single(pod, ctx, event),
            None => EventResult::Ignored,
        }
    }

    fn semantics(&self, ctx: &mut SemanticsCtx) {
        match (&self.spec.label, &self.center) {
            (Some(label), Some(pod)) => {
                ctx.push_container(
                    Role::Image,
                    |node| node.set_label(label.as_str()),
                    |ctx| pod.semantics_child(ctx),
                );
            }
            (Some(label), None) => {
                ctx.push_node(Role::Image, |node| node.set_label(label.as_str()));
            }
            (None, Some(pod)) => pod.semantics_child(ctx),
            (None, None) => {}
        }
    }

    frust::authoring::visit_children!(center);
}

/// A row's share: its first value when positive and finite.
fn share(values: &[Option<f32>]) -> Option<f64> {
    values
        .first()
        .copied()
        .flatten()
        .filter(|v| v.is_finite() && *v > 0.0)
        .map(f64::from)
}

/// The whole ring of `disc`, with no seam.
fn full_ring(disc: Disc) -> BezPath {
    sector_path(disc.center, disc.inner, disc.outer, 0.0, TAU, 0.0)
}

fn build_ring(categories: &Categories, size: Size, style: RingStyle) -> Ring {
    let disc = Disc::of(size, style.inner_fraction);
    let shares: Vec<(usize, f64)> = categories.with_rows(|rows| {
        rows.iter()
            .enumerate()
            .filter_map(|(row, r)| share(&r.values).map(|v| (row, v)))
            .collect()
    });
    let total: f64 = shares.iter().map(|(_, v)| v).sum();
    if shares.is_empty() || !(total.is_finite() && total > 0.0) {
        return Ring::Track(full_ring(disc));
    }
    if let [(row, _)] = shares[..] {
        return Ring::Sections(vec![(row, full_ring(disc))]);
    }
    let mut sections = Vec::with_capacity(shares.len());
    let mut before = 0.0;
    for (row, value) in shares {
        let from = TAU * before / total;
        before += value;
        let to = TAU * before / total;
        let start = style.start + style.direction * from;
        let path = section_path(disc, start, to - from, style);
        if !path.is_empty() {
            sections.push((row, path));
        }
    }
    Ring::Sections(sections)
}

/// One section from `start` through `sweep` (unsigned, run in `style.direction`).
fn section_path(disc: Disc, start: f64, sweep: f64, style: RingStyle) -> BezPath {
    if style.half_gap <= 0.0 {
        let signed = style.direction * sweep;
        return sector_path(
            disc.center,
            disc.inner,
            disc.outer,
            start,
            signed,
            style.corner,
        );
    }
    gapped_sector(
        disc,
        start,
        style.direction,
        sweep,
        style.half_gap,
        style.corner,
    )
}

/// Where a gapped section comes nearest the centre.
#[derive(Clone, Copy, Debug)]
enum Near {
    /// Along the hole's circle, between the two offset edges.
    Arc,
    /// At the point, this far from the centre on the bisector, where the offset edges meet.
    Apex(f64),
    /// Around the circle of the half gap's radius, between the two edges' feet (a section of
    /// a half turn or more whose edges meet no hole).
    Foot,
}

/// A section whose straight edges are offset `half_gap` from its radial edges, its corners
/// rounded by up to `corner`, as a closed path. Empty when the section does not clear the
/// gaps at the rim.
///
/// A point is placed by its distance from the centre and its angular offset from the
/// section's start, measured towards its end; the end edge mirrors the start edge.
fn gapped_sector(
    disc: Disc,
    start: f64,
    direction: f64,
    sweep: f64,
    half_gap: f64,
    corner: f64,
) -> BezPath {
    let Disc {
        center,
        inner,
        outer,
    } = disc;
    let h = half_gap;
    let mut path = BezPath::new();
    if !(sweep.is_finite() && sweep > 0.0 && outer > h) {
        return path;
    }
    let rim = (h / outer).asin();
    if sweep <= 2.0 * rim {
        return path;
    }
    let half = sweep / 2.0;
    let near = if inner > h && (h / inner).asin() < half {
        Near::Arc
    } else if sweep < PI {
        Near::Apex(h / half.sin())
    } else {
        Near::Foot
    };
    let reach = match near {
        Near::Arc => inner,
        Near::Apex(distance) => distance,
        Near::Foot => h,
    };
    if reach >= outer {
        return path;
    }

    // Corner limits: a fillet fits between an edge and the opposite edge's mirror when its
    // centre stays within half the sweep (`k` is that half-angle's sine, capped at a quarter
    // turn), and the two fillets on one edge do not cross.
    let k = half.min(FRAC_PI_2).sin();
    let outer_corner = corner
        .min((outer - reach) / 2.0)
        .min((k * outer - h) / (1.0 + k))
        .max(0.0);
    let inner_corner = match near {
        Near::Arc if k < 1.0 => outer_corner.min((k * inner - h) / (1.0 - k)).max(0.0),
        Near::Arc => outer_corner,
        _ => 0.0,
    };

    let at = |radius: f64, offset: f64| point_on_circle(center, radius, start + direction * offset);
    // The point `u` along an offset edge from the foot of the centre's perpendicular.
    let start_edge = |u: f64| at(u.hypot(h), h.atan2(u));
    let end_edge = |u: f64| at(u.hypot(h), sweep - h.atan2(u));

    // The inner corner's fillet: its offset from each edge and how far along the edge it
    // meets it.
    let inner_fillet = (inner_corner > 0.0).then(|| {
        let offset = ((h + inner_corner) / (inner + inner_corner)).asin();
        (offset, (inner + inner_corner) * offset.cos())
    });

    let first = match (near, inner_fillet) {
        (Near::Arc, Some((_, along))) => start_edge(along),
        (Near::Arc, None) => at(inner, (h / inner).asin()),
        (Near::Apex(distance), _) => at(distance, half),
        (Near::Foot, _) => at(h, FRAC_PI_2),
    };
    path.move_to(first);

    if outer_corner > 0.0 {
        let offset = ((h + outer_corner) / (outer - outer_corner)).asin();
        let along = (outer - outer_corner) * offset.cos();
        path.line_to(start_edge(along));
        let fillet = at(outer - outer_corner, offset);
        fillet_to(
            &mut path,
            fillet,
            outer_corner,
            start_edge(along),
            at(outer, offset),
        );
        append_arc(
            &mut path,
            center,
            outer,
            start + direction * offset,
            direction * (sweep - 2.0 * offset),
        );
        let fillet = at(outer - outer_corner, sweep - offset);
        fillet_to(
            &mut path,
            fillet,
            outer_corner,
            at(outer, sweep - offset),
            end_edge(along),
        );
    } else {
        path.line_to(at(outer, rim));
        append_arc(
            &mut path,
            center,
            outer,
            start + direction * rim,
            direction * (sweep - 2.0 * rim),
        );
    }

    match (near, inner_fillet) {
        (Near::Arc, Some((offset, along))) => {
            path.line_to(end_edge(along));
            let fillet = at(inner + inner_corner, sweep - offset);
            fillet_to(
                &mut path,
                fillet,
                inner_corner,
                end_edge(along),
                at(inner, sweep - offset),
            );
            append_arc(
                &mut path,
                center,
                inner,
                start + direction * (sweep - offset),
                -direction * (sweep - 2.0 * offset),
            );
            let fillet = at(inner + inner_corner, offset);
            fillet_to(
                &mut path,
                fillet,
                inner_corner,
                at(inner, offset),
                start_edge(along),
            );
        }
        (Near::Arc, None) => {
            let offset = (h / inner).asin();
            path.line_to(at(inner, sweep - offset));
            append_arc(
                &mut path,
                center,
                inner,
                start + direction * (sweep - offset),
                -direction * (sweep - 2.0 * offset),
            );
        }
        (Near::Apex(_), _) => {}
        (Near::Foot, _) => {
            path.line_to(at(h, sweep - FRAC_PI_2));
            append_arc(
                &mut path,
                center,
                h,
                start + direction * (sweep - FRAC_PI_2),
                -direction * (sweep - PI),
            );
        }
    }
    path.close_path();
    path
}

/// Append the arc of `radius` around `center` from `start_angle` through `sweep`, assuming the
/// path's current point is the arc's start; a zero sweep appends nothing.
fn append_arc(path: &mut BezPath, center: Point, radius: f64, start_angle: f64, sweep: f64) {
    let arc = arc_path(center, radius, start_angle, sweep);
    path.extend(arc.elements().iter().skip(1).copied());
}

/// Append the short arc of the circle of `radius` around `center` from `from` (the current
/// point) to `to`, both on that circle.
fn fillet_to(path: &mut BezPath, center: Point, radius: f64, from: Point, to: Point) {
    let start = angle_of(from, center);
    let mut sweep = angle_of(to, center) - start;
    if sweep > PI {
        sweep -= TAU;
    } else if sweep < -PI {
        sweep += TAU;
    }
    if sweep == 0.0 {
        path.line_to(to);
    } else {
        append_arc(path, center, radius, start, sweep);
    }
}

#[cfg(test)]
mod tests {
    use super::{Disc, gapped_sector};
    use frust::authoring::Point;
    use std::f64::consts::{FRAC_PI_2, PI};

    fn disc(inner: f64) -> Disc {
        Disc {
            center: Point::new(100.0, 100.0),
            inner,
            outer: 100.0,
        }
    }

    #[test]
    fn a_section_too_thin_for_its_gaps_is_empty() {
        // At the rim a half gap of 5 takes asin(0.05) on each side.
        let rim = (0.05f64).asin();
        assert!(gapped_sector(disc(0.0), 0.0, 1.0, 2.0 * rim, 5.0, 0.0).is_empty());
        assert!(!gapped_sector(disc(0.0), 0.0, 1.0, 2.0 * rim + 0.01, 5.0, 0.0).is_empty());
        assert!(gapped_sector(disc(50.0), 0.0, 1.0, FRAC_PI_2, 200.0, 0.0).is_empty());
    }

    #[test]
    fn every_shape_closes_inside_the_disc() {
        for inner in [0.0, 3.0, 40.0] {
            for sweep in [0.3, FRAC_PI_2, PI, 1.5 * PI] {
                for corner in [0.0, 6.0, 80.0] {
                    for direction in [1.0, -1.0] {
                        let path = gapped_sector(disc(inner), 0.4, direction, sweep, 4.0, corner);
                        assert!(!path.is_empty());
                        for el in path.elements() {
                            if let Some(p) = el.end_point() {
                                let d = p.distance(Point::new(100.0, 100.0));
                                assert!(d <= 100.0 + 1e-6, "{inner} {sweep} {corner}: {d}");
                            }
                        }
                    }
                }
            }
        }
    }
}
